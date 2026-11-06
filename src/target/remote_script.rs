//! Renders the remote deploy script from a resolved configuration.
//!
//! This module is a pure renderer: given a `Root` (already secret-resolved),
//! the interpolated secrets, and the UCI command batch, it produces the shell
//! script string that `target::deploy` will ship over SSH. It performs no IO
//! and does not execute anything — that separation is what lets the deploy
//! orchestrator stay a thin coordinator.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::config::models::{FileContent, Root};

/// Build the reload / `reload_config` command block for the modified configs.
fn reload_commands(modified: &[String], service_map: &BTreeMap<String, String>) -> String {
    if modified.is_empty() {
        return "if [ -x /sbin/reload_config ]; then /sbin/reload_config; fi\n".to_string();
    }

    let mut out = String::with_capacity(512);
    out.push_str("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else\n");

    let mut seen = HashSet::new();
    for config in modified {
        let cmd = service_map
            .get(config)
            .cloned()
            .unwrap_or_else(|| format!("/etc/init.d/{config} reload"));

        if seen.insert(cmd.clone()) {
            let bin = cmd.split_whitespace().next().unwrap_or(&cmd);
            out.push_str(&format!("  [ -x {bin} ] && {cmd}\n"));
        }
    }
    out.push_str("fi\n");
    out
}

/// procd init script installed as `S15nuci_rollback` that restores `/etc/config`
/// on boot if the watchdog triggers or power-loss occurs.
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

/// Render the full remote script: ssh keys, root password, files, the boot
/// rollback hook, the UCI command batch, and the detached watchdog that reverts
/// on timeout. Pure string assembly — no side effects.
pub fn build_remote_script(
    root: &Root,
    secrets: &HashMap<String, String>,
    uci_commands: &str,
    deployer_key: Option<&str>,
    modified_configs: &[String],
    service_map: &BTreeMap<String, String>,
    watchdog_timeout: u64,
) -> String {
    let mut script = String::with_capacity(4096);

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

    if let Some(pwd) = secrets.get("root_password")
        && !pwd.is_empty()
    {
        script.push_str(&format!(
            "if command -v chpasswd >/dev/null 2>&1; then\n\
             chpasswd <<'CHPWD'\nroot:{pwd}\nCHPWD\n\
             else\n\
             printf '%s\\n%s\\n' '{pwd}' '{pwd}' | passwd root >/dev/null 2>&1\n\
             fi\n"
        ));
    }

    if let Some(files) = &root.files {
        for (i, file) in files.iter().enumerate() {
            let dest = &file.path;
            let dir = std::path::Path::new(dest)
                .parent()
                .unwrap_or(std::path::Path::new("/"));
            let dir_str = dir.to_string_lossy();
            script.push_str(&format!("mkdir -p '{dir_str}'\n"));

            let guard_open = match &file.checksum {
                Some(sum) => format!(
                    "if [ \"$(sha256sum '{dest}' 2>/dev/null | cut -d' ' -f1)\" != '{sum}' ]; then\n"
                ),
                None => String::new(),
            };
            let indent = if file.checksum.is_some() { "    " } else { "" };
            script.push_str(&guard_open);

            // Unrecoverable OpenWrt Busybox limitation: base64 may not be available in
            // minimal rootfs images. Text files use POSIX cat heredoc for zero-dependency.
            // Only explicit base64 payloads use decode to avoid re-encoding.
            match &file.content {
                FileContent::Text(text) => {
                    script.push_str(&format!("{indent}cat > '{dest}' <<'NUCI_FILE_{i}_EOF'\n"));
                    script.push_str(indent);
                    script.push_str(text);
                    if !text.ends_with('\n') {
                        script.push('\n');
                    }
                    script.push_str(&format!("{indent}NUCI_FILE_{i}_EOF\n"));
                }
                FileContent::Base64(b64) => {
                    script.push_str(&format!("{indent}echo '{b64}' | base64 -d > '{dest}'\n"));
                }
            }

            if file.executable {
                script.push_str(&format!("{indent}chmod 755 '{dest}'\n"));
            } else {
                script.push_str(&format!("{indent}chmod 644 '{dest}'\n"));
            }

            if file.checksum.is_some() {
                script.push_str("fi\n");
            }
        }
    }

    script.push_str(&boot_rollback_hook());
    script.push_str(uci_commands);
    script.push('\n');

    let reload_cmds = reload_commands(modified_configs, service_map);
    // Unrecoverable Linux process signal behavior: trap '' HUP is required so background watchdog
    // survives SSH session disconnect.
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

    // Unrecoverable OpenWrt SSH network disconnect behavior: reloading network or Dropbear/SSHD
    // synchronously in the foreground SSH session severs the connection (exit status 255),
    // bypassing the 60s reconnection handshake. Detaching reload with `(sleep 1; ...) &`
    // allows `/tmp/deploy.sh` to exit 0 first so the client enters the handshake loop.
    script.push_str(&format!("( sleep 1; {reload_cmds} ) >/dev/null 2>&1 &\n"));

    script
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

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
        let out = reload_commands(&["network".into()], &BTreeMap::new());
        assert!(out.starts_with("if [ -x /sbin/reload_config ]; then /sbin/reload_config; else"));
        assert!(out.contains("/etc/init.d/network reload"));
        assert!(out.trim_end().ends_with("fi"));
    }

    #[test]
    fn boot_rollback_hook_restores_and_self_deletes() {
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
        fs::create_dir_all(etc.join(".uci-rollback-backup")).unwrap();
        fs::write(
            etc.join(".uci-rollback-backup").join("system"),
            "config system\n\toption hostname 'good'\n",
        )
        .unwrap();
        fs::write(
            etc.join("config").join("system"),
            "config system\n\toption hostname 'CORRUPTED'\n",
        )
        .unwrap();

        let script = body.replace("/etc", &etc.to_string_lossy());
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("{script}\n"))
            .output()
            .unwrap();
        assert!(out.status.success(), "hook body failed: {:?}", out);

        let restored = fs::read_to_string(etc.join("config").join("system")).unwrap();
        assert!(restored.contains("good"), "config not restored: {restored}");
        assert!(
            !etc.join(".uci-rollback-backup").exists(),
            "backup not removed"
        );
        assert!(
            !etc.join("init.d").join("nuci_rollback").exists(),
            "init script not removed"
        );
        assert!(
            !etc.join("rc.d").join("S15nuci_rollback").exists(),
            "rc.d symlink not removed"
        );
    }
}
