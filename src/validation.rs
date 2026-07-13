use crate::error::ConfigError;
use crate::helpers::iter_options;
use crate::models::{Root, Section};
use serde_json::Value;

fn is_valid_uci_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_valid_uci_type(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn validate_section_map(
    map: &serde_json::Map<String, Value>,
    config_name: &str,
    section_path: &str,
) -> Result<(), ConfigError> {
    let ty = map.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
        ConfigError(format!(
            "{config_name}.{section_path} missing required '_type'"
        ))
    })?;

    if !is_valid_uci_type(ty) {
        return Err(ConfigError(format!(
            "Invalid type '{ty}' in {config_name}.{section_path}: only [a-zA-Z0-9_-] allowed"
        )));
    }

    for (opt_name, opt_val) in iter_options(map) {
        if !is_valid_uci_identifier(opt_name) {
            return Err(ConfigError(format!(
                "Invalid option '{opt_name}' in {config_name}.{section_path}: only [a-zA-Z0-9_-] allowed"
            )));
        }
        if matches!(opt_val, Value::Null) {
            return Err(ConfigError(format!(
                "{config_name}.{section_path}.{opt_name} has null value"
            )));
        }
        if let Value::String(s) = opt_val
            && s.is_empty()
        {
            eprintln!(
                "Warning: {config_name}.{section_path}.{opt_name} is empty string — UCI treats '' as unset. Consider omitting it."
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_root(root: &Root) -> Result<(), ConfigError> {
    for (config_name, sections) in &root.settings {
        if !is_valid_uci_identifier(config_name) {
            return Err(ConfigError(format!(
                "Invalid config name '{config_name}': only [a-zA-Z0-9_-] allowed"
            )));
        }

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    if arr.is_empty() {
                        return Err(ConfigError(format!(
                            "Empty list section '{section_name}' in config '{config_name}' is not supported: its UCI type cannot be determined. To remove a section, omit it from your Nix configuration."
                        )));
                    }
                    if !is_valid_uci_type(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid list identifier '{section_name}' in config '{config_name}': only [a-zA-Z0-9_-] allowed"
                        )));
                    }

                    for (idx, item) in arr.iter().enumerate() {
                        let path = format!("@{section_name}[{idx}]");
                        validate_section_map(item, config_name, &path)?;
                    }
                }
                Section::Named(map) => {
                    if !is_valid_uci_identifier(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid section '{section_name}' in config '{config_name}': only [a-zA-Z0-9_-] allowed"
                        )));
                    }
                    validate_section_map(map, config_name, section_name)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use serde_json::Map;

    #[test]
    fn validate_rejects_hyphen_in_config_name() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([("network-config".into(), IndexMap::new())]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid config name"));
    }

    #[test]
    fn validate_allows_hyphen_in_type() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("wifi-iface".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "wireless".into(),
                IndexMap::from([("radio0".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        assert!(validate_root(&root).is_ok());
    }

    #[test]
    fn validate_rejects_hyphen_in_option_name() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("ip-address".into(), Value::String("192.168.1.1".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "network".into(),
                IndexMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid option"));
    }

    #[test]
    fn validate_rejects_hyphen_in_section_name() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "network".into(),
                IndexMap::from([("my-section".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid section"));
    }

    #[test]
    fn validate_rejects_null_value() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("proto".into(), Value::Null);
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "network".into(),
                IndexMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("null value"));
    }

    #[test]
    fn validate_rejects_missing_type() {
        let mut obj = Map::new();
        obj.insert("proto".into(), Value::String("static".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "network".into(),
                IndexMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("missing required '_type'"));
    }

    #[test]
    fn validate_list_missing_type() {
        let mut item = Map::new();
        item.insert("Port".into(), Value::String("22".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "dropbear".into(),
                IndexMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("missing required '_type'"));
    }

    #[test]
    fn validate_rejects_empty_list_section() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "wireless".into(),
                IndexMap::from([("wifi-iface".into(), Section::List(vec![]))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Empty list section"));
    }

    #[test]
    fn validate_list_rejects_hyphen_in_option() {
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("dropbear".into()));
        item.insert("listen-port".into(), Value::String("22".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "dropbear".into(),
                IndexMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid option"));
    }

    #[test]
    fn validate_empty_settings_ok() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::new(),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        assert!(validate_root(&root).is_ok());
    }

    #[test]
    fn validate_rejects_digit_start_in_config_name() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([("3network".into(), IndexMap::new())]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid config name"));
    }

    #[test]
    fn validate_rejects_digit_start_in_option() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("0proto".into(), Value::String("static".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: IndexMap::from([(
                "network".into(),
                IndexMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid option"));
    }
}
