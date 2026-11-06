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

use crate::config::models::Root;

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

/// Render the authorized-keys block: joins declared SSH keys, appends the active
/// deployer key to prevent lockout when it isn't already present, and writes
/// `/etc/dropbear/authorized_keys`. `deployer_key` is `None` when no local agent
/// key is available — the caller is warned but the block is still emitted for any
/// configured keys. Pure string assembly — the `eprintln!` warnings are the only
/// side effect and are preserved from the original inline logic.
fn render_ssh_keys(keys: &[String], deployer_key: Option<&str>) -> String {
    if keys.is_empty() {
        return String::new();
    }

    let mut joined = keys.join("\n");
    if let Some(key) = deployer_key {
        let pub_part: String = key
            .split_whitespace()
            .take(2)
            .collect::<Vec<&str>>()
            .join(" ");
        if !joined.contains(&pub_part) {
            eprintln!("⚠ Deployer key not in config, appending to prevent lockout...");
            joined = format!("{joined}\n{key}");
        }
    } else {
        eprintln!(
            "⚠ No active SSH keys in local ssh-agent. Ensure root.sshKeys contains your key."
        );
    }

    format!(
        "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys <<'SSHKEYS'\n{joined}\nSSHKEYS\n\
         chmod 700 /etc/dropbear && chmod 600 /etc/dropbear/authorized_keys\n"
    )
}

/// Render the root password block. Uses `chpasswd` when present, otherwise falls
/// back to `passwd`. Emits nothing when `root_password` is absent or empty.
fn render_root_password(root_password: &str) -> String {
    if root_password.is_empty() {
        return String::new();
    }
    format!(
        "if command -v chpasswd >/dev/null 2>&1; then\n\
         chpasswd <<'CHPWD'\nroot:{root_password}\nCHPWD\n\
         else\n\
         printf '%s\\n%s\\n' '{root_password}' '{root_password}' | passwd root >/dev/null 2>&1\n\
         fi\n"
    )
}

/// Render the detach-on-disconnect watchdog plus the foreground reload tail.
///
/// The watchdog runs `sleep {watchdog_timeout}` in a `trap '' HUP` subshell so it
/// survives SSH session loss; if a rollback backup still exists when it fires, it
/// restores `/etc/config` and re-runs the reload commands before clearing state.
/// The separate `( sleep 1; {reload_cmds} ) &` tail lets `/tmp/deploy.sh` exit 0
/// first so a foreground network/Dropbear reload doesn't sever the SSH link.
/// Pure string assembly — no side effects.
fn render_watchdog(reload_cmds: &str, watchdog_timeout: u64) -> String {
    format!(
        "( trap '' HUP; sleep {watchdog_timeout}; \
          if [ -d /etc/.uci-rollback-backup ]; then \
              cp -a /etc/.uci-rollback-backup/* /etc/config/; \
              {reload_cmds} \
          fi; \
          rm -rf /etc/.uci-rollback-backup /etc/init.d/nuci_rollback /etc/rc.d/S15nuci_rollback /tmp/.uci-watchdog-pid \
        ) >/dev/null 2>&1 </dev/null & \
          echo $! > /tmp/.uci-watchdog-pid\n\
         ( sleep 1; {reload_cmds} ) >/dev/null 2>&1 &\n"
    )
}

/// Render the full remote script: ssh keys, root password, files, the boot
/// rollback hook, the UCI command batch, and the detached watchdog that reverts
/// on timeout. Pure string assembly — no side effects. When `verbose` is set,
/// the script enables `set -x` tracing and emits a `[nuci] stage:` banner before
/// each logical stage so `--verbose` surfaces an ordered, per-stage view.
/// All inputs needed to render the remote deploy script. Bundling them in one
/// struct keeps `build_remote_script` a single-argument pure function and avoids
/// an over-long parameter list.
pub struct RenderInput<'a> {
    pub root: &'a Root,
    pub secrets: &'a HashMap<String, String>,
    pub uci_commands: String,
    pub deployer_key: Option<String>,
    pub managed_configs: Vec<String>,
    pub service_map: BTreeMap<String, String>,
    pub watchdog_timeout: u64,
    pub verbose: bool,
}

/// Render the full remote script: ssh keys, root password, files, the boot
/// rollback hook, the UCI command batch, and the detached watchdog that reverts
/// on timeout. Pure string assembly — no side effects. When `verbose` is set,
/// the script enables `set -x` tracing and emits a `[nuci] stage:` banner before
/// each logical stage so `--verbose` surfaces an ordered, per-stage view.
pub fn build_remote_script(input: &RenderInput) -> String {
    let RenderInput {
        root,
        secrets,
        uci_commands,
        deployer_key,
        managed_configs,
        service_map,
        watchdog_timeout,
        verbose,
    } = input;
    let mut script = String::with_capacity(4096);

    if *verbose {
        // Trace every command (with its stdout/stderr) to stderr, and emit a
        // machine-scannable banner before each logical stage so `--verbose`
        // surfaces an ordered, per-stage view of the deploy instead of silent stdout.
        script.push_str("set -x\n");
    }
    let stage = |script: &mut String, name: &str| {
        if *verbose {
            script.push_str(&format!("echo '[nuci] stage: {name}'\n"));
        }
    };

    stage(&mut script, "ssh-keys");
    script.push_str(&render_ssh_keys(&root.ssh_keys, deployer_key.as_deref()));

    if let Some(pwd) = secrets.get("root_password") {
        stage(&mut script, "root-password");
        script.push_str(&render_root_password(pwd));
    }

    if let Some(files) = &root.files {
        stage(&mut script, "files");
        script.push_str(&crate::target::file_transfer::render_file_block(files));
    }

    stage(&mut script, "uci");
    script.push_str(&boot_rollback_hook());
    script.push_str(uci_commands);
    script.push('\n');

    let reload_cmds = reload_commands(managed_configs, service_map);
    stage(&mut script, "reload");
    script.push_str(&render_watchdog(&reload_cmds, *watchdog_timeout));

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
    #[test]
    fn verbose_emits_stage_markers_and_set_x() {
        let json = r#"{
            "packageManager": "opkg",
            "settings": {},
            "sshKeys": ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample test"],
            "files": [{ "path": "/etc/nuci-managed.txt", "content": "ok" }]
        }"#;
        let root: crate::config::models::Root = serde_json::from_str(json).expect("valid root");
        let secrets = std::collections::HashMap::new();

        let quiet = build_remote_script(&RenderInput {
            root: &root,
            secrets: &secrets,
            uci_commands: "set system.cfg01=system\n".to_string(),
            deployer_key: None,
            managed_configs: vec![],
            service_map: std::collections::BTreeMap::new(),
            watchdog_timeout: 60,
            verbose: false,
        });
        assert!(!quiet.contains("set -x"), "quiet must not enable tracing");
        assert!(
            !quiet.contains("[nuci] stage:"),
            "quiet must not emit stage banners"
        );

        let verbose = build_remote_script(&RenderInput {
            root: &root,
            secrets: &secrets,
            uci_commands: "set system.cfg01=system\n".to_string(),
            deployer_key: None,
            managed_configs: vec![],
            service_map: std::collections::BTreeMap::new(),
            watchdog_timeout: 60,
            verbose: true,
        });
        assert!(verbose.contains("set -x"), "verbose must enable tracing");
        assert!(
            verbose.contains("[nuci] stage: ssh-keys"),
            "verbose must banner ssh-keys stage"
        );
        assert!(
            verbose.contains("[nuci] stage: files"),
            "verbose must banner files stage"
        );
        assert!(
            verbose.contains("[nuci] stage: uci"),
            "verbose must banner uci stage"
        );
        assert!(
            verbose.contains("[nuci] stage: reload"),
            "verbose must banner reload stage"
        );
        let pos = |s: &str| verbose.find(s).unwrap();
        assert!(pos("[nuci] stage: ssh-keys") < pos("[nuci] stage: files"));
        assert!(pos("[nuci] stage: files") < pos("[nuci] stage: uci"));
        assert!(pos("[nuci] stage: uci") < pos("[nuci] stage: reload"));
    }
}
