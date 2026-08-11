use crate::error::ConfigError;
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

fn validate_section_options(
    options: &indexmap::IndexMap<String, Value>,
    config_name: &str,
    section_path: &str,
) -> Result<(), ConfigError> {
    for (opt_name, opt_val) in options {
        if !is_valid_uci_identifier(opt_name) {
            return Err(ConfigError::Validation(format!(
                "Invalid option '{opt_name}' in {config_name}.{section_path}: only [a-zA-Z0-9_] allowed"
            )));
        }
        if matches!(opt_val, Value::Null) {
            return Err(ConfigError::Validation(format!(
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
            return Err(ConfigError::Validation(format!(
                "Invalid config name '{config_name}': only [a-zA-Z0-9_] allowed (no digits at start)"
            )));
        }

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    if arr.is_empty() {
                        return Err(ConfigError::Validation(format!(
                            "Empty list section '{section_name}' in config '{config_name}' is not supported: its UCI type cannot be determined. To remove a section, omit it from your Nix configuration."
                        )));
                    }
                    if !is_valid_uci_type(section_name) {
                        return Err(ConfigError::Validation(format!(
                            "Invalid list identifier '{section_name}' in config '{config_name}': only [a-zA-Z0-9_-] allowed"
                        )));
                    }

                    for (idx, item) in arr.iter().enumerate() {
                        if !is_valid_uci_type(&item.section_type) {
                            return Err(ConfigError::Validation(format!(
                                "Invalid type '{}' in {config_name}.@{section_name}[{idx}]",
                                item.section_type
                            )));
                        }
                        let path = format!("@{section_name}[{idx}]");
                        validate_section_options(&item.options, config_name, &path)?;
                    }
                }
                Section::Named(section) => {
                    if !is_valid_uci_identifier(section_name) {
                        return Err(ConfigError::Validation(format!(
                            "Invalid section '{section_name}' in config '{config_name}': only [a-zA-Z0-9_] allowed (no digits at start)"
                        )));
                    }
                    if !is_valid_uci_type(&section.section_type) {
                        return Err(ConfigError::Validation(format!(
                            "Invalid type '{}' in {config_name}.{section_name}",
                            section.section_type
                        )));
                    }
                    validate_section_options(&section.options, config_name, section_name)?;
                }
            }
        }
    }

    if let Some(raw) = &root.raw_uci {
        for (i, line) in raw.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Err(ConfigError::Validation(format!("rawUci[{i}] is empty")));
            }
            if !trimmed.starts_with("uci ") {
                return Err(ConfigError::Validation(format!(
                    "rawUci[{i}] must be a 'uci' command, got: {trimmed}"
                )));
            }
        }
    }

    if let Some(files) = &root.files {
        for (i, file) in files.iter().enumerate() {
            let path = &file.path;
            if !path.starts_with('/') {
                return Err(ConfigError::Validation(format!(
                    "files[{i}].path must be absolute, got: {path}"
                )));
            }
            if path.contains("..") {
                return Err(ConfigError::Validation(format!(
                    "files[{i}].path must not contain '..': {path}"
                )));
            }
            if file.content.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "files[{i}].path={path} has empty content"
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Section, SectionData};
    use indexmap::IndexMap;

    fn empty_root() -> Root {
        Root {
            package_manager: "opkg".into(),
            settings: IndexMap::new(),
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
            raw_uci: None,
            files: None,
        }
    }

    fn named_cfg(
        config: &str,
        section: &str,
        section_type: &str,
        options: IndexMap<String, Value>,
    ) -> Root {
        Root {
            settings: IndexMap::from([(
                config.into(),
                IndexMap::from([(
                    section.into(),
                    Section::Named(SectionData {
                        section_type: section_type.into(),
                        options,
                    }),
                )]),
            )]),
            ..empty_root()
        }
    }

    fn anon_cfg(
        config: &str,
        section: &str,
        section_type: &str,
        options: IndexMap<String, Value>,
    ) -> Root {
        Root {
            settings: IndexMap::from([(
                config.into(),
                IndexMap::from([(
                    section.into(),
                    Section::List(vec![SectionData {
                        section_type: section_type.into(),
                        options,
                    }]),
                )]),
            )]),
            ..empty_root()
        }
    }

    fn opts(items: &[(&str, Value)]) -> IndexMap<String, Value> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn assert_rejects(root: &Root, expected: &str) {
        let err = validate_root(root).unwrap_err();
        assert!(
            format!("{err}").contains(expected),
            "expected '{expected}' in '{err}'"
        );
    }

    #[test]
    fn rejects_invalid_named_identifiers() {
        for (config, section, ty, options, expected) in [
            (
                "network-config",
                "lan",
                "interface",
                IndexMap::new(),
                "Invalid config name",
            ),
            (
                "3network",
                "lan",
                "interface",
                IndexMap::new(),
                "Invalid config name",
            ),
            (
                "network",
                "my-section",
                "interface",
                IndexMap::new(),
                "Invalid section",
            ),
            (
                "network",
                "lan",
                "bad type!",
                IndexMap::new(),
                "Invalid type",
            ),
            (
                "network",
                "lan",
                "interface",
                opts(&[("ip-address", "1.1.1.1".into())]),
                "Invalid option",
            ),
            (
                "network",
                "lan",
                "interface",
                opts(&[("0proto", "static".into())]),
                "Invalid option",
            ),
        ] {
            assert_rejects(&named_cfg(config, section, ty, options), expected);
        }
    }

    #[test]
    fn rejects_invalid_list_identifiers() {
        for (config, section, ty, options, expected) in [
            (
                "dropbear",
                "dropbear",
                "bad type!",
                IndexMap::new(),
                "Invalid type",
            ),
            (
                "dropbear",
                "dropbear",
                "dropbear",
                opts(&[("listen-port", "22".into())]),
                "Invalid option",
            ),
        ] {
            assert_rejects(&anon_cfg(config, section, ty, options), expected);
        }
    }

    #[test]
    fn rejects_null_value_and_empty_list() {
        assert_rejects(
            &named_cfg(
                "network",
                "lan",
                "interface",
                opts(&[("proto", Value::Null)]),
            ),
            "null value",
        );
        let root = Root {
            settings: IndexMap::from([(
                "wireless".into(),
                IndexMap::from([("wifi-iface".into(), Section::List(vec![]))]),
            )]),
            ..empty_root()
        };
        assert_rejects(&root, "Empty list section");
    }

    #[test]
    fn allows_valid_configs() {
        assert!(validate_root(&empty_root()).is_ok());
        // Types may contain hyphens (wifi-iface), identifiers may not.
        assert!(
            validate_root(&named_cfg(
                "wireless",
                "radio0",
                "wifi-iface",
                IndexMap::new()
            ))
            .is_ok()
        );
    }

    #[test]
    fn validates_raw_uci() {
        let root = Root {
            raw_uci: Some(vec!["rm -rf /".into()]),
            ..empty_root()
        };
        assert_rejects(&root, "must be a 'uci' command");

        let root = Root {
            raw_uci: Some(vec!["uci rename network.lan=lan2".into()]),
            ..empty_root()
        };
        assert!(validate_root(&root).is_ok());
    }
}
