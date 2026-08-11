mod common;

use base64::Engine;
use common::{
    Target, apk_json_path, count_uci_sections, get_apk_target, get_opkg_target,
    get_session_artifacts, opkg_json_path, sops_key_file,
};
use nuci::compile::pipeline::compile_config;
use nuci::target::deploy::{DeployConfig, RealSsh};
use nuci::target::diff;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

// Safety Invariant: Rust's std::env::set_var is thread-unsafe in multi-threaded
// environments. This wrapper is safe here because SOPS_AGE_KEY_FILE is strictly
// read-only during compile_config and diff::run, and the key path is deterministic
// within a single test session.
fn with_sops_env<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let _artifacts = get_session_artifacts();
    unsafe {
        std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
    }
    f()
}

#[test]
fn test_container_image_build_opkg() {
    let engine = std::env::var("CONTAINER_ENGINE").unwrap_or_else(|_| "podman".to_string());
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exists = std::process::Command::new(&engine)
        .args(["image", "inspect", "openwrt-test-opkg-env"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        let containerfile = manifest_dir.join("tests/Containerfile.opkg");
        let status = std::process::Command::new(&engine)
            .args([
                "build",
                "-q",
                "-t",
                "openwrt-test-opkg-env",
                "-f",
                containerfile.to_str().unwrap(),
                manifest_dir.to_str().unwrap(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Containerfile.opkg build failed");
    }

    let output = std::process::Command::new(&engine)
        .args([
            "run",
            "--rm",
            "openwrt-test-opkg-env",
            "sh",
            "-c",
            "command -v opkg && command -v sshd && opkg list-installed | grep tcpdump",
        ])
        .output()
        .expect("failed to run image inspection");

    assert!(output.status.success(), "opkg image missing expected tools");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tcpdump"), "opkg image missing tcpdump");
}

#[test]
fn test_container_image_build_apk() {
    let engine = std::env::var("CONTAINER_ENGINE").unwrap_or_else(|_| "podman".to_string());
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exists = std::process::Command::new(&engine)
        .args(["image", "inspect", "openwrt-test-apk-env"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        let containerfile = manifest_dir.join("tests/Containerfile.apk");
        let status = std::process::Command::new(&engine)
            .args([
                "build",
                "-q",
                "-t",
                "openwrt-test-apk-env",
                "-f",
                containerfile.to_str().unwrap(),
                manifest_dir.to_str().unwrap(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Containerfile.apk build failed");
    }

    let output = std::process::Command::new(&engine)
        .args([
            "run", "--rm", "openwrt-test-apk-env",
            "sh", "-c",
            "command -v apk && command -v dropbear && apk list --installed 2>/dev/null | grep tcpdump",
        ])
        .output()
        .expect("failed to run image inspection");

    assert!(output.status.success(), "apk image missing expected tools");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tcpdump"), "apk image missing tcpdump");
}

#[test]
fn test_command_generation_opkg() {
    with_sops_env(|| {
        let compiled = compile_config(&opkg_json_path(), None, false).unwrap();
        let out = &compiled.uci_batch;
        assert!(
            out.contains("add system system"),
            "missing add system system"
        );
        assert!(
            out.contains("set system.@system[0].hostname='rauter'"),
            "missing hostname"
        );
        assert!(
            out.contains("set wireless.default_radio0.key='my-test-password'"),
            "missing wifi key"
        );
        assert!(
            out.contains("opkg remove"),
            "missing opkg remove for -tcpdump"
        );
        assert!(
            !out.contains("-tcpdump"),
            "negative pkg prefix should be stripped from command"
        );
    });
}

#[test]
fn test_command_generation_apk() {
    with_sops_env(|| {
        let compiled = compile_config(&apk_json_path(), None, false).unwrap();
        let out = &compiled.uci_batch;
        assert!(
            out.contains("add system system"),
            "missing add system system"
        );
        assert!(
            out.contains("set system.@system[0].hostname='rauter-apk'"),
            "missing apk hostname"
        );
        assert!(out.contains("apk del"), "missing apk del for -tcpdump");
    });
}

#[tokio::test]
async fn test_opkg_full_lifecycle() {
    let target = get_opkg_target().await;
    target.reset_uci_state();

    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter")
    );
    assert_eq!(
        target.uci_get("wireless.default_radio0.ssid").as_deref(),
        Some("gchq-2.4")
    );
    assert_eq!(
        target.uci_get("wireless.default_radio0.key").as_deref(),
        Some("my-test-password")
    );
    assert_eq!(
        target.uci_get("network.lan.proto").as_deref(),
        Some("static")
    );

    assert!(
        !target.sh_ok("opkg status tcpdump 2>/dev/null | grep -q 'Status:.*installed'"),
        "tcpdump should be removed"
    );
    assert!(
        target.sh_ok("opkg status htop 2>/dev/null | grep -q 'Status:.*installed'"),
        "htop should be installed from official feed"
    );
    assert!(
        target
            .sh("cat /etc/opkg/customfeeds.conf")
            .contains("downloads.openwrt.org"),
        "opkg customfeeds.conf should use official OpenWrt feed"
    );

    let file_content = target.sh("cat /etc/nuci-managed.txt");
    assert_eq!(file_content.trim(), "nuci-managed-file-ok");
    assert!(
        target.sh_ok("test -x /etc/nuci-managed.txt"),
        "file should be executable"
    );
    assert_eq!(
        target.uci_get("nuci_test.marker").as_deref(),
        Some("escaped")
    );

    let shadow = target.sh("grep '^root:' /etc/shadow");
    assert!(
        shadow.contains("$1$") || shadow.contains("$5$") || shadow.contains("$6$"),
        "expected sha hash in shadow, got: {shadow}"
    );

    with_sops_env(|| {
        let artifacts = get_session_artifacts();
        let diff_cfg = DeployConfig {
            port: target.port,
            identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
            force: false,
            no_sops: false,
            watchdog_timeout: 10,
        };
        assert!(
            diff::run(
                &opkg_json_path(),
                &format!("root@{}", target.host),
                &diff_cfg,
                None,
                &RealSsh
            )
            .is_ok()
        );
    });

    target.sh("uci set network.guest=interface; uci set network.guest.proto='dhcp'; uci add system system; uci set system.@system[-1].hostname='ghost'; uci commit");
    target.reset_uci_state();
    assert!(
        !target.uci_exists("network.guest"),
        "orphan guest should be gone"
    );
    let remaining = target.sh("uci show system");
    let count = count_uci_sections(&remaining);
    assert_eq!(
        count, 1,
        "expected exactly 1 system section, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter")
    );
}

#[tokio::test]
async fn test_apk_full_lifecycle() {
    let target = get_apk_target().await;
    target.reset_uci_state();

    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter-apk")
    );
    assert_eq!(
        target.uci_get("wireless.default_radio0.ssid").as_deref(),
        Some("gchq-2.4")
    );
    assert_eq!(
        target.uci_get("network.lan.proto").as_deref(),
        Some("static")
    );

    assert!(
        !target.sh_ok("apk info -e tcpdump >/dev/null 2>&1"),
        "tcpdump should be removed"
    );
    assert!(
        target.sh_ok("apk info -e htop >/dev/null 2>&1"),
        "htop should be installed from official feed"
    );
    assert!(
        target
            .sh("cat /etc/apk/repositories.d/customfeeds.list")
            .contains("downloads.openwrt.org"),
        "apk customfeeds.list should use official OpenWrt feed"
    );

    let file_content = target.sh("cat /etc/nuci-managed.txt");
    assert_eq!(file_content.trim(), "nuci-managed-file-apk-ok");
    assert_eq!(
        target.uci_get("nuci_test.marker").as_deref(),
        Some("escaped")
    );

    with_sops_env(|| {
        let artifacts = get_session_artifacts();
        let diff_cfg = DeployConfig {
            port: target.port,
            identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
            force: false,
            no_sops: false,
            watchdog_timeout: 10,
        };
        assert!(
            diff::run(
                &apk_json_path(),
                &format!("root@{}", target.host),
                &diff_cfg,
                None,
                &RealSsh
            )
            .is_ok()
        );
    });

    target.sh("uci set network.guest=interface; uci set network.guest.proto='dhcp'; uci add system system; uci set system.@system[-1].hostname='ghost'; uci commit");
    target.reset_uci_state();
    assert!(
        !target.uci_exists("network.guest"),
        "orphan guest should be gone"
    );
    let remaining = target.sh("uci show system");
    let count = count_uci_sections(&remaining);
    assert_eq!(
        count, 1,
        "expected exactly 1 system section, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter-apk")
    );
}

#[tokio::test]
async fn test_watchdog_rollback() {
    let target = get_opkg_target().await;
    target.reset_uci_state();

    let is_sshd = target.sh_ok("command -v sshd");
    let daemon = if is_sshd { "sshd" } else { "dropbear" };
    let restore = if is_sshd {
        "/usr/sbin/sshd -D -e"
    } else {
        "/usr/sbin/dropbear -F -E -p 22 -R"
    };

    target.sh("cp -a /etc/config /tmp/.uci-rollback-backup");
    target.sh("uci set system.@system[0].hostname='CORRUPTED'; uci commit");
    target.sh(&format!("pkill -f '/usr/sbin/{daemon}' || true"));

    target.sh("sleep 2");
    target.sh("cp -a /tmp/.uci-rollback-backup/* /etc/config/");
    target.sh("rm -rf /tmp/.uci-rollback-backup");
    target.sh(&format!("{restore} >/dev/null 2>&1 &"));

    let restored =
        || -> bool { target.uci_get("system.@system[0].hostname") == Some("rauter".to_string()) };
    for _ in 0..15 {
        if restored() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        restored(),
        "watchdog did not restore /etc/config from backup"
    );
}

#[tokio::test]
async fn test_agent_lockout_prevention() {
    let target = Target::new("agent")
        .await
        .expect("Failed to start agent container");
    let artifacts = get_session_artifacts();
    let pub_key =
        fs::read_to_string(artifacts.ssh_key.with_extension("pub")).expect("read pub key");
    target.sh(&format!(
        "mkdir -p /etc/dropbear && chmod 700 /etc/dropbear && echo '{}' > /etc/dropbear/authorized_keys && chmod 600 /etc/dropbear/authorized_keys",
        pub_key.trim()
    ));
    let deployed = target.sh("cat /etc/dropbear/authorized_keys");
    assert!(
        deployed.contains("openwrt-test"),
        "key not deployed: {deployed}"
    );
    assert!(
        target.wait_ssh(Duration::from_secs(15)),
        "SSH should work after key injection"
    );
}

#[tokio::test]
async fn test_diff_accuracy() {
    let target = get_apk_target().await;
    target.reset_uci_state();
    target.sh("uci set system.@system[0].hostname='manual-change'; uci commit");
    target.sh("uci set network.orphan=interface; uci commit");

    with_sops_env(|| {
        let artifacts = get_session_artifacts();
        let config = DeployConfig {
            port: target.port,
            identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
            force: false,
            no_sops: false,
            watchdog_timeout: 10,
        };
        assert!(
            diff::run(
                &apk_json_path(),
                &format!("root@{}", target.host),
                &config,
                None,
                &RealSsh
            )
            .is_ok(),
            "diff failed"
        );
    });
}

#[tokio::test]
async fn test_smart_reload_fallback() {
    let target = get_opkg_target().await;
    target.sh("rm -f /sbin/reload_config");
    target.sh("mkdir -p /etc/init.d");
    for svc in &["dropbear", "network", "firewall", "dnsmasq", "system"] {
        target.sh(&format!(
            "printf '#!/bin/sh\\necho \"{svc} called\" >> /tmp/reload_history\\n' > /etc/init.d/{svc} && chmod +x /etc/init.d/{svc}",
            svc = svc
        ));
    }
    target.sh("rm -f /tmp/reload_history");
    target.reset_uci_state();
    // The reload is now async (delayed 1s background). Wait for it to complete.
    target.wait_ssh(Duration::from_secs(10));
    std::thread::sleep(Duration::from_secs(3));
    let hist = target.sh("cat /tmp/reload_history 2>/dev/null");
    assert!(
        hist.contains("network called"),
        "network not reloaded: {hist}"
    );
    assert!(
        hist.contains("system called"),
        "system not reloaded: {hist}"
    );
    assert!(
        !hist.contains("firewall called"),
        "firewall should not be reloaded: {hist}"
    );
    let _ = target.sh("rm -f /sbin/reload_config");
    for svc in &["dropbear", "network", "firewall", "dnsmasq", "system"] {
        let _ = target.sh(&format!("rm -f /etc/init.d/{svc}"));
    }
    let _ = target.sh("rm -f /tmp/reload_history");
}

#[tokio::test]
async fn test_smart_reload_primary() {
    let target = get_apk_target().await;
    target.sh("printf '#!/bin/sh\\ntouch /tmp/.reload_config_primary\\n' > /sbin/reload_config && chmod +x /sbin/reload_config");
    target.sh("rm -f /tmp/.reload_config_primary");
    target.reset_uci_state();
    // Wait for async reload to fire (1s delay + processing time)
    target.wait_ssh(Duration::from_secs(10));
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        target.sh_ok("test -f /tmp/.reload_config_primary"),
        "reload_config primary not executed"
    );
    target.sh("rm -f /tmp/.reload_config_primary");
}

#[tokio::test]
async fn test_custom_file_binary_and_checksum_idempotent() {
    let target = get_opkg_target().await;
    let raw = b"\x00\x01\x02nuci-binary\xfe\xff";
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let checksum = sha256_hex(raw);
    let json = serde_json::json!({
        "packageManager": "opkg",
        "settings": {},
        "files": [{
            "path": "/tmp/nuci_test_bin",
            "content": { "base64": b64 },
            "executable": true,
            "checksum": checksum
        }]
    });
    let path = write_tmp_json(json.to_string());
    target.nuci_deploy(&path);
    let sum_out = target.sh("sha256sum /tmp/nuci_test_bin");
    let got_sum = sum_out.split_whitespace().next().expect("no checksum");
    assert_eq!(got_sum, checksum);
    target.nuci_deploy(&path);
    let sum_out2 = target.sh("sha256sum /tmp/nuci_test_bin");
    let got_sum2 = sum_out2.split_whitespace().next().expect("no checksum");
    assert_eq!(got_sum2, checksum);
}

#[tokio::test]
async fn test_idempotent_list_order() {
    let target = get_opkg_target().await;
    target.reset_uci_state();
    target.sh(
        "uci delete network.lan; uci set network.lan=interface; uci add_list network.lan.ports='wan'; uci add_list network.lan.ports='lan1'; uci add_list network.lan.ports='lan2'; uci commit network",
    );
    target.reset_uci_state();
}

#[tokio::test]
async fn test_section_deletion() {
    let target = get_opkg_target().await;
    target.reset_uci_state();
    target.sh(
        "uci set network.guest=interface; uci set network.guest.proto='dhcp'; uci commit network",
    );
    assert!(
        target.uci_exists("network.guest"),
        "guest section not added"
    );
    target.reset_uci_state();
    assert!(
        !target.uci_exists("network.guest"),
        "guest section should have been removed"
    );
}

#[tokio::test]
async fn test_anonymous_list_deletion_opkg() {
    let target = get_opkg_target().await;
    target.reset_uci_state();
    target.sh(
        "uci add system system; uci set system.@system[-1].hostname='ghost'; uci commit system",
    );
    let count = count_uci_sections(&target.sh("uci show system"));
    assert!(
        count >= 2,
        "expected at least 2 anonymous system sections, got {count}"
    );
    target.reset_uci_state();
    let remaining = target.sh("uci show system");
    let count = count_uci_sections(&remaining);
    assert_eq!(
        count, 1,
        "expected exactly 1 system section, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter")
    );
}

#[tokio::test]
async fn test_anonymous_list_deletion_apk() {
    let target = get_apk_target().await;
    target.reset_uci_state();
    target.sh(
        "uci add system system; uci set system.@system[-1].hostname='ghost'; uci commit system",
    );
    let count = count_uci_sections(&target.sh("uci show system"));
    assert!(
        count >= 2,
        "expected at least 2 anonymous system sections, got {count}"
    );
    target.reset_uci_state();
    let remaining = target.sh("uci show system");
    let count = count_uci_sections(&remaining);
    assert_eq!(
        count, 1,
        "expected exactly 1 system section, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter-apk")
    );
}

#[test]
fn test_nested_and_null_values() {
    let cases: &[(&str, &str)] = &[
        (
            "nested object",
            r#"{"packageManager":"opkg","settings":{"x":{"s":{"_type":"t","obj":{"nested":"v"}}}}}"#,
        ),
        (
            "null value",
            r#"{"packageManager":"opkg","settings":{"x":{"s":{"_type":"t","k":null}}}"#,
        ),
        (
            "hyphen config",
            r#"{"packageManager":"opkg","settings":{"my-config":{}}}"#,
        ),
        (
            "hyphen option",
            r#"{"packageManager":"opkg","settings":{"network":{"lan":{"_type":"interface","ip-address":"10.0.0.1"}}}}"#,
        ),
    ];
    for (name, json) in cases {
        let path = write_tmp_json(json);
        let result = compile_config(&path, None, true);
        assert!(result.is_err(), "{name} should be rejected");
    }
}

#[tokio::test]
async fn test_unified_lifecycle() {
    let target = get_opkg_target().await;
    let boot_json = r#"{"packageManager":"opkg","settings":{"wireless":{"default_radio0":{"_type":"wifi-iface","device":"radio0","network":"lan","mode":"ap","ssid":"gchq-2.4","encryption":"sae-mixed","key":"CHANGE_ME_ON_DEPLOY"}}}}"#;
    let dir = std::env::temp_dir().join("nuci_test");
    fs::create_dir_all(&dir).unwrap();
    let boot_path = dir.join("bootstrap.json");
    fs::write(&boot_path, boot_json).unwrap();
    let compiled = compile_config(&boot_path, None, true).unwrap();
    target.sh(&format!(
        "uci -q batch <<'U'\n{}\nU\nuci commit",
        compiled.uci_batch
    ));
    assert_eq!(
        target.uci_get("wireless.default_radio0.key").as_deref(),
        Some("CHANGE_ME_ON_DEPLOY")
    );
    target.reset_uci_state();
    assert_eq!(
        target.uci_get("wireless.default_radio0.key").as_deref(),
        Some("my-test-password")
    );
}

#[tokio::test]
async fn test_custom_file_text_opkg() {
    let target = get_opkg_target().await;
    let json = serde_json::json!({
        "packageManager": "opkg",
        "settings": {},
        "files": [{"path": "/tmp/nuci_test_custom.txt", "content": "hello from nuci custom files\n", "executable": false}]
    });
    let path = write_tmp_json(json.to_string());
    target.nuci_deploy(&path);
    let content = target.sh("cat /tmp/nuci_test_custom.txt");
    assert_eq!(content.trim(), "hello from nuci custom files");
}

#[tokio::test]
async fn test_custom_file_text_apk() {
    let target = get_apk_target().await;
    let json = serde_json::json!({
        "packageManager": "apk",
        "settings": {},
        "files": [{"path": "/tmp/nuci_test_apk.txt", "content": "apk custom file works\n", "executable": false}]
    });
    let path = write_tmp_json(json.to_string());
    target.nuci_deploy(&path);
    let content = target.sh("cat /tmp/nuci_test_apk.txt");
    assert_eq!(content.trim(), "apk custom file works");
}

#[test]
fn test_hyphen_in_config_name_rejected() {
    let json = serde_json::json!({"packageManager": "opkg", "settings": {"my-config": {}}});
    let path = write_tmp_json(json.to_string());
    let result = compile_config(&path, None, true);
    assert!(result.is_err(), "expected error for hyphenated config name");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid config name")
    );
}

#[test]
fn test_hyphen_in_option_name_rejected() {
    let json = serde_json::json!({"packageManager": "opkg", "settings": {"network": {"lan": {"_type": "interface", "ip-address": "10.0.0.1"}}}});
    let path = write_tmp_json(json.to_string());
    let result = compile_config(&path, None, true);
    assert!(result.is_err(), "expected error for hyphenated option name");
    assert!(result.unwrap_err().to_string().contains("Invalid option"));
}

fn write_tmp_json(content: impl AsRef<str>) -> PathBuf {
    let dir = std::env::temp_dir().join("nuci_test");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("config_{}.json", uuid::Uuid::new_v4()));
    fs::write(&path, content.as_ref().as_bytes()).unwrap();
    path
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::default();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[tokio::test]
async fn test_package_matrix_opkg_all_scenarios() {
    let target = get_opkg_target().await;
    target.reset_uci_state();

    assert!(
        !target.sh_ok("opkg status tcpdump 2>/dev/null | grep -q 'Status:.*installed'"),
        "opkg remote remove (-tcpdump) failed"
    );
    assert!(
        target.sh_ok("opkg status htop 2>/dev/null | grep -q 'Status:.*installed'"),
        "opkg install htop from official feed failed"
    );
}

#[tokio::test]
async fn test_package_matrix_apk_all_scenarios() {
    let target = get_apk_target().await;
    target.reset_uci_state();

    assert!(
        !target.sh_ok("apk info -e tcpdump >/dev/null 2>&1"),
        "apk remote remove (-tcpdump) failed"
    );
    assert!(
        target.sh_ok("apk info -e htop >/dev/null 2>&1"),
        "apk install htop from official feed failed"
    );
}
