use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::deploy::{DeployConfig, ssh_exec};
use crate::error::ConfigError;
use crate::helpers::iter_options;
use crate::models::Section;
use crate::pipeline::compile_config;

/// Flatten Nix config into `config.section.option = value` map (no quoting — matches `uci show`).
fn extract_desired_map(
    configs: &indexmap::IndexMap<String, indexmap::IndexMap<String, Section>>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();

    for (config_name, sections) in configs {
        for (section_name, section) in sections {
            match section {
                Section::Named(obj) => {
                    if let Some(ty) = obj.get("_type").and_then(|v| v.as_str()) {
                        map.insert(format!("{config_name}.{section_name}"), ty.to_string());
                    }
                    for (opt, val) in iter_options(obj) {
                        if let Some(s) = val_str(val) {
                            map.insert(format!("{config_name}.{section_name}.{opt}"), s);
                        }
                    }
                }
                Section::List(arr) => {
                    for (idx, item) in arr.iter().enumerate() {
                        let ty = item
                            .get("_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or(section_name);
                        map.insert(format!("{config_name}.@{ty}[{idx}]"), ty.to_string());
                        for (opt, val) in iter_options(item) {
                            if let Some(s) = val_str(val) {
                                map.insert(format!("{config_name}.@{ty}[{idx}].{opt}"), s);
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
fn parse_uci_show(output: &str) -> BTreeMap<String, String> {
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
    let compiled = compile_config(json_path, secrets_dir)?;
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

    let uci_cmd = format!(
        "for c in {}; do uci -q show \"$c\" 2>/dev/null; done",
        managed.join(" ")
    );
    eprintln!("Fetching current configuration from {target} (read-only)...");
    let remote_output = ssh_exec(target, &uci_cmd, None, config)?;
    let remote = parse_uci_show(&remote_output);

    let all_keys: BTreeSet<&String> = remote.keys().chain(desired.keys()).collect();

    let (mut adds, mut dels, mut mods, mut same) = (0u32, 0u32, 0u32, 0u32);

    println!("\n\x1b[1;36m=== Configuration Diff ({target}) ===\x1b[0m\n");

    for key in all_keys {
        match (remote.get(key), desired.get(key)) {
            (None, Some(d)) => {
                println!("\x1b[32m+ {key}={d}\x1b[0m");
                adds += 1;
            }
            (Some(r), None) => {
                println!("\x1b[31m- {key}={r}\x1b[0m");
                dels += 1;
            }
            (Some(r), Some(d)) if r != d => {
                println!("\x1b[31m- {key}={r}\x1b[0m");
                println!("\x1b[32m+ {key}={d}\x1b[0m");
                mods += 1;
            }
            _ => same += 1,
        }
    }

    println!(
        "\n\x1b[1mSummary:\x1b[0m \x1b[32m{adds} to add\x1b[0m, \x1b[31m{dels} to remove\x1b[0m, \x1b[33m{mods} to change\x1b[0m, {same} unchanged."
    );
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
}
