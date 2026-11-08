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
/// Removes the per-deploy staging script and watchdog pid file left on the
/// target when `run` returns early (e.g. a failed UCI command aborts the deploy
/// script via `set -e`, so execution never reaches the success-path cleanup).
/// RAII: dropped on every `return` from `run`, so a failed deploy does not leave
/// `/tmp/deploy.sh` behind (audit TODO 1.2, acceptance item 5).
struct StagingCleanup<'a> {
    target: &'a str,
    ssh: &'a dyn SshExec,
    config: &'a DeployConfig,
    active: bool,
}

impl<'a> StagingCleanup<'a> {
    /// Starts inert: no remote `rm` on drop until `arm()` is called, which only
    /// happens after the staging script has actually been uploaded. This keeps the
    /// guard a no-op for early returns that never wrote the script (no-op skip),
    /// so it never issues a stray remote command.
    fn new(target: &'a str, ssh: &'a dyn SshExec, config: &'a DeployConfig) -> Self {
        Self {
            target,
            ssh,
            config,
            active: false,
        }
    }
    /// Arm the guard: the staging script exists on the target and must be removed
    /// on any subsequent early return.
    fn arm(&mut self) {
        self.active = true;
    }
    /// Success-path cleanup runs explicitly and disarms the guard so it does not
    /// double-exec (the guard only needs to cover the early-return paths).
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for StagingCleanup<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = self.ssh.exec(
            self.target,
            "rm -f /tmp/deploy.sh /tmp/.uci-watchdog-pid",
            None,
            self.config,
        );
    }
}

pub struct DeployConfig {
    pub port: u16,
    pub identity_file: Option<String>,
    pub force: bool,
    pub no_sops: bool,
    /// Echo captured remote stdout/stderr of deploy/diff to the local terminal instead of discarding it.
    pub verbose: bool,
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
        .stderr(Stdio::piped())
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // Surface the remote diagnostic so a failed UCI batch reports which
        // command broke, not just a non-zero exit.
        return Err(ConfigError::Deploy(format!(
            "SSH command failed on {target} (exit {}): {}{}",
            output.status.code().unwrap_or(-1),
            cmd,
            if stderr.is_empty() {
                String::new()
            } else {
                format!("\nremote stderr:\n{stderr}")
            }
        )));
    }
    // Stderr is captured (not inherited) so failures embed the remote diagnostic
    // in ConfigError; on success it is mirrored to local stderr to keep warning
    // visibility (e.g. opkg "Collected errors: ...").
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        eprintln!("{stderr}");
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
            out.push_str(&format!(
                "if uci -q get {key} >/dev/null 2>&1; then uci -q delete {key}; fi\n"
            ));
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
    // Drop guard (see StagingCleanup): removes the staging script + watchdog pid
    // on any early return; the success path disarms it after the full cleanup.
    let mut staging_cleanup = StagingCleanup::new(target, ssh, config);
    // The package manager actually on the target wins; probe only when the config
    // emits package-manager commands. Pure UCI/file deployments need no SSH probe.
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
        let remote_output = ssh
            .exec(target, &discovery_cmd, None, config)
            .map_err(|e| {
                ConfigError::Deploy(format!(
                    "Failed to read current configuration from {target} before deploy: {e}"
                ))
            })?;
        let mut parts = remote_output.splitn(2, SERVICE_SEPARATOR);
        let uci_output = parts.next().unwrap_or("");
        let services_output = parts.next().unwrap_or("");

        service_map = parse_services(services_output);
        remote_map = parse_uci_show(uci_output);
    }

    let desired_map = extract_desired_map(&compiled.resolved_root.settings);
    let orphan_cmds = orphan_delete_commands(&remote_map, &compiled.resolved_root.settings);
    let has_orphans = !orphan_cmds.is_empty();

    // remote_map/desired_map only cover UCI settings, so any non-settings resource
    // (files, packages, rawUci, sshKeys, secrets) forces a deploy — no remote
    // state tracking exists for those.
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
    let remote_script = crate::target::remote_script::build_remote_script(
        &crate::target::remote_script::RenderInput {
            root: &compiled.resolved_root,
            secrets: &compiled.secrets,
            uci_commands,
            package_commands: compiled.package_commands.clone(),
            deployer_key,
            managed_configs,
            service_map,
            watchdog_timeout: config.watchdog_timeout,
            verbose: config.verbose,
        },
    );
    // Arm the staging guard now: the script is about to be uploaded via this exec,
    // and `cat > /tmp/deploy.sh` runs (and writes the file) before `&& sh` can
    // fail. Any `set -e` abort inside the script returns early via `?` below, and
    // the guard's Drop then removes the staged script.
    staging_cleanup.arm();
    let deploy_out = ssh.exec(
        target,
        "cat > /tmp/deploy.sh && sh /tmp/deploy.sh",
        Some(remote_script.as_bytes()),
        config,
    )?;
    if config.verbose {
        // Echo captured stdout that the deploy flow would otherwise discard.
        print!("{deploy_out}");
    }

    eprintln!("Waiting for target to come back (60s rollback window)...");
    // The reconnect probe and the success cleanup are merged into one exec: the
    // first round-trip that succeeds both verifies connectivity *and* runs the
    // best-effort cleanup, eliminating up to two extra SSH round-trips after a
    // successful reconnect.
    let cleanup = "test -f /tmp/.uci-watchdog-pid && kill $(cat /tmp/.uci-watchdog-pid); \
                   rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback \
                          /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid \
                          /tmp/.nuci-deploy-ok /tmp/deploy.sh";
    let mut connected = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Connectivity = the SSH link itself is up (probe `true` succeeds iff the
        // exec runs). This must NOT depend on the watchdog pid file: after a prior
        // successful deploy the pid is cleaned up, and a subsequent deploy would
        // otherwise be misreported as "not back".
        if ssh
            .exec(target, &format!("true && {cleanup}"), None, config)
            .is_ok()
        {
            eprintln!("Connectivity verified.");
            // Full success cleanup done above (best-effort); disarm the RAII guard
            // so its Drop does not issue a redundant remote `rm`.
            staging_cleanup.disarm();
            connected = true;
            break;
        }
    }

    if !connected {
        return Err(ConfigError::Deploy(
            "Failed to reconnect within 60s. Target may have rolled back.".into(),
        ));
    }

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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        // discovery(0) + deploy(1) + reconnect/cleanup merged(2).
        assert_eq!(calls.len(), 3);
        let (deploy_cmd, deploy_stdin) = &calls[1];
        assert_eq!(deploy_cmd, "cat > /tmp/deploy.sh && sh /tmp/deploy.sh");
        let script = String::from_utf8_lossy(deploy_stdin.as_ref().unwrap());
        assert!(script.contains("set network.lan.proto='static'"));
        assert!(calls[2].0.contains("kill $(cat /tmp/.uci-watchdog-pid)"));
        assert!(calls[2].0.contains("rm -rf /etc/.uci-rollback-backup"));
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());

        assert!(script.contains("mkdir -p '/etc/custom'"));
        assert!(script.contains("cat > '/etc/rc.local.tmp'"));
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("echo hello"));
        assert!(script.contains("chmod 755 '/etc/rc.local.tmp'"));
        assert!(script.contains("mv -f '/etc/rc.local.tmp' '/etc/rc.local'"));
        assert!(script.contains("cat > '/etc/custom/config.txt.tmp'"));
        assert!(script.contains("chmod 644 '/etc/custom/config.txt.tmp'"));
        assert!(script.contains("mv -f '/etc/custom/config.txt.tmp' '/etc/custom/config.txt'"));
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());

        assert!(script.contains("echo 'aGVsbG8=' | base64 -d > '/usr/bin/blob.tmp'"));
        assert!(script.contains("chmod 755 '/usr/bin/blob.tmp'"));
        // Post-write checksum verify is against the .tmp file before rename.
        assert!(script.contains("sha256sum '/usr/bin/blob.tmp'"));
        assert!(
            script.contains("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert!(script.contains("if [ \"$(sha256sum '/usr/bin/blob'"));
        assert!(script.contains("mv -f '/usr/bin/blob.tmp' '/usr/bin/blob'"));
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
            verbose: false,
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 2, "files declared must bypass the no-op skip");
        let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
        assert!(script.contains("cat > '/etc/flag.tmp'"));
        assert!(script.contains("mv -f '/etc/flag.tmp' '/etc/flag'"));
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(calls.len(), 2, "rawUci declared must bypass the no-op skip");
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(
            calls.len(),
            3,
            "capability probe + deploy + (connectivity probe + cleanup merged)"
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();
        let calls = ssh.calls.borrow();
        assert_eq!(
            calls.len(),
            2,
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
            verbose: false,
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
        assert_eq!(calls.len(), 2, "loaded secrets must bypass the no-op skip");
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
            verbose: false,
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        // First run: calls[0]=deploy script, calls[1]=capability probe,
        // calls[2]=reconnect probe + success cleanup (merged into one exec;
        // discovery is skipped when there are no settings to manage). Second
        // run appends its own 3 calls, so the second deploy script is at calls[3].
        {
            let calls = ssh.calls.borrow();
            let script = String::from_utf8_lossy(calls[0].1.as_ref().unwrap());
            assert!(script.contains("cat > '/tmp/test.txt.tmp'"));
            assert!(script.contains("mv -f '/tmp/test.txt.tmp' '/tmp/test.txt'"));
            assert!(script.contains("hello world"));
        }
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
            // calls[2] is the second deploy script (first 2 calls are from the first run)
            let script2 = String::from_utf8_lossy(calls[2].1.as_ref().unwrap());
            assert!(script2.contains("base64 -d > '/tmp/bin.tmp'"));
            assert!(script2.contains("mv -f '/tmp/bin.tmp' '/tmp/bin'"));
        }
    }

    /// A failed remote deploy (early `?` return) must still leave no staging
    /// script: `StagingCleanup::drop` issues `rm -f /tmp/deploy.sh`.
    #[test]
    fn run_cleans_staging_script_on_failed_deploy() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(
            br#"{
                "packageManager": "opkg",
                "settings": {
                    "network": { "lan": { "_type": "interface", "proto": "static" } }
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
            verbose: false,
        };
        // FailingSsh succeeds on the capability probe but errors when asked to run
        // the deploy script, simulating a `set -e` abort on the target.
        let ssh = FailingSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        let result = run(f.path(), "root@127.0.0.1", &config, None, &ssh);
        assert!(
            result.is_err(),
            "failed deploy script must surface an error"
        );

        let calls = ssh.calls.borrow();
        let cleanup_ran = calls
            .iter()
            .any(|(cmd, _)| cmd.contains("rm -f /tmp/deploy.sh"));
        assert!(
            cleanup_ran,
            "staging script must be removed even when the deploy fails: {calls:?}"
        );
    }

    /// Like `FakeSsh` but the deploy-script exec fails, so `run` returns early.
    struct FailingSsh {
        calls: std::cell::RefCell<Vec<(String, Option<Vec<u8>>)>>,
    }

    impl SshExec for FailingSsh {
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
            if cmd.contains("command -v apk") {
                return Ok("opkg\n".to_string());
            }
            // The deploy script is run via `cat > /tmp/deploy.sh && sh /tmp/deploy.sh`.
            if cmd.contains("sh /tmp/deploy.sh") {
                return Err(ConfigError::Deploy("simulated deploy failure".into()));
            }
            Ok(String::new())
        }
    }
}
