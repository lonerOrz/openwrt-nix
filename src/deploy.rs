use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::ConfigError;
use crate::models::Root;
use crate::pipeline::compile_config;

pub(crate) struct DeployConfig {
    pub port: u16,
    pub identity_file: Option<String>,
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

/// Generate shell reload commands for specific configs.
/// Fast path: `/sbin/reload_config` if available.
/// Fallback: targeted reload per changed config.
fn reload_commands(modified: &[String]) -> String {
    if modified.is_empty() {
        return "if [ -x /sbin/reload_config ]; then /sbin/reload_config; fi\n".to_string();
    }

    let mut out = String::with_capacity(512);
    out.push_str("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else\n");

    let mut network_reloaded = false;
    for config in modified {
        match config.as_str() {
            "network" | "wireless" => {
                if !network_reloaded {
                    out.push_str("  [ -x /etc/init.d/network ] && /etc/init.d/network restart\n");
                    network_reloaded = true;
                }
            }
            "dropbear" => {
                out.push_str("  [ -x /etc/init.d/dropbear ] && /etc/init.d/dropbear reload\n");
            }
            "firewall" => {
                out.push_str("  [ -x /etc/init.d/firewall ] && /etc/init.d/firewall reload\n");
            }
            "dhcp" => {
                out.push_str("  [ -x /etc/init.d/dnsmasq ] && /etc/init.d/dnsmasq reload\n");
            }
            _ => {
                out.push_str(&format!(
                    "  [ -x /etc/init.d/{c} ] && /etc/init.d/{c} reload\n",
                    c = config
                ));
            }
        }
    }

    out.push_str("fi\n");
    out
}

fn build_remote_script(
    root: &Root,
    secrets: &HashMap<String, String>,
    uci_commands: &str,
    deployer_key: Option<&str>,
    modified_configs: &[String],
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
            "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys <<'SSHKEYS'\n{keys}\nSSHKEYS\n"
        ));
    }

    // 2. Root password (heredoc to safely handle special characters)
    if let Some(pwd) = secrets.get("root_password")
        && !pwd.is_empty()
    {
        script.push_str(&format!("chpasswd <<'CHPWD'\nroot:{pwd}\nCHPWD\n"));
    }

    // 3. Persistent backup + boot-time self-destructing rollback hook
    script.push_str("cp -a /etc/config /etc/.uci-rollback-backup\n");
    script.push_str("mkdir -p /etc/init.d /etc/rc.d\n");
    script.push_str("cat > /etc/init.d/nuci_rollback <<'BOOT_EOF'\n");
    script.push_str("#!/bin/sh\n");
    script.push_str(
        "if [ \"$1\" = \"boot\" ] || [ \"$1\" = \"start\" ] || [ \"$1\" = \"\" ]; then\n",
    );
    script.push_str("    if [ -d /etc/.uci-rollback-backup ]; then\n");
    script.push_str("        cp -a /etc/.uci-rollback-backup/* /etc/config/\n");
    script.push_str("        rm -rf /etc/.uci-rollback-backup\n");
    script.push_str("    fi\n");
    script.push_str("    rm -f /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback\n");
    script.push_str("fi\n");
    script.push_str("BOOT_EOF\n");
    script.push_str("chmod +x /etc/init.d/nuci_rollback\n");
    script.push_str("ln -sf /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback\n");

    // 4. UCI commands (piped from compile)
    script.push_str(uci_commands);
    script.push('\n');

    // 5. Rollback watchdog — restore persistent backup + targeted reload on timeout
    let watchdog_timeout =
        std::env::var("NUCI_WATCHDOG_TIMEOUT").unwrap_or_else(|_| "60".to_string());
    let reload_cmds = reload_commands(modified_configs);
    script.push_str(&format!(
        "( sleep {watchdog_timeout}; \
          if [ -d /etc/.uci-rollback-backup ]; then \
              cp -a /etc/.uci-rollback-backup/* /etc/config/; \
              {reload_cmds} \
          fi; \
          rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid \
        ) >/dev/null 2>&1 </dev/null & \
          echo $! > /tmp/.uci-watchdog-pid\n"
    ));

    // 6. Apply config — targeted service reload
    script.push_str(&reload_commands(modified_configs));

    script
}

pub(crate) fn run(
    json_path: &Path,
    target: &str,
    config: &DeployConfig,
    secrets_dir: Option<&Path>,
) -> Result<(), ConfigError> {
    // 1. Compile config through shared pipeline
    let compiled = compile_config(json_path, secrets_dir)?;

    // 2. Transfer local packages via tar over SSH stdin
    transfer_packages(target, &compiled.resolved_root, config)?;

    // Collect which config files are being modified
    let modified_configs: Vec<String> = compiled.resolved_root.settings.keys().cloned().collect();

    // 3. Build and execute the entire remote deployment script in one SSH call
    let deployer_key = get_local_deployer_key();
    let remote_script = build_remote_script(
        &compiled.resolved_root,
        &compiled.secrets,
        &compiled.uci_batch,
        deployer_key.as_deref(),
        &modified_configs,
    );
    eprintln!("Deploying to {target}...");
    ssh_exec(
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
        if ssh_exec(
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
    let _ = ssh_exec(
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
        let out = reload_commands(&[]);
        assert_eq!(
            out,
            "if [ -x /sbin/reload_config ]; then /sbin/reload_config; fi\n"
        );
        // Must NOT contain bare "else\nfi" (POSIX syntax error)
        assert!(!out.contains("else\nfi"));
    }

    #[test]
    fn reload_single_config() {
        let out = reload_commands(&["dropbear".into()]);
        assert!(out.contains("/etc/init.d/dropbear reload"));
        assert!(!out.contains("network restart"));
    }

    #[test]
    fn reload_network_and_wireless_dedup() {
        let out = reload_commands(&["network".into(), "wireless".into()]);
        // Should only contain one network restart, not two
        assert_eq!(out.matches("network restart").count(), 1);
    }

    #[test]
    fn reload_fallback_to_generic_initd() {
        let out = reload_commands(&["custom-svc".into()]);
        assert!(out.contains("/etc/init.d/custom-svc reload"));
    }
}
