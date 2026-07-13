use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::ConfigError;
use crate::generator::{serialize_package_management, serialize_uci};
use crate::models::{PkgBackend, Root};
use crate::validation::validate_root;

const SSH_OPTS: &[&str] = &[
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPath=/tmp/ssh-%r@%h:%p",
    "-o",
    "ControlPersist=5m",
];

fn ssh_exec(target: &str, cmd: &str, stdin_data: Option<&[u8]>) -> Result<String, ConfigError> {
    let mut args: Vec<&str> = SSH_OPTS.to_vec();
    args.push(target);
    args.push(cmd);

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
        .map_err(|e| ConfigError(format!("Failed to spawn ssh: {e}")))?;

    if let Some(data) = stdin_data
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin
            .write_all(data)
            .map_err(|e| ConfigError(format!("Failed to write to ssh stdin: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| ConfigError(format!("Failed to wait for ssh: {e}")))?;

    if !output.status.success() {
        return Err(ConfigError(format!(
            "SSH command failed on {target}: {cmd} (exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn scp_to_target(target: &str, local_path: &Path, remote_path: &str) -> Result<(), ConfigError> {
    let local_str = local_path
        .to_str()
        .ok_or_else(|| ConfigError("Invalid local path".into()))?;

    let status = Command::new("scp")
        .args(SSH_OPTS)
        .arg(local_str)
        .arg(format!("{target}:{remote_path}"))
        .status()
        .map_err(|e| ConfigError(format!("Failed to spawn scp: {e}")))?;

    if !status.success() {
        return Err(ConfigError(format!(
            "SCP failed to copy {} to {target}:{remote_path}",
            local_path.display()
        )));
    }
    Ok(())
}

/// Decrypt SOPS files in memory — no temp files, no tempfile dependency.
pub(crate) fn decrypt_sops_mem(root: &Root) -> Result<HashMap<String, String>, ConfigError> {
    let mut secrets = HashMap::new();
    let sops_files = match root.secrets.as_ref().and_then(|s| s.sops.as_ref()) {
        Some(sops) => &sops.files,
        None => return Ok(secrets),
    };

    for file in sops_files {
        if !Path::new(file).exists() {
            return Err(ConfigError(format!(
                "Configured SOPS file not found: {file}"
            )));
        }

        let output = Command::new("sops")
            .args(["-d", "--output-type", "json", file])
            .output()
            .map_err(|e| ConfigError(format!("Failed to run sops: {e}")))?;

        if !output.status.success() {
            return Err(ConfigError(format!("Failed to decrypt sops file: {file}")));
        }

        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| ConfigError(format!("Failed to parse decrypted JSON: {e}")))?;

        if let Some(obj) = parsed.as_object() {
            for (k, v) in obj {
                if k == "sops" {
                    continue;
                }
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => v.to_string(),
                };
                secrets.insert(k.clone(), val);
            }
        }
    }
    Ok(secrets)
}

fn transfer_packages(target: &str, root: &Root) -> Result<(), ConfigError> {
    let local_pkgs = match &root.package_sources {
        Some(sources) => match &sources.local_packages {
            Some(pkgs) => pkgs,
            None => return Ok(()),
        },
        None => return Ok(()),
    };

    for pkg in local_pkgs {
        let path = Path::new(pkg);
        if !path.exists() {
            continue;
        }
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or(pkg);
        eprintln!("Transferring {filename} to {target}:/tmp/ ...");
        scp_to_target(target, path, &format!("/tmp/{filename}"))?;
    }
    Ok(())
}

fn build_remote_script(
    root: &Root,
    secrets: &HashMap<String, String>,
    uci_commands: &str,
) -> String {
    let mut script = String::with_capacity(4096);

    // 1. SSH keys
    if !root.ssh_keys.is_empty() {
        let mut keys = root.ssh_keys.join("\n");

        // Prevent lockout: check if deployer's key is included
        let deployer_key = Command::new("ssh-add")
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
            .and_then(|s| s.lines().next().map(String::from));

        if let Some(ref key) = deployer_key {
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

    // 3. Backup current config
    script.push_str("cp -a /etc/config /tmp/.uci-rollback-backup\n");

    // 4. UCI commands (piped from compile)
    script.push_str(uci_commands);
    script.push('\n');

    // 5. Rollback watchdog (60s)
    script.push_str(
        "( sleep 60; cp -a /tmp/.uci-rollback-backup/* /etc/config/; \
          if [ -x /sbin/reload_config ]; then /sbin/reload_config; \
          else /etc/init.d/network restart; fi || true; \
          rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid ) & \
          echo $! > /tmp/.uci-watchdog-pid\n",
    );

    // 6. Apply config
    script.push_str("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else /etc/init.d/network restart; fi\n");

    script
}

pub(crate) fn run(json_path: &Path, target: &str) -> Result<(), ConfigError> {
    // 1. Parse config
    let file = fs::File::open(json_path)?;
    let root: Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    validate_root(&root)?;

    // 2. Decrypt SOPS in memory
    let secrets = decrypt_sops_mem(&root)?;

    // 3. Resolve secrets and generate UCI commands
    let resolved_root = crate::secrets::resolve_secrets(root, &secrets)?;
    let mut uci_buffer = String::with_capacity(4096);
    serialize_uci(&mut uci_buffer, &resolved_root.settings)?;
    let backend = PkgBackend::from_str(&resolved_root.package_manager);
    serialize_package_management(
        &mut uci_buffer,
        backend,
        resolved_root.package_sources.as_ref(),
        resolved_root.packages.as_deref(),
    )?;

    // 4. Transfer local packages (separate SCP, can't do in script)
    transfer_packages(target, &resolved_root)?;

    // 5. Build and execute the entire remote deployment script in one SSH call
    let remote_script = build_remote_script(&resolved_root, &secrets, &uci_buffer);
    eprintln!("Deploying to {target}...");
    ssh_exec(target, "sh -s", Some(remote_script.as_bytes()))?;

    // 6. Wait for target to come back, kill rollback watchdog
    eprintln!("Waiting for target to come back (60s rollback window)...");
    let mut connected = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if ssh_exec(
            target,
            "kill $(cat /tmp/.uci-watchdog-pid) 2>/dev/null",
            None,
        )
        .is_ok()
        {
            eprintln!("Connectivity verified, rollback watchdog cancelled.");
            connected = true;
            break;
        }
    }

    if !connected {
        return Err(ConfigError(
            "Failed to reconnect within 60s. Target may have rolled back.".into(),
        ));
    }

    // 7. Cleanup
    let _ = ssh_exec(
        target,
        "rm -rf /tmp/.uci-rollback-backup /tmp/.uci-watchdog-pid",
        None,
    );

    // 8. Setup tinc VPN (if configured)
    let _ = ssh_exec(
        target,
        "if [ ! -f /etc/tinc/retiolum/rsa_key.priv ]; then \
            mkdir -p /etc/tinc/retiolum; \
            tinc -n retiolum generate-keys; \
            /etc/init.d/tinc start; \
        fi",
        None,
    );

    // 9. Sync tinc hosts directory via tar pipe (no rsync dependency)
    let hosts_path = Path::new("/etc/tinc/retiolum/hosts");
    if hosts_path.exists() && fs::read_dir(hosts_path).is_ok_and(|mut d| d.next().is_some()) {
        let tar_output = Command::new("tar")
            .args(["-C", "/etc/tinc/retiolum", "-cf", "-", "hosts"])
            .output()
            .map_err(|e| ConfigError(format!("Failed to run tar locally: {e}")))?;

        if tar_output.status.success() && !tar_output.stdout.is_empty() {
            let _ = ssh_exec(
                target,
                "mkdir -p /etc/tinc/retiolum && tar -C /etc/tinc/retiolum -xf -",
                Some(&tar_output.stdout),
            );
        }
    }

    Ok(())
}
