use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::compile::pipeline::compile_config;
use crate::config::models::{PkgBackend, Section};
use crate::config::uci_key::is_named_section_key;
use crate::target::capabilities::{discover_package_manager, resolve_backend};
use crate::target::diff::{
    SERVICE_SEPARATOR, build_discovery_command, extract_desired_map, parse_services, parse_uci_show,
};
use crate::utils::error::ConfigError;
use indexmap::IndexMap;

pub struct DeployConfig {
    pub port: u16,
    pub identity_file: Option<String>,
    pub force: bool,
    pub no_sops: bool,
    pub watchdog_timeout: u64,
}

fn build_ssh_args(config: &DeployConfig) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPath=/tmp/ssh-%C".into(),
        "-o".into(),
        "ControlPersist=5m".into(),
    ];
    if config.port != 22 {
        args.extend(["-p".into(), config.port.to_string()]);
    }
    if let Some(ref identity) = config.identity_file {
        args.extend([
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-i".into(),
            identity.clone(),
        ]);
    }
    args
}

pub trait SshExec {
    fn exec(
        &self,
        target: &str,
        cmd: &str,
        stdin_data: Option<&[u8]>,
        config: &DeployConfig,
    ) -> Result<String, ConfigError>;
}

pub struct RealSsh;

impl SshExec for RealSsh {
    fn exec(
        &self,
        target: &str,
        cmd: &str,
        stdin_data: Option<&[u8]>,
        config: &DeployConfig,
    ) -> Result<String, ConfigError> {
        ssh_exec(target, cmd, stdin_data, config)
    }
}

pub fn ssh_exec(
    target: &str,
    cmd: &str,
    stdin_data: Option<&[u8]>,
    config: &DeployConfig,
) -> Result<String, ConfigError> {
    let mut args = build_ssh_args(config);
    args.push(target.into());
    args.push(cmd.into());

    let mut child = Command::new("ssh")
        .args(&args)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| ConfigError::Deploy(format!("Failed to spawn ssh: {e}")))?;

    if let Some(data) = stdin_data
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin
            .write_all(data)
            .map_err(|e| ConfigError::Deploy(format!("Failed to write to ssh stdin: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| ConfigError::Deploy(format!("Failed to wait for ssh: {e}")))?;

    if !output.status.success() {
        return Err(ConfigError::Deploy(format!(
            "SSH command failed on {target}: {cmd} (exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn get_local_deployer_key() -> Option<String> {
    Command::new("ssh-add")
        .arg("-L")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .and_then(|s| s.lines().next().map(String::from))
}

fn orphan_delete_commands(
    remote: &BTreeMap<String, String>,
    desired: &IndexMap<String, IndexMap<String, Section>>,
) -> String {
    let desired_named: HashSet<String> = desired
        .iter()
        .flat_map(|(config, sections)| {
            sections
                .iter()
                .filter(|(_, section)| matches!(section, Section::Named(_)))
                .map(move |(name, _)| format!("{config}.{name}"))
        })
        .collect();

    let mut out = String::new();
    for key in remote.keys() {
        if !is_named_section_key(key) {
            continue;
        }
        if !desired_named.contains(key) {
            out.push_str(&format!("uci -q delete {key}\n"));
        }
    }
    out
}

pub fn run(
    json_path: &Path,
    target: &str,
    config: &DeployConfig,
    secrets_dir: Option<&Path>,
    ssh: &dyn SshExec,
) -> Result<(), ConfigError> {
    // Runtime capability discovery: the package manager actually on the target wins.
    // Only probe when the config actually emits package-manager commands; pure UCI/file
    // deployments need no extra SSH round-trip. A missing/stale `packageManager` no longer
    // sends the wrong commands to the box when packages are involved.
    let hint = crate::compile::pipeline::read_package_manager_hint(json_path)?;
    let backend = if hint.needs_pm {
        let discovered = discover_package_manager(ssh, target, config)?;
        match resolve_backend(&hint.declared, discovered) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Warning: {e}");
                discovered
            }
        }
    } else {
        PkgBackend::try_from(hint.declared.as_str())?
    };
    let compiled = compile_config(json_path, secrets_dir, config.no_sops, Some(backend))?;

    let managed_configs: Vec<String> = compiled.resolved_root.settings.keys().cloned().collect();
    let managed_refs: Vec<&str> = managed_configs.iter().map(|s| s.as_str()).collect();

    let mut service_map = BTreeMap::new();
    let mut remote_map: BTreeMap<String, String> = BTreeMap::new();

    if !managed_refs.is_empty() {
        let discovery_cmd = build_discovery_command(&managed_refs);
        if let Ok(remote_output) = ssh.exec(target, &discovery_cmd, None, config) {
            let mut parts = remote_output.splitn(2, SERVICE_SEPARATOR);
            let uci_output = parts.next().unwrap_or("");
            let services_output = parts.next().unwrap_or("");

            service_map = parse_services(services_output);
            remote_map = parse_uci_show(uci_output);
        }
    }

    let desired_map = extract_desired_map(&compiled.resolved_root.settings);
    let orphan_cmds = orphan_delete_commands(&remote_map, &compiled.resolved_root.settings);
    let has_orphans = !orphan_cmds.is_empty();

    // Conservative skip: remote_map/desired_map only cover UCI settings, so once any
    // non-settings resource (files, packages, rawUci, sshKeys, secrets) is declared we
    // always deploy. ponytail: no remote state tracking for those — safe, just not skip-optimized.
    let has_non_setting_changes = {
        let root = &compiled.resolved_root;
        root.files.as_ref().is_some_and(|f| !f.is_empty())
            || root.raw_uci.as_ref().is_some_and(|r| !r.is_empty())
            || root.packages.as_ref().is_some_and(|p| !p.is_empty())
            || !root.ssh_keys.is_empty()
            || !compiled.secrets.is_empty()
    };

    if !config.force && !has_orphans && remote_map == desired_map && !has_non_setting_changes {
        eprintln!("Configuration on {target} is already up-to-date. Skipping deployment.");
        return Ok(());
    }

    let uci_commands = format!("{orphan_cmds}{}", compiled.uci_batch);

    crate::target::file_transfer::transfer_packages(
        json_path,
        target,
        &compiled.resolved_root,
        config,
        ssh,
    )?;

    let deployer_key = get_local_deployer_key();
    let plan = crate::target::deploy_plan::DeployPlan::new(
        &compiled.resolved_root,
        &compiled.secrets,
        uci_commands,
        deployer_key,
        managed_configs,
        service_map,
        config.watchdog_timeout,
    );
    let remote_script = plan.render();
    eprintln!("Deploying to {target}...");
    ssh.exec(
        target,
        "cat > /tmp/deploy.sh && sh /tmp/deploy.sh",
        Some(remote_script.as_bytes()),
        config,
    )?;

    eprintln!("Waiting for target to come back (60s rollback window)...");
    let mut connected = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Probe connectivity and best-effort cancel the rollback watchdog in a
        // single call. The previous probe used `kill $(cat /tmp/.uci-watchdog-pid)`
        // without `|| true`; when the pid file is absent or the watchdog has
        // already fired, `kill` exits non-zero, so the `.is_ok()` gate reported
        // "not connected" even on a healthy SSH link — wrongly triggering the
        // rollback-failure error. `|| true` makes the call's exit status reflect
        // the SSH connection itself, not the pid file's presence.
        if ssh
            .exec(
                target,
                "kill $(cat /tmp/.uci-watchdog-pid) 2>/dev/null || true",
                None,
                config,
            )
            .is_ok()
        {
            eprintln!("Connectivity verified, rollback watchdog cancelled.");
            connected = true;
            break;
        }
    }

    if !connected {
        return Err(ConfigError::Deploy(
            "Failed to reconnect within 60s. Target may have rolled back.".into(),
        ));
    }

    let _ = ssh.exec(
        target,
        "rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid /tmp/deploy.sh",
        None,
        config,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_deletes_unmanaged_named_sections() {
        let mut sections = IndexMap::new();
        sections.insert(
            "lan".into(),
            Section::Named(crate::config::models::SectionData {
                section_type: "interface".into(),
                options: IndexMap::new(),
            }),
        );
        let mut settings = IndexMap::new();
        settings.insert("network".into(), sections);

        let mut remote = BTreeMap::new();
        remote.insert("network.lan".into(), "interface".into());
        remote.insert("network.guest".into(), "interface".into());
        remote.insert("network.lan.proto".into(), "static".into());
        remote.insert("network.@interface[0]".into(), "interface".into());

        let cmds = orphan_delete_commands(&remote, &settings);
        assert!(cmds.contains("uci -q delete network.guest"));
        assert!(!cmds.contains("uci -q delete network.lan"));
        assert!(!cmds.contains("uci -q delete network.@interface"));
    }

    #[test]
    fn orphan_empty_when_all_declared() {
        let mut sections = IndexMap::new();
        sections.insert(
            "lan".into(),
            Section::Named(crate::config::models::SectionData {
                section_type: "interface".into(),
                options: IndexMap::new(),
            }),
        );
        let mut settings = IndexMap::new();
        settings.insert("network".into(), sections);

        let mut remote = BTreeMap::new();
        remote.insert("network.lan".into(), "interface".into());
        assert!(orphan_delete_commands(&remote, &settings).is_empty());
    }

    struct FakeSsh {
        calls: std::cell::RefCell<Vec<(String, Option<Vec<u8>>)>>,
    }

    impl SshExec for FakeSsh {
        fn exec(
            &self,
            _target: &str,
            cmd: &str,
            stdin_data: Option<&[u8]>,
            _config: &DeployConfig,
        ) -> Result<String, ConfigError> {
            self.calls
                .borrow_mut()
                .push((cmd.to_string(), stdin_data.map(|d| d.to_vec())));
            // Answer the capability probe so discovery succeeds in offline tests.
            if cmd.contains("command -v apk") {
                return Ok("opkg\n".to_string());
            }
            Ok(String::new())
        }
    }

    #[test]
    fn run_orchestrates_deploy_without_ssh() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {
                    "network": {
                        "lan": { "_type": "interface", "proto": "static" }
                    }
                }
            }"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 4);
        let (deploy_cmd, deploy_stdin) = &calls[1];
        assert_eq!(deploy_cmd, "cat > /tmp/deploy.sh && sh /tmp/deploy.sh");
        let script = String::from_utf8_lossy(deploy_stdin.as_ref().unwrap());
        assert!(script.contains("set network.lan.proto='static'"));
        assert!(calls[2].0.contains("kill $(cat /tmp/.uci-watchdog-pid)"));
        assert!(calls[3].0.contains("rm -rf /etc/.uci-rollback-backup"));
    }

    #[test]
    fn build_remote_script_includes_custom_files() {
        use std::io::Write;

        let json_text = r##"{
            "packageManager": "opkg",
            "settings": {},
            "files": [
                {
                    "path": "/etc/rc.local",
                    "content": "#!/bin/sh\necho hello\n",
                    "executable": true
                },
                {
                    "path": "/etc/custom/config.txt",
                    "content": "key=value\n"
                }
            ]
        }"##;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json_text.as_bytes()).unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: true,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());

        assert!(script.contains("mkdir -p '/etc/custom'"));
        // Text content uses POSIX heredoc (zero-dependency, Busybox-safe)
        assert!(script.contains("cat > '/etc/rc.local'"));
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("echo hello"));
        assert!(script.contains("chmod 755 '/etc/rc.local'"));
        assert!(script.contains("cat > '/etc/custom/config.txt'"));
        assert!(script.contains("chmod 644 '/etc/custom/config.txt'"));
    }

    #[test]
    fn build_remote_script_binary_and_checksum() {
        use std::io::Write;

        let json_text = r##"{
            "packageManager": "opkg",
            "settings": {},
            "files": [
                {
                    "path": "/usr/bin/blob",
                    "content": { "base64": "aGVsbG8=" },
                    "executable": true,
                    "checksum": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                }
            ]
        }"##;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json_text.as_bytes()).unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: true,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());

        assert!(script.contains("echo 'aGVsbG8=' | base64 -d > '/usr/bin/blob'"));
        assert!(script.contains("chmod 755 '/usr/bin/blob'"));
        assert!(script.contains("sha256sum '/usr/bin/blob'"));
        assert!(
            script.contains("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert!(script.contains("if [ \"$(sha256sum '/usr/bin/blob'"));
        assert!(script.contains("( sleep 1; "));
    }

    #[test]
    fn run_skips_when_nothing_to_change() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(br#"{ "packageManager": "opkg", "settings": {} }"#)
            .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        assert_eq!(
            ssh.calls.borrow().len(),
            0,
            "empty config must be a no-op skip"
        );
    }

    #[test]
    fn run_does_not_skip_when_files_declared() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {},
                "files": [
                    { "path": "/etc/flag", "content": "" }
                ]
            }"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 3, "files declared must bypass the no-op skip");
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
        assert!(script.contains("cat > '/etc/flag'"));
    }

    #[test]
    fn run_does_not_skip_when_raw_uci_declared() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": ["uci set network.lan.proto='static'"]
            }"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 3, "rawUci declared must bypass the no-op skip");
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
        assert!(script.contains("uci set network.lan.proto='static'"));
    }

    #[test]
    fn run_does_not_skip_when_packages_declared() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {},
                "packages": ["htop"]
            }"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(
            calls.len(),
            4,
            "capability probe + deploy + watchdog + rollback"
        );
        let script = String::from_utf8_lossy(calls[1].1.as_ref().unwrap());
        assert!(script.contains("opkg install 'htop'"));
    }

    #[test]
    fn run_does_not_skip_when_ssh_keys_declared() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {},
                "sshKeys": ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample test"]
            }"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(
            calls.len(),
            3,
            "sshKeys declared must bypass the no-op skip"
        );
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
        assert!(script.contains("/etc/dropbear/authorized_keys"));
    }

    #[test]
    fn run_does_not_skip_when_secrets_loaded() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(br#"{ "packageManager": "opkg", "settings": {} }"#)
            .unwrap();

        let secrets_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            secrets_dir.path().join("secrets.json"),
            br#"{"unreferenced_secret": "value"}"#,
        )
        .unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(
            f.path(),
            "root@127.0.0.1",
            &config,
            Some(secrets_dir.path()),
            &ssh,
        )
        .unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 3, "loaded secrets must bypass the no-op skip");
    }

    #[test]
    fn build_remote_script_uses_base64_for_text_files() {
        use std::io::Write;

        let json_text = r##"{
            "packageManager": "opkg",
            "settings": {},
            "files": [
                {
                    "path": "/tmp/test.txt",
                    "content": "hello world"
                }
            ]
        }"##;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json_text.as_bytes()).unwrap();

        let config = DeployConfig {
            port: 22,
            identity_file: None,
            force: true,
            no_sops: false,
            watchdog_timeout: 60,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        // Verify text files use heredoc (Busybox-safe, no base64 dependency)
        // calls[0] = deploy script, calls[1] = watchdog check, calls[2] = cleanup
        // (discovery call skipped when no settings to manage)
        {
            let calls = ssh.calls.borrow();
            let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
            assert!(script.contains("cat > '/tmp/test.txt'"));
            assert!(script.contains("hello world"));
        }
        // Base64 content still uses decode
        let json_text2 = r##"{
            "packageManager": "opkg",
            "settings": {},
            "files": [
                {
                    "path": "/tmp/bin",
                    "content": { "base64": "aGVsbG8=" }
                }
            ]
        }"##;
        let mut f2 = tempfile::NamedTempFile::new().unwrap();
        f2.write_all(json_text2.as_bytes()).unwrap();
        run(f2.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        {
            let calls = ssh.calls.borrow();
            // calls[3] is the second deploy script (first 3 are from the first run)
            let script2 = String::from_utf8_lossy(calls[3].1.as_ref().unwrap());
            assert!(script2.contains("base64 -d > '/tmp/bin'"));
        }
    }
}
