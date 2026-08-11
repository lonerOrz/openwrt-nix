mod common;

use base64::Engine as _;
use common::{
    Target, apk_json_path, count_uci_sections, get_apk_target, get_opkg_target,
    get_session_artifacts, opkg_json_path, sops_key_file,
};
use nuci::deploy::{DeployConfig, RealSsh};
use nuci::diff;
use nuci::pipeline::compile_config;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

// ══════════════════════════════════════════════════════════════════════════
// 1. Command generation (compile output correctness)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn test_command_generation_opkg() {
    let _artifacts = get_session_artifacts();
    unsafe {
        std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
    }
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
        out.contains("opkg update && opkg install luci"),
        "missing opkg install"
    );
    assert!(
        out.contains("opkg install /tmp/tcpdump.ipk"),
        "missing local ipk"
    );
}

#[test]
fn test_command_generation_apk() {
    let _artifacts = get_session_artifacts();
    unsafe {
        std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
    }
    let compiled = compile_config(&apk_json_path(), None, false).unwrap();
    let out = &compiled.uci_batch;
    assert!(
        out.contains("add system system"),
        "missing add system system"
    );
    assert!(out.contains("apk -U add tcpdump"), "missing apk add");
    assert!(
        out.contains("apk add --allow-untrusted /tmp/libuci20250120.apk"),
        "missing local apk"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 2. Deploy + UCI state verification (real container)
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_opkg_deploy() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
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
    assert!(target.sh_ok("opkg list-installed luci"));
    assert!(target.sh_ok("opkg list-installed tcpdump"));
    let feeds = target.sh("cat /etc/opkg/customfeeds.conf");
    assert!(feeds.contains("src/gz custom https://example.com/packages"));
}

#[tokio::test]
async fn test_apk_deploy() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };
    target.reset_uci_state();

    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter-apk")
    );
    assert!(target.sh_ok("apk info -e tcpdump"));
    assert!(target.sh_ok("apk info -e libuci20250120"));
    let feeds = target.sh("cat /etc/apk/repositories.d/customfeeds.list");
    assert!(feeds.contains("https://example.com/packages"));
}

// ══════════════════════════════════════════════════════════════════════════
// 3. Raw UCI Escape Hatch + Password Sync
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_raw_uci_escape_hatch_opkg() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();
    target.wait_ssh(Duration::from_secs(10));
    assert_eq!(
        target.uci_get("nuci_test.marker").as_deref(),
        Some("escaped")
    );
}

#[tokio::test]
async fn test_raw_uci_escape_hatch_apk() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };
    target.reset_uci_state();
    target.wait_ssh(Duration::from_secs(10));
    assert_eq!(
        target.uci_get("nuci_test.marker").as_deref(),
        Some("escaped")
    );
}

#[tokio::test]
async fn test_password_synced() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();
    let shadow = target.sh("grep '^root:' /etc/shadow");
    assert!(
        shadow.contains("$1$") || shadow.contains("$5$") || shadow.contains("$6$"),
        "expected sha1/sha512 hash in shadow, got: {shadow}"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 4. Idempotency — list order must NOT trigger false changes
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_idempotent_list_order() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();

    // Set the SAME logical list in a DIFFERENT order (simulating hand-edited remote).
    target.sh(&[
        "uci delete network.lan;",
        "uci set network.lan=interface;",
        "uci add_list network.lan.ports='wan';",
        "uci add_list network.lan.ports='lan1';",
        "uci add_list network.lan.ports='lan2';",
        "uci commit network",
    ]
    .join(" "));

    // Redeploy to converge — should be idempotent (no false changes).
    target.reset_uci_state();
}

// ══════════════════════════════════════════════════════════════════════════
// 5. Section Deletion
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_section_deletion() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();

    // Manually add a section not in the Nix config.
    target.sh(&[
        "uci set network.guest=interface;",
        "uci set network.guest.proto='dhcp';",
        "uci commit network",
    ]
    .join(" "));
    assert!(
        target.uci_exists("network.guest"),
        "guest section not added"
    );

    // Redeploy — removed section should be cleared.
    target.reset_uci_state();
    assert!(
        !target.uci_exists("network.guest"),
        "guest section should have been removed"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 6. Anonymous List Deletion (opkg)
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_anonymous_list_deletion_opkg() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();

    // Hand-add a second anonymous system section (not in Nix).
    target.sh(&[
        "uci add system system;",
        "uci set system.@system[-1].hostname='ghost';",
        "uci commit system",
    ]
    .join(" "));
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
        "expected exactly 1 system section after redeploy, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter")
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 7. Anonymous List Deletion (apk)
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_anonymous_list_deletion_apk() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };
    target.reset_uci_state();

    target.sh(&[
        "uci add system system;",
        "uci set system.@system[-1].hostname='ghost';",
        "uci commit system",
    ]
    .join(" "));
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
        "expected exactly 1 system section after redeploy, got {count}: {remaining}"
    );
    assert_eq!(
        target.uci_get("system.@system[0].hostname").as_deref(),
        Some("rauter-apk")
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 8. Diff previews packages & keys
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_diff_previews_packages_and_keys() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();

    let artifacts = get_session_artifacts();
    unsafe {
        std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
    }
    let config = DeployConfig {
        port: target.port,
        identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
        force: false,
        no_sops: false,
        watchdog_timeout: 10,
    };
    let result = diff::run(
        &opkg_json_path(),
        &format!("root@{}", target.host),
        &config,
        None,
        &RealSsh,
    );
    assert!(result.is_ok(), "diff failed");
}

// ══════════════════════════════════════════════════════════════════════════
// 9. Diff Accuracy
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_diff_accuracy() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };
    target.reset_uci_state();

    target.sh("uci set system.@system[0].hostname='manual-change'; uci commit");
    target.sh("uci set network.orphan=interface; uci commit");

    let artifacts = get_session_artifacts();
    unsafe {
        std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
    }
    let config = DeployConfig {
        port: target.port,
        identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
        force: false,
        no_sops: false,
        watchdog_timeout: 10,
    };
    let result = diff::run(
        &apk_json_path(),
        &format!("root@{}", target.host),
        &config,
        None,
        &RealSsh,
    );
    assert!(result.is_ok(), "diff failed");
}

// ══════════════════════════════════════════════════════════════════════════
// 10. Nested / Complex Values (validation)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn test_nested_and_null_values() {
    let cases: &[(&str, &str)] = &[
        (
            "nested object",
            r#"{"packageManager":"opkg","settings":{"x":{"s":{"_type":"t","obj":{"nested":"v"}}}}}"#,
        ),
        (
            "null value",
            r#"{"packageManager":"opkg","settings":{"x":{"s":{"_type":"t","k":null}}}}"#,
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

// ══════════════════════════════════════════════════════════════════════════
// 11. Agent Lockout Prevention
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_agent_lockout_prevention() {
    let Some(target) = Target::new("agent").await else {
        eprintln!("SKIP: agent container unavailable");
        return;
    };

    let artifacts = get_session_artifacts();
    let pub_key =
        fs::read_to_string(artifacts.ssh_key.with_extension("pub")).expect("read pub key");

    // Write authorized_keys manually (simulating nuci's deploy).
    target.sh(&[
        "mkdir -p /etc/dropbear",
        "chmod 700 /etc/dropbear",
        &format!("echo '{}' > /etc/dropbear/authorized_keys", pub_key.trim()),
        "chmod 600 /etc/dropbear/authorized_keys",
    ]
    .join(" && "));

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

// ══════════════════════════════════════════════════════════════════════════
// 12. Watchdog Rollback Recovery
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_watchdog_rollback() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    target.reset_uci_state();

    // Detect SSH daemon and corrupt config + arm watchdog.
    let is_sshd = target.sh_ok("command -v sshd");
    let daemon = if is_sshd { "sshd" } else { "dropbear" };
    let restore = if is_sshd {
        "/usr/sbin/sshd -D -e"
    } else {
        "/usr/sbin/dropbear -F -E -p 22 -R"
    };

    target.sh(&[
        "cp -a /etc/config /tmp/.uci-rollback-backup;",
        "uci set system.@system[0].hostname='CORRUPTED'; uci commit;",
        &format!("pkill -f '/usr/sbin/{daemon}' || true"),
    ]
    .join(" "));

    // Arm watchdog in background.
    target.sh(&[
        "( trap '' HUP; sleep 2;".to_string(),
        "cp -a /tmp/.uci-rollback-backup/* /etc/config/;".to_string(),
        "rm -rf /tmp/.uci-rollback-backup;".to_string(),
        format!("{restore} >/dev/null 2>&1 )"),
        "</dev/null > /tmp/watchdog.log 2>&1 & echo $! > /tmp/.uci-watchdog-pid".to_string(),
    ]
    .join(" "));

    // Poll for rollback.
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

// ══════════════════════════════════════════════════════════════════════════
// 13. Smart Reload Fallback
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_smart_reload_fallback() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };
    // Remove reload_config to force the init.d fallback path.
    target.sh("rm -f /sbin/reload_config");
    target.sh("mkdir -p /etc/init.d");
    for svc in &["dropbear", "network", "firewall", "dnsmasq", "system"] {
        target.sh(&format!(
            "printf '#!/bin/sh\\necho \"{svc} called\" >> /tmp/reload_history\\n' \
             > /etc/init.d/{svc} && chmod +x /etc/init.d/{svc}",
            svc = svc
        ));
    }
    target.sh("rm -f /tmp/reload_history");

    target.reset_uci_state();
    target.wait_ssh(Duration::from_secs(10));

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

    // Cleanup.
    let _ = target.sh("rm -f /sbin/reload_config");
    for svc in &["dropbear", "network", "firewall", "dnsmasq", "system"] {
        let _ = target.sh(&format!("rm -f /etc/init.d/{svc}"));
    }
    let _ = target.sh("rm -f /tmp/reload_history");
}

// ══════════════════════════════════════════════════════════════════════════
// 14. Smart Reload Primary (reload_config path)
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_smart_reload_primary() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };

    // Overwrite reload_config with a marker script.
    target.sh("printf '#!/bin/sh\\ntouch /tmp/.reload_config_primary\\n' > /sbin/reload_config && chmod +x /sbin/reload_config");
    target.sh("rm -f /tmp/.reload_config_primary");

    target.reset_uci_state();
    target.wait_ssh(Duration::from_secs(10));

    // Primary branch should have run reload_config.
    assert!(
        target.sh_ok("test -f /tmp/.reload_config_primary"),
        "/sbin/reload_config primary branch was not executed"
    );

    // Restore original reload_config.
    target.sh("rm -f /tmp/.reload_config_primary");
}

// ══════════════════════════════════════════════════════════════════════════
// 15. Unified Lifecycle (Day-1 bootstrap → Day-2 SOPS deploy)
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_unified_lifecycle() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };

    // Day 1: bootstrap with a plain-text config (no secrets).
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

    // Day 2: full SOPS-decrypting deploy.
    target.reset_uci_state();
    assert_eq!(
        target.uci_get("wireless.default_radio0.key").as_deref(),
        Some("my-test-password")
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 16. Custom Files
// ══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_custom_file_text_opkg() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };

    let json = serde_json::json!({
        "packageManager": "opkg",
        "settings": {},
        "files": [{
            "path": "/tmp/nuci_test_custom.txt",
            "content": "hello from nuci custom files\n",
            "executable": false
        }]
    });
    let path = write_tmp_json(json.to_string());
    target.nuci_deploy(&path);

    let content = target.sh("cat /tmp/nuci_test_custom.txt");
    assert_eq!(content.trim(), "hello from nuci custom files");
}

#[tokio::test]
async fn test_custom_file_text_apk() {
    let Some(target) = get_apk_target().await else {
        eprintln!("SKIP: apk container unavailable");
        return;
    };

    let json = serde_json::json!({
        "packageManager": "apk",
        "settings": {},
        "files": [{
            "path": "/tmp/nuci_test_apk.txt",
            "content": "apk custom file works\n",
            "executable": false
        }]
    });
    let path = write_tmp_json(json.to_string());
    target.nuci_deploy(&path);

    let content = target.sh("cat /tmp/nuci_test_apk.txt");
    assert_eq!(content.trim(), "apk custom file works");
}

#[tokio::test]
async fn test_custom_file_binary_and_checksum_idempotent() {
    let Some(target) = get_opkg_target().await else {
        eprintln!("SKIP: opkg container unavailable");
        return;
    };

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

    // First deploy: write the file.
    target.nuci_deploy(&path);
    let got_sum = target.sh("sha256sum /tmp/nuci_test_bin");
    let got_sum = got_sum
        .split_whitespace()
        .next()
        .expect("no checksum output");
    assert_eq!(got_sum, checksum);

    // Second deploy: checksum guard should skip the write (idempotent).
    target.nuci_deploy(&path);
    let got_sum2 = target.sh("sha256sum /tmp/nuci_test_bin");
    let got_sum2 = got_sum2
        .split_whitespace()
        .next()
        .expect("no checksum output");
    assert_eq!(got_sum2, checksum);
}

// ══════════════════════════════════════════════════════════════════════════
// 17. Hyphen in Config/Section/Option Names
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn test_hyphen_in_config_name_rejected() {
    let json = serde_json::json!({
        "packageManager": "opkg",
        "settings": { "my-config": {} }
    });
    let path = write_tmp_json(json.to_string());
    let result = compile_config(&path, None, true);
    assert!(result.is_err(), "expected error for hyphenated config name");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid config name"), "got: {err}");
}

#[test]
fn test_hyphen_in_option_name_rejected() {
    let json = serde_json::json!({
        "packageManager": "opkg",
        "settings": {
            "network": {
                "lan": {
                    "_type": "interface",
                    "ip-address": "10.0.0.1"
                }
            }
        }
    });
    let path = write_tmp_json(json.to_string());
    let result = compile_config(&path, None, true);
    assert!(result.is_err(), "expected error for hyphenated option name");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid option"), "got: {err}");
}

// ── Helpers ────────────────────────────────────────────────────────────────

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
