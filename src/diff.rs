use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::deploy::{DeployConfig, ssh_exec};
use crate::error::ConfigError;
use crate::helpers::iter_options;
use crate::models::{PkgBackend, Section};
use crate::pipeline::compile_config;
use crate::uci_key::{
    anonymous_option_key, anonymous_section_key, named_option_key, named_section_key,
};

pub(crate) const SERVICE_SEPARATOR: &str = "===NUCI_SERVICES===";
pub(crate) const STATE_SEPARATOR: &str = "===NUCI_STATE===";

/// Build a single SSH command that fetches UCI state + discovers init.d service mappings.
pub(crate) fn build_discovery_command(managed: &[&str]) -> String {
    format!(
        "for c in {configs}; do uci -q show \"$c\" 2>/dev/null; done; \
         echo '{sep}'; \
         for c in {configs}; do \
             if [ -x /etc/init.d/\"$c\" ]; then \
                 echo \"$c:/etc/init.d/$c reload\"; \
             elif [ \"$c\" = wireless ]; then \
                 if [ -x /sbin/wifi ]; then echo \"wireless:/sbin/wifi reload\"; \
                 elif [ -x /etc/init.d/network ]; then echo \"wireless:/etc/init.d/network restart\"; \
                 else echo \"wireless:none\"; fi; \
             else \
                 m=$(grep -lE \"config_load .?$c.?\" /etc/init.d/* 2>/dev/null | head -n 1); \
                 if [ -n \"$m\" ]; then echo \"$c:$m reload\"; \
                 else echo \"$c:none\"; fi; \
             fi; \
         done",
        configs = managed.join(" "),
        sep = SERVICE_SEPARATOR,
    )
}

/// Build a single SSH command that probes the live target for package/key/password
/// state, so `nuci diff` can mark what is already deployed vs what will change.
pub(crate) fn build_state_command(packages: &[String], backend: PkgBackend) -> String {
    let probe = match backend {
        PkgBackend::Opkg => "opkg list-installed",
        PkgBackend::Apk => "apk info -e",
    };
    let mut cmd = String::new();
    if !packages.is_empty() {
        cmd.push_str(&format!(
            "for p in {}; do {} \"$p\" >/dev/null 2>&1 && echo \"$p:yes\" || echo \"$p:no\"; done; ",
            packages.join(" "),
            probe
        ));
    }
    cmd.push_str(&format!(
        "echo '{sep}'; cat /etc/dropbear/authorized_keys 2>/dev/null; \
         echo '{sep}'; grep '^root:' /etc/shadow 2>/dev/null | cut -d: -f2",
        sep = STATE_SEPARATOR
    ));
    cmd
}

/// Parse the `pkg:yes|no` lines into a per-package installed map.
pub(crate) fn parse_package_state(output: &str) -> BTreeMap<String, bool> {
    let mut map = BTreeMap::new();
    for line in output.lines() {
        if let Some((pkg, st)) = line.trim().split_once(':')
            && (st == "yes" || st == "no")
        {
            map.insert(pkg.to_string(), st == "yes");
        }
    }
    map
}

/// Parse the service discovery portion of a combined SSH output into `config -> reload command`.
pub(crate) fn parse_services(output: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some((config, cmd)) = line.split_once(':')
            && cmd != "none"
            && !config.is_empty()
        {
            map.insert(config.to_string(), cmd.to_string());
        }
    }
    map
}

/// Flatten Nix config into `config.section.option = value` map (no quoting — matches `uci show`).
pub(crate) fn extract_desired_map(
    configs: &indexmap::IndexMap<String, indexmap::IndexMap<String, Section>>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();

    for (config_name, sections) in configs {
        for (section_name, section) in sections {
            match section {
                Section::Named(obj) => {
                    if let Some(ty) = obj.get("_type").and_then(|v| v.as_str()) {
                        map.insert(named_section_key(config_name, section_name), ty.to_string());
                    }
                    for (opt, val) in iter_options(obj) {
                        if let Some(s) = val_str(val) {
                            map.insert(named_option_key(config_name, section_name, opt), s);
                        }
                    }
                }
                Section::List(arr) => {
                    for (idx, item) in arr.iter().enumerate() {
                        let ty = item
                            .get("_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or(section_name);
                        map.insert(anonymous_section_key(config_name, ty, idx), ty.to_string());
                        for (opt, val) in iter_options(item) {
                            if let Some(s) = val_str(val) {
                                map.insert(anonymous_option_key(config_name, ty, idx, opt), s);
                            }
                        }
                    }
                }
            }
        }
    }
    map
}

/// Format a JSON value as a plain string (no quotes, matching `uci show` output).
fn val_str(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_string()),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().filter_map(val_str).collect();
            if items.is_empty() {
                None
            } else {
                Some(items.join(" "))
            }
        }
        _ => None,
    }
}

/// Strip UCI quotes and unescape from `uci show` output.
///
/// `uci show` wraps values in single quotes: `proto='static'`.
/// Inside quotes, literal `'` is escaped as `'\''`.
/// Arrays use space-separated quoted items: `'a' 'b'`.
fn sanitize_uci_value(v: &str) -> String {
    let trimmed = v.trim();
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        let inside = &trimmed[1..trimmed.len() - 1];
        inside.replace("'\\''", "'").replace("' '", " ")
    } else {
        trimmed.to_string()
    }
}

/// Parse `uci show` output into a flat map.
pub(crate) fn parse_uci_show(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), sanitize_uci_value(v)))
        })
        .collect()
}

pub(crate) fn run(
    json_path: &Path,
    target: &str,
    config: &DeployConfig,
    secrets_dir: Option<&Path>,
) -> Result<(), ConfigError> {
    let compiled = compile_config(json_path, secrets_dir, false)?;
    let desired = extract_desired_map(&compiled.resolved_root.settings);

    let managed: Vec<&str> = compiled
        .resolved_root
        .settings
        .keys()
        .map(|k| k.as_str())
        .collect();

    if managed.is_empty() {
        println!("No settings defined in config.");
        return Ok(());
    }

    let uci_cmd = build_discovery_command(&managed);
    eprintln!("Fetching current configuration & services from {target} (read-only)...");
    let remote_output = ssh_exec(target, &uci_cmd, None, config)?;

    let mut parts = remote_output.splitn(2, SERVICE_SEPARATOR);
    let uci_output = parts.next().unwrap_or("");
    let services_output = parts.next().unwrap_or("");

    let remote = parse_uci_show(uci_output);
    let service_map = parse_services(services_output);

    // Probe live target state (packages / keys / password) in one round-trip.
    let backend = PkgBackend::from_str(&compiled.resolved_root.package_manager);
    let packages = compiled.resolved_root.packages.clone().unwrap_or_default();
    let state_cmd = build_state_command(&packages, backend);
    let state_output = ssh_exec(target, &state_cmd, None, config)?;
    let state_blocks: Vec<&str> = state_output.split(STATE_SEPARATOR).collect();
    let pkg_state = parse_package_state(state_blocks.first().copied().unwrap_or(""));
    let remote_keys = state_blocks
        .get(1)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let remote_shadow = state_blocks
        .get(2)
        .and_then(|s| s.lines().next())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let all_keys: BTreeSet<&String> = remote.keys().chain(desired.keys()).collect();

    let (mut adds, mut dels, mut mods, mut same) = (0u32, 0u32, 0u32, 0u32);
    let mut affected = BTreeSet::new();

    println!("\n\x1b[1;36m=== Configuration Diff ({target}) ===\x1b[0m\n");

    for key in all_keys {
        match (remote.get(key), desired.get(key)) {
            (None, Some(d)) => {
                println!("\x1b[32m+ {key}={d}\x1b[0m");
                adds += 1;
                if let Some(cfg) = key.split('.').next() {
                    affected.insert(cfg.to_string());
                }
            }
            (Some(r), None) => {
                println!("\x1b[31m- {key}={r}\x1b[0m");
                dels += 1;
                if let Some(cfg) = key.split('.').next() {
                    affected.insert(cfg.to_string());
                }
            }
            (Some(r), Some(d)) if r != d => {
                println!("\x1b[31m- {key}={r}\x1b[0m");
                println!("\x1b[32m+ {key}={d}\x1b[0m");
                mods += 1;
                if let Some(cfg) = key.split('.').next() {
                    affected.insert(cfg.to_string());
                }
            }
            _ => same += 1,
        }
    }

    println!(
        "\n\x1b[1mSummary:\x1b[0m \x1b[32m{adds} to add\x1b[0m, \x1b[31m{dels} to remove\x1b[0m, \x1b[33m{mods} to change\x1b[0m, {same} unchanged."
    );

    if !affected.is_empty() {
        println!("\n\x1b[1;33mAffected services (auto-discovered):\x1b[0m");
        for cfg in &affected {
            match service_map.get(cfg) {
                Some(cmd) => println!("  {cfg} \u{2192} {cmd}"),
                None => println!("  {cfg} \u{2192} /etc/init.d/{cfg} reload"),
            }
        }
    }

    // High-risk changes beyond UCI — surface them so `diff` is a real preview,
    // marked against the live target state.
    if let Some(packages) = &compiled.resolved_root.packages {
        if !packages.is_empty() {
            println!("\n\x1b[1;35m[Packages]\x1b[0m");
            for pkg in packages {
                match pkg_state.get(pkg) {
                    Some(true) => println!("  \x1b[90m{pkg}  (Installed)\x1b[0m"),
                    Some(false) => println!("  \x1b[32m+ {pkg}  (To Install)\x1b[0m"),
                    None => println!("  \x1b[32m+ {pkg}  (To Install)\x1b[0m"),
                }
            }
        }
    }

    let ssh_keys = &compiled.resolved_root.ssh_keys;
    if !ssh_keys.is_empty() {
        let desired_keys: BTreeSet<String> = ssh_keys
            .iter()
            .map(|k| {
                // Take only <keytype> <base64>; ignore the trailing comment.
                // The comment is non-identifying, so a comment edit alone
                // won't falsely register as a key change.
                k.split_whitespace()
                    .take(2)
                    .collect::<Vec<&str>>()
                    .join(" ")
            })
            .collect();
        let missing: Vec<&String> = desired_keys
            .iter()
            .filter(|k| !remote_keys.contains(k.as_str()))
            .collect();
        println!("\n\x1b[1;35m[SSH Keys] ({} desired)\x1b[0m", ssh_keys.len());
        if missing.is_empty() {
            println!(
                "  \x1b[90mAll {} key(s) already synced\x1b[0m",
                ssh_keys.len()
            );
        } else {
            println!(
                "  \x1b[32m+ {}/{} key(s) to deploy\x1b[0m",
                missing.len(),
                ssh_keys.len()
            );
        }
    }

    if compiled.secrets.contains_key("root_password") {
        println!("\n\x1b[1;35m[Root Password]\x1b[0m");
        if remote_shadow.is_empty() {
            println!("  \x1b[32mWill be set (target has no root password)\x1b[0m");
        } else {
            println!("  \x1b[90mManaged (target root password already set)\x1b[0m");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uci_show_strips_quotes() {
        let input =
            "network.lan=interface\nnetwork.lan.proto='static'\nnetwork.lan.ipaddr='192.168.1.1'\n";
        let map = parse_uci_show(input);
        assert_eq!(map.get("network.lan.proto"), Some(&"static".to_string()));
        assert_eq!(
            map.get("network.lan.ipaddr"),
            Some(&"192.168.1.1".to_string())
        );
    }

    #[test]
    fn sanitize_escaped_single_quote() {
        // uci show output: 'admin'\''s WiFi'
        assert_eq!(sanitize_uci_value("'admin'\\''s WiFi'"), "admin's WiFi");
    }

    #[test]
    fn sanitize_array() {
        // uci show: 'a' 'b' 'c'
        assert_eq!(sanitize_uci_value("'a' 'b' 'c'"), "a b c");
    }

    #[test]
    fn sanitize_plain_string() {
        assert_eq!(sanitize_uci_value("'hello'"), "hello");
    }

    #[test]
    fn sanitize_no_quotes() {
        assert_eq!(sanitize_uci_value("interface"), "interface");
    }

    #[test]
    fn val_str_types() {
        assert_eq!(
            val_str(&serde_json::Value::String("hello".into())),
            Some("hello".into())
        );
        assert_eq!(val_str(&serde_json::Value::Bool(true)), Some("1".into()));
        assert_eq!(
            val_str(&serde_json::json!([1, "two", true])),
            Some("1 two 1".into())
        );
    }

    #[test]
    fn parse_services_direct_initd() {
        let input = "network:/etc/init.d/network reload\ndropbear:/etc/init.d/dropbear reload\n";
        let map = parse_services(input);
        assert_eq!(
            map.get("network"),
            Some(&"/etc/init.d/network reload".to_string())
        );
        assert_eq!(
            map.get("dropbear"),
            Some(&"/etc/init.d/dropbear reload".to_string())
        );
    }

    #[test]
    fn parse_services_none_excluded() {
        let input = "wireless:none\nfirewall:/etc/init.d/firewall reload\n";
        let map = parse_services(input);
        assert!(!map.contains_key("wireless"));
        assert!(map.contains_key("firewall"));
    }

    #[test]
    fn parse_services_empty() {
        let map = parse_services("");
        assert!(map.is_empty());
    }

    #[test]
    fn build_discovery_command_contains_separator() {
        let cmd = build_discovery_command(&["network", "wireless"]);
        assert!(cmd.contains(SERVICE_SEPARATOR));
        assert!(cmd.contains("uci -q show"));
        assert!(cmd.contains("config_load"));
    }

    #[test]
    fn build_state_command_opkg_probes_packages() {
        let cmd = build_state_command(&["luci".into(), "tcpdump".into()], PkgBackend::Opkg);
        assert!(cmd.contains("opkg list-installed"));
        assert!(cmd.contains("luci"));
        assert!(cmd.contains(STATE_SEPARATOR));
        assert!(cmd.contains("authorized_keys"));
        assert!(cmd.contains("/etc/shadow"));
    }

    #[test]
    fn build_state_command_apk_probes_packages() {
        let cmd = build_state_command(&["luci".into()], PkgBackend::Apk);
        assert!(cmd.contains("apk info -e"));
    }

    #[test]
    fn parse_package_state_marks_installed() {
        let out = "luci:yes\ntcpdump:no\n";
        let map = parse_package_state(out);
        assert_eq!(map.get("luci"), Some(&true));
        assert_eq!(map.get("tcpdump"), Some(&false));
    }
}
