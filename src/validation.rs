use crate::error::ConfigError;
use crate::helpers::iter_options;
use crate::models::{Root, Section};
use serde_json::Value;

fn is_valid_uci_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_valid_uci_type(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn validate_root(root: &Root) -> Result<(), ConfigError> {
    for (config_name, sections) in &root.settings {
        if !is_valid_uci_identifier(config_name) {
            return Err(ConfigError(format!(
                "Invalid config name '{}': only [a-zA-Z0-9_] allowed",
                config_name
            )));
        }

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    if arr.is_empty() {
                        return Err(ConfigError(format!(
                            "Empty list section '{}' in config '{}' is not supported: its UCI type cannot be determined. To remove a section, omit it from your Nix configuration.",
                            section_name, config_name
                        )));
                    }
                    if !is_valid_uci_type(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid list identifier '{}' in config '{}': only [a-zA-Z0-9_-] allowed",
                            section_name, config_name
                        )));
                    }

                    for (idx, item) in arr.iter().enumerate() {
                        let ty = item.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                            ConfigError(format!(
                                "{}.@{}[{}] missing required '_type'",
                                config_name, section_name, idx
                            ))
                        })?;

                        if !is_valid_uci_type(ty) {
                            return Err(ConfigError(format!(
                                "Invalid type '{}' in {}.@{}[{}]: only [a-zA-Z0-9_-] allowed",
                                ty, config_name, section_name, idx
                            )));
                        }

                        for (opt_name, opt_val) in iter_options(item) {
                            if !is_valid_uci_identifier(opt_name) {
                                return Err(ConfigError(format!(
                                    "Invalid option '{}' in {}.@{}[{}]: only [a-zA-Z0-9_] allowed",
                                    opt_name, config_name, section_name, idx
                                )));
                            }
                            if matches!(opt_val, Value::Null) {
                                return Err(ConfigError(format!(
                                    "{}.@{}[{}].{} has null value",
                                    config_name, section_name, idx, opt_name
                                )));
                            }
                        }
                    }
                }
                Section::Named(map) => {
                    if !is_valid_uci_identifier(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid section '{}' in config '{}': only [a-zA-Z0-9_] allowed",
                            section_name, config_name
                        )));
                    }

                    let ty = map.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                        ConfigError(format!(
                            "{}.{} missing required '_type'",
                            config_name, section_name
                        ))
                    })?;

                    if !is_valid_uci_type(ty) {
                        return Err(ConfigError(format!(
                            "Invalid type '{}' in {}.{}: only [a-zA-Z0-9_-] allowed",
                            ty, config_name, section_name
                        )));
                    }

                    for (opt_name, opt_val) in iter_options(map) {
                        if !is_valid_uci_identifier(opt_name) {
                            return Err(ConfigError(format!(
                                "Invalid option '{}' in {}.{}: only [a-zA-Z0-9_] allowed",
                                opt_name, config_name, section_name
                            )));
                        }
                        if matches!(opt_val, Value::Null) {
                            return Err(ConfigError(format!(
                                "{}.{}.{} has null value",
                                config_name, section_name, opt_name
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;
    use std::collections::BTreeMap;

    #[test]
    fn validate_rejects_hyphen_in_config_name() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([("network-config".into(), BTreeMap::new())]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "wireless".into(),
                BTreeMap::from([("radio0".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("my-section".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "dropbear".into(),
                BTreeMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "wireless".into(),
                BTreeMap::from([("wifi-iface".into(), Section::List(vec![]))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::from([(
                "dropbear".into(),
                BTreeMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            opkg: None,
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
            settings: BTreeMap::new(),
            packages: None,
            opkg: None,
            ssh_keys: vec![],
            secrets: None,
        };
        assert!(validate_root(&root).is_ok());
    }
}
