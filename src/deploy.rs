use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::diff::{
    SERVICE_SEPARATOR, build_discovery_command, extract_desired_map, parse_services, parse_uci_show,
};
use crate::error::ConfigError;
use crate::models::{Root, Section};
use crate::pipeline::compile_config;
use crate::uci_key::is_named_section_key;
use indexmap::IndexMap;

pub(crate) struct DeployConfig {
    pub port: u16,
    pub identity_file: Option<String>,
    pub force: bool,
}

fn build_ssh_args(config: &DeployConfig) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "UserKnownHostsFile=/dev/null".into(),
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
        args.extend(["-i".into(), identity.clone()]);
    }
    args
}

/// Transport seam for running a command on the target over SSH.
///
/// `run` takes a `&dyn SshExec` so the deploy orchestration can be exercised
/// against an in-memory fake in unit tests without a real device or container.
/// `RealSsh` is the only production adapter; `ssh_exec` wraps it for the rest
/// of the codebase (e.g. `diff::run`).
pub(crate) trait SshExec {
    fn exec(
        &self,
        target: &str,
        cmd: &str,
        stdin_data: Option<&[u8]>,
        config: &DeployConfig,
    ) -> Result<String, ConfigError>;
}

/// Production transport: shells out to the `ssh` binary.
pub(crate) struct RealSsh;

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

pub(crate) fn ssh_exec(
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

fn transfer_packages(target: &str, root: &Root, config: &DeployConfig) -> Result<(), ConfigError> {
    let local_pkgs = match &root.package_sources {
        Some(sources) => match &sources.local_packages {
            Some(pkgs) => pkgs,
            None => return Ok(()),
        },
        None => return Ok(()),
    };

    if local_pkgs.is_empty() {
        return Ok(());
    }

    // Stage all packages into a temp dir
    let staging = tempfile::tempdir()?;
    for pkg in local_pkgs {
        let path = Path::new(pkg);
        if !path.exists() {
            return Err(ConfigError::Validation(format!(
                "Local package not found: {}",
                path.display()
            )));
        }
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or(pkg);
        std::fs::copy(path, staging.path().join(filename))?;
    }

    // Tar + SSH stdin — single channel, no scp dependency
    eprintln!("Bundling {} local package(s)...", local_pkgs.len());
    let tar_bytes = Command::new("tar")
        .arg("-cf")
        .arg("-")
        .arg("-C")
        .arg(staging.path())
        .arg(".")
        .output()
        .map_err(|e| ConfigError::Deploy(format!("Failed to run local tar: {e}")))?;

    if !tar_bytes.status.success() {
        return Err(ConfigError::Deploy(format!(
            "Local tar failed: {}",
            String::from_utf8_lossy(&tar_bytes.stderr)
        )));
    }

    eprintln!("Transferring to {target}:/tmp/ via SSH stream...");
    ssh_exec(target, "tar -xf - -C /tmp", Some(&tar_bytes.stdout), config)?;
    Ok(())
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

/// Generate shell reload commands using the target's dynamically discovered service map.
/// Zero hardcoded service names — fully self-adaptive.
fn reload_commands(modified: &[String], service_map: &BTreeMap<String, String>) -> String {
    if modified.is_empty() {
        return "if [ -x /sbin/reload_config ]; then /sbin/reload_config; fi\n".to_string();
    }

    let mut out = String::with_capacity(512);
    out.push_str("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else\n");

    let mut seen = std::collections::HashSet::new();
    for config in modified {
        let cmd = service_map
            .get(config)
            .cloned()
            .unwrap_or_else(|| format!("/etc/init.d/{config} reload"));

        // Dedup by command string (e.g. network+wireless both mapping to same restart)
        if seen.insert(cmd.clone()) {
            let bin = cmd.split_whitespace().next().unwrap_or(&cmd);
            out.push_str(&format!("  [ -x {bin} ] && {cmd}\n"));
        }
    }

    out.push_str("fi\n");
    out
}

/// The boot-time rollback hook: a procd-style init script installed as
/// `S15nuci_rollback` that, on the next boot, restores `/etc/config` from the
/// pre-deploy backup and then self-deletes. This is the safety net for a
/// device that reboots *during* the watchdog window (the watchdog itself only
/// covers a still-running device). Firing on boot requires procd as PID 1,
/// which the test harness does not run (see test/containers.py); the hook's
/// shell logic is unit-tested in isolation via `boot_rollback_hook_restores`.
fn boot_rollback_hook() -> String {
    let mut s = String::from("cp -a /etc/config /etc/.uci-rollback-backup\n");
    s.push_str("mkdir -p /etc/init.d /etc/rc.d\n");
    s.push_str("cat > /etc/init.d/nuci_rollback <<'BOOT_EOF'\n");
    s.push_str("#!/bin/sh\n");
    s.push_str("if [ \"$1\" = \"boot\" ] || [ \"$1\" = \"start\" ] || [ \"$1\" = \"\" ]; then\n");
    s.push_str("    if [ -d /etc/.uci-rollback-backup ]; then\n");
    s.push_str("        cp -a /etc/.uci-rollback-backup/* /etc/config/\n");
    s.push_str("        rm -rf /etc/.uci-rollback-backup\n");
    s.push_str("    fi\n");
    s.push_str("    rm -f /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback\n");
    s.push_str("fi\n");
    s.push_str("BOOT_EOF\n");
    s.push_str("chmod +x /etc/init.d/nuci_rollback\n");
    s.push_str("ln -sf /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback\n");
    s.push_str("timeout 5 sync 2>/dev/null || true\n");
    s
}

fn build_remote_script(
    root: &Root,
    secrets: &HashMap<String, String>,
    uci_commands: &str,
    deployer_key: Option<&str>,
    modified_configs: &[String],
    service_map: &BTreeMap<String, String>,
) -> String {
    let mut script = String::with_capacity(4096);

    // 1. SSH keys
    if !root.ssh_keys.is_empty() {
        let mut keys = root.ssh_keys.join("\n");

        if let Some(key) = deployer_key {
            let pub_part: String = key
                .split_whitespace()
                .take(2)
                .collect::<Vec<&str>>()
                .join(" ");
            if !keys.contains(&pub_part) {
                eprintln!("⚠ Deployer key not in config, appending to prevent lockout...");
                keys = format!("{keys}\n{key}");
            }
        } else {
            eprintln!(
                "⚠ No active SSH keys in local ssh-agent. Ensure root.sshKeys contains your key."
            );
        }

        script.push_str(&format!(
            "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys <<'SSHKEYS'\n{keys}\nSSHKEYS\n\
             chmod 700 /etc/dropbear && chmod 600 /etc/dropbear/authorized_keys\n"
        ));
    }

    // 2. Root password (heredoc to safely handle special characters)
    if let Some(pwd) = secrets.get("root_password")
        && !pwd.is_empty()
    {
        script.push_str(&format!(
            "if command -v chpasswd >/dev/null 2>&1; then\n\
             chpasswd <<'CHPWD'\nroot:{pwd}\nCHPWD\n\
             else\n\
             printf '{pwd}\\n{pwd}\\n' | passwd root >/dev/null 2>&1\n\
             fi\n"
        ));
    }

    // 2.5. Custom files
    if let Some(files) = &root.files {
        for (i, file) in files.iter().enumerate() {
            let dest = &file.path;
            let dir = Path::new(dest).parent().unwrap_or(Path::new("/"));
            let dir_str = dir.to_string_lossy();
            script.push_str(&format!("mkdir -p '{dir_str}'\n"));
            script.push_str(&format!("cat > '{dest}' <<'NUCI_FILE_{i}_EOF'\n"));
            script.push_str(&file.content);
            if !file.content.ends_with('\n') {
                script.push('\n');
            }
            script.push_str(&format!("NUCI_FILE_{i}_EOF\n"));
            if file.executable {
                script.push_str(&format!("chmod 755 '{dest}'\n"));
            } else {
                script.push_str(&format!("chmod 644 '{dest}'\n"));
            }
        }
    }

    // 3. Persistent backup + boot-time self-destructing rollback hook
    script.push_str(&boot_rollback_hook());

    // 4. UCI commands (piped from compile)
    script.push_str(uci_commands);
    script.push('\n');

    // 5. Rollback watchdog — restore persistent backup + targeted reload on timeout
    let watchdog_timeout =
        std::env::var("NUCI_WATCHDOG_TIMEOUT").unwrap_or_else(|_| "60".to_string());
    let reload_cmds = reload_commands(modified_configs, service_map);
    script.push_str(&format!(
        "( trap '' HUP; sleep {watchdog_timeout}; \
          if [ -d /etc/.uci-rollback-backup ]; then \
              cp -a /etc/.uci-rollback-backup/* /etc/config/; \
              {reload_cmds} \
          fi; \
          rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid \
        ) >/dev/null 2>&1 </dev/null & \
          echo $! > /tmp/.uci-watchdog-pid\n"
    ));

    // 6. Apply config — targeted service reload
    script.push_str(&reload_commands(modified_configs, service_map));

    script
}

/// Emit `uci delete` for named sections that exist on the target but are no
/// longer declared in the Nix config. Anonymous list sections are skipped —
/// they have no stable identity to match against across redeploys.
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
        // A named-section root key from `uci show` looks like `config.name`
        // (exactly one dot, no '@' anonymous marker, no '[index]').
        if !is_named_section_key(key) {
            continue;
        }
        if !desired_named.contains(key) {
            out.push_str(&format!("uci -q delete {key}\n"));
        }
    }
    out
}

pub(crate) fn run(
    json_path: &Path,
    target: &str,
    config: &DeployConfig,
    secrets_dir: Option<&Path>,
    ssh: &dyn SshExec,
) -> Result<(), ConfigError> {
    let compiled = compile_config(json_path, secrets_dir, false)?;

    let managed_configs: Vec<String> = compiled.resolved_root.settings.keys().cloned().collect();
    let managed_refs: Vec<&str> = managed_configs.iter().map(|s| s.as_str()).collect();

    // Combined idempotency check + service discovery (single SSH round-trip)
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
    // Sections present on the target but removed from the Nix config must be
    // cleared, so "delete from Nix" actually deletes on the router.
    let orphan_cmds = orphan_delete_commands(&remote_map, &compiled.resolved_root.settings);
    let has_orphans = !orphan_cmds.is_empty();

    if !config.force && !has_orphans && remote_map == desired_map {
        eprintln!("Configuration on {target} is already up-to-date. Skipping deployment.");
        return Ok(());
    }

    // Prepend orphan-section deletions so removed Nix sections are cleared on target.
    let uci_commands = format!("{orphan_cmds}{}", compiled.uci_batch);

    transfer_packages(target, &compiled.resolved_root, config)?;

    let deployer_key = get_local_deployer_key();
    let remote_script = build_remote_script(
        &compiled.resolved_root,
        &compiled.secrets,
        &uci_commands,
        deployer_key.as_deref(),
        &managed_configs,
        &service_map,
    );
    eprintln!("Deploying to {target}...");
    ssh.exec(
        target,
        "cat > /tmp/deploy.sh && sh /tmp/deploy.sh",
        Some(remote_script.as_bytes()),
        config,
    )?;

    // 4. Wait for target to come back, kill rollback watchdog
    eprintln!("Waiting for target to come back (60s rollback window)...");
    let mut connected = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if ssh
            .exec(
                target,
                "kill $(cat /tmp/.uci-watchdog-pid) 2>/dev/null",
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

    // 5. Cleanup — remove persistent backup, boot hook, and watchdog PID
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
    fn reload_empty_configs_skips_else() {
        let out = reload_commands(&[], &BTreeMap::new());
        assert_eq!(
            out,
            "if [ -x /sbin/reload_config ]; then /sbin/reload_config; fi\n"
        );
        assert!(!out.contains("else\nfi"));
    }

    #[test]
    fn reload_single_config() {
        let out = reload_commands(&["dropbear".into()], &BTreeMap::new());
        assert!(out.contains("/etc/init.d/dropbear reload"));
        assert!(!out.contains("network restart"));
    }

    #[test]
    fn reload_uses_dynamic_map() {
        let mut map = BTreeMap::new();
        map.insert("dhcp".into(), "/etc/init.d/dnsmasq reload".into());
        let out = reload_commands(&["dhcp".into()], &map);
        assert!(out.contains("/etc/init.d/dnsmasq reload"));
        assert!(!out.contains("/etc/init.d/dhcp reload"));
    }

    #[test]
    fn reload_dedup_by_command() {
        let mut map = BTreeMap::new();
        map.insert("network".into(), "/etc/init.d/network restart".into());
        map.insert("wireless".into(), "/etc/init.d/network restart".into());
        let out = reload_commands(&["network".into(), "wireless".into()], &map);
        assert_eq!(out.matches("network restart").count(), 1);
    }

    #[test]
    fn reload_fallback_to_generic_initd() {
        let out = reload_commands(&["custom-svc".into()], &BTreeMap::new());
        assert!(out.contains("/etc/init.d/custom-svc reload"));
    }

    #[test]
    fn reload_nonempty_keeps_primary_reload_config_branch() {
        // The primary `if [ -x /sbin/reload_config ]` branch must be emitted
        // even when there are modified configs (not just the empty case), so a
        // real device with procd falls back to the canonical reload_config.
        let out = reload_commands(&["network".into()], &BTreeMap::new());
        assert!(out.starts_with("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else"));
        assert!(out.contains("/etc/init.d/network reload"));
        assert!(out.trim_end().ends_with("fi"));
    }

    #[test]
    fn orphan_deletes_unmanaged_named_sections() {
        use indexmap::IndexMap;
        use serde_json::Map;

        let mut obj = Map::new();
        obj.insert(
            "_type".into(),
            serde_json::Value::String("interface".into()),
        );
        let mut sections = IndexMap::new();
        sections.insert("lan".into(), Section::Named(obj));
        let mut settings = IndexMap::new();
        settings.insert("network".into(), sections);

        // Remote has network.lan (declared) + network.guest (not declared).
        let mut remote = BTreeMap::new();
        remote.insert("network.lan".into(), "interface".into());
        remote.insert("network.guest".into(), "interface".into());
        remote.insert("network.lan.proto".into(), "static".into());
        remote.insert("network.@interface[0]".into(), "interface".into());

        let cmds = orphan_delete_commands(&remote, &settings);
        assert!(cmds.contains("uci -q delete network.guest"));
        assert!(!cmds.contains("uci -q delete network.lan"));
        // Anonymous sections must never be deleted by this path.
        assert!(!cmds.contains("uci -q delete network.@interface"));
    }

    #[test]
    fn orphan_empty_when_all_declared() {
        use indexmap::IndexMap;
        use serde_json::Map;

        let mut obj = Map::new();
        obj.insert(
            "_type".into(),
            serde_json::Value::String("interface".into()),
        );
        let mut sections = IndexMap::new();
        sections.insert("lan".into(), Section::Named(obj));
        let mut settings = IndexMap::new();
        settings.insert("network".into(), sections);

        let mut remote = BTreeMap::new();
        remote.insert("network.lan".into(), "interface".into());
        assert!(orphan_delete_commands(&remote, &settings).is_empty());
    }

    /// In-memory transport that records every call and returns scripted output.
    /// Lets `run` be exercised with zero SSH/container involvement.
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
            // Discovery call returns no remote config; watchdog/cleanup calls
            // succeed so the happy path completes.
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
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        // 1) discovery, 2) deploy script (has stdin), 3) watchdog poll, 4) cleanup.
        assert_eq!(calls.len(), 4);
        let (deploy_cmd, deploy_stdin) = &calls[1];
        assert_eq!(deploy_cmd, "cat > /tmp/deploy.sh && sh /tmp/deploy.sh");
        let script = String::from_utf8_lossy(deploy_stdin.as_ref().unwrap());
        // Compiled UCI batch must be present in the deployed script.
        assert!(script.contains("set network.lan.proto='static'"));
        // Watchdog kill + cleanup must both have been issued.
        assert!(calls[2].0.contains("kill $(cat /tmp/.uci-watchdog-pid)"));
        assert!(calls[3].0.contains("rm -rf /etc/.uci-rollback-backup"));
    }

    #[test]
    fn boot_rollback_hook_restores_and_self_deletes() {
        use std::fs;
        use std::process::Command;

        // The harness runs without procd as PID 1, so the hook cannot be
        // exercised across a real reboot. Instead we run its init-script body
        // directly (the `if [ "$1" = boot ] ...` block) against a fake /etc
        // tree, asserting it restores the pre-deploy backup and removes itself.
        let hook = boot_rollback_hook();
        let start = hook.find("if [ \"$1\"").expect("hook has boot guard");
        let end = hook
            .find("BOOT_EOF\n")
            .expect("hook has heredoc terminator");
        let body = &hook[start..end];

        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        fs::create_dir_all(etc.join("config")).unwrap();
        fs::create_dir_all(etc.join("init.d")).unwrap();
        fs::create_dir_all(etc.join("rc.d")).unwrap();
        // Pre-deploy backup with the "good" hostname.
        fs::create_dir_all(etc.join(".uci-rollback-backup")).unwrap();
        fs::write(
            etc.join(".uci-rollback-backup").join("system"),
            "config system\n\toption hostname 'good'\n",
        )
        .unwrap();
        // Live config corrupted during deploy.
        fs::write(
            etc.join("config").join("system"),
            "config system\n\toption hostname 'CORRUPTED'\n",
        )
        .unwrap();

        // Run the hook body with /etc rewritten to our temp root.
        let script = body.replace("/etc", &etc.to_string_lossy());
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("{script}\n"))
            .output()
            .unwrap();
        assert!(out.status.success(), "hook body failed: {:?}", out);

        // Backup restored into live config.
        let restored = fs::read_to_string(etc.join("config").join("system")).unwrap();
        assert!(restored.contains("good"), "config not restored: {restored}");
        // Backup consumed.
        assert!(
            !etc.join(".uci-rollback-backup").exists(),
            "backup not removed"
        );
        // Hook self-deleted.
        assert!(
            !etc.join("init.d").join("nuci_rollback").exists(),
            "init script not removed"
        );
        assert!(
            !etc.join("rc.d").join("S15nuci_rollback").exists(),
            "rc.d symlink not removed"
        );
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
        };
        let ssh = FakeSsh {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        run(f.path(), "root@127.0.0.1", &config, None, &ssh).unwrap();

        let calls = ssh.calls.borrow();
        let deploy_stdin = &calls[0].1.as_ref().unwrap();
        let script = String::from_utf8_lossy(deploy_stdin);

        assert!(script.contains("mkdir -p '/etc/custom'"));
        assert!(script.contains("cat > '/etc/rc.local'"));
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("echo hello"));
        assert!(script.contains("chmod 755 '/etc/rc.local'"));
        assert!(script.contains("cat > '/etc/custom/config.txt'"));
        assert!(script.contains("chmod 644 '/etc/custom/config.txt'"));
    }
}
