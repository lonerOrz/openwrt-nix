use crate::config::models::{Root, Section};
use crate::config::package::{PackageAction, RemotePath};
use serde_json::Value;
use std::path::Path;

fn is_valid_uci_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn is_valid_config_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
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
) -> Result<(), super::super::utils::error::ConfigError> {
    for (opt_name, opt_val) in options {
        if !is_valid_uci_identifier(opt_name) {
            return Err(super::super::utils::error::ConfigError::Validation(
                format!(
                    "Invalid option '{opt_name}' in {config_name}.{section_path}: only [a-zA-Z0-9_] allowed"
                ),
            ));
        }
        if matches!(opt_val, Value::Null) {
            return Err(super::super::utils::error::ConfigError::Validation(
                format!("{config_name}.{section_path}.{opt_name} has null value"),
            ));
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

pub(crate) fn validate_root(root: &Root) -> Result<(), super::super::utils::error::ConfigError> {
    for (config_name, sections) in &root.settings {
        if !is_valid_config_name(config_name) {
            return Err(super::super::utils::error::ConfigError::Validation(
                format!(
                    "Invalid config name '{config_name}': must start with a letter and contain only [a-zA-Z0-9_-]"
                ),
            ));
        }

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    if arr.is_empty() {
                        return Err(super::super::utils::error::ConfigError::Validation(
                            format!(
                                "Empty list section '{section_name}' in config '{config_name}' is not supported: its UCI type cannot be determined. To remove a section, omit it from your Nix configuration."
                            ),
                        ));
                    }
                    if !is_valid_uci_type(section_name) {
                        return Err(super::super::utils::error::ConfigError::Validation(
                            format!(
                                "Invalid list identifier '{section_name}' in config '{config_name}': only [a-zA-Z0-9_-] allowed"
                            ),
                        ));
                    }

                    for (idx, item) in arr.iter().enumerate() {
                        if !is_valid_uci_type(&item.section_type) {
                            return Err(super::super::utils::error::ConfigError::Validation(
                                format!(
                                    "Invalid type '{}' in {config_name}.@{section_name}[{idx}]",
                                    item.section_type
                                ),
                            ));
                        }
                        let path = format!("@{section_name}[{idx}]");
                        validate_section_options(&item.options, config_name, &path)?;
                    }
                }
                Section::Named(section) => {
                    if !is_valid_uci_identifier(section_name) {
                        return Err(super::super::utils::error::ConfigError::Validation(
                            format!(
                                "Invalid section '{section_name}' in config '{config_name}': only [a-zA-Z0-9_] allowed (no digits at start)"
                            ),
                        ));
                    }
                    if !is_valid_uci_type(&section.section_type) {
                        return Err(super::super::utils::error::ConfigError::Validation(
                            format!(
                                "Invalid type '{}' in {config_name}.{section_name}",
                                section.section_type
                            ),
                        ));
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
                return Err(super::super::utils::error::ConfigError::Validation(
                    format!("rawUci[{i}] is empty"),
                ));
            }
            if !trimmed.starts_with("uci ") {
                return Err(super::super::utils::error::ConfigError::Validation(
                    format!("rawUci[{i}] must be a 'uci' command, got: {trimmed}"),
                ));
            }
        }
    }

    if let Some(files) = &root.files {
        for (i, file) in files.iter().enumerate() {
            let path = &file.path;
            if !path.starts_with('/') {
                return Err(super::super::utils::error::ConfigError::Validation(
                    format!("files[{i}].path must be absolute, got: {path}"),
                ));
            }
            if path.contains("..") {
                return Err(super::super::utils::error::ConfigError::Validation(
                    format!("files[{i}].path must not contain '..': {path}"),
                ));
            }
            // Empty content allowed: writes a zero-byte file.
        }
    }

    if let Some(packages) = &root.packages {
        for spec in packages {
            // Validates the package name charset at the boundary; a lone `-`
            // or a shell-payload name is a config error, never passed through.
            PackageAction::parse(spec)?;
        }
    }

    if let Some(sources) = &root.package_sources
        && let Some(local) = &sources.local_packages
    {
        for (i, p) in local.iter().enumerate() {
            let file_name = Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    super::super::utils::error::ConfigError::Validation(format!(
                        "package_sources.local_packages[{i}] must be a file path: {p}"
                    ))
                })?;
            RemotePath::tmp(file_name)?;
        }
    }

    for key in &root.ssh_keys {
        if !is_valid_authorized_key(key) {
            return Err(super::super::utils::error::ConfigError::Validation(
                format!(
                    "sshKeys entry is not a valid authorized_keys line (must be a single line, no shell metacharacters): {key}"
                ),
            ));
        }
    }

    if let Some(files) = &root.files {
        for (i, file) in files.iter().enumerate() {
            if let Some(sum) = &file.checksum
                && !is_valid_sha256(sum)
            {
                return Err(super::super::utils::error::ConfigError::Validation(
                    format!(
                        "files[{i}].checksum must be a 64-character hex SHA-256 digest, got: {sum}"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// A valid `authorized_keys` line is a single line (no newline/CR) containing at
/// least two space-separated tokens (keytype + base64 body) and no shell
/// metacharacters. We deliberately do NOT whitelist key types: the set evolves
/// across OpenSSH/Dropbear versions and Dropbear also allows an options prefix
/// (`restrict`, `command="..."`, ...). Rejecting on metacharacters-only blocks
/// injection (newlines, `;`, `&`, `|`, backticks, `$()`) without constraining
/// legitimate keys.
fn is_valid_authorized_key(line: &str) -> bool {
    if line.is_empty() || line.contains(['\n', '\r']) {
        return false;
    }
    if line
        .chars()
        .any(|c| matches!(c, ';' | '&' | '|' | '`' | '$' | '(' | ')' | '<' | '>'))
    {
        return false;
    }
    line.split_whitespace().count() >= 2
}

/// SHA-256 digest: exactly 64 hexadecimal digits (case-insensitive), per
/// NIST FIPS 180-4. The target-side `sha256sum` guard already expects this
/// shape; this rejects malformed values at the config boundary.
fn is_valid_sha256(sum: &str) -> bool {
    sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{Section, SectionData};
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
    fn accepts_hyphenated_config_names() {
        // Real OpenWrt config names (e.g. prometheus-node-exporter-lua) may contain hyphens.
        let root = named_cfg(
            "prometheus-node-exporter-lua",
            "main",
            "prometheus-node-exporter-lua",
            IndexMap::new(),
        );
        assert!(validate_root(&root).is_ok());
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

    fn with_packages(mut root: Root, packages: Vec<String>) -> Root {
        root.packages = Some(packages);
        root
    }

    fn with_local_packages(mut root: Root, paths: Vec<String>) -> Root {
        root.package_sources = Some(crate::config::models::PackageSources {
            opkg_feeds: None,
            apk_repositories: None,
            local_packages: Some(paths),
        });
        root
    }

    #[test]
    fn accepts_valid_package_specs() {
        for specs in [
            vec!["luci".to_string(), "-tcpdump".to_string()],
            vec!["kmod-nf-conntrack".to_string(), "gtk+".to_string()],
            vec!["prometheus-node-exporter-lua".to_string()],
        ] {
            assert!(
                validate_root(&with_packages(empty_root(), specs)).is_ok(),
                "valid specs accepted"
            );
        }
    }

    #[test]
    fn rejects_invalid_package_specs() {
        for bad in [
            "",            // empty spec
            "-",           // lone dash
            "a b",         // space
            "$(touch /x)", // command substitution
            "pkg;rm",      // shell metacharacter
            "../pkg",      // path traversal
            "a'b",         // quote
        ] {
            let root = with_packages(empty_root(), vec![bad.to_string()]);
            let err = validate_root(&root).unwrap_err();
            assert!(
                format!("{err}").contains("Invalid package name"),
                "'{bad}' must be rejected: {err}"
            );
        }
    }

    #[test]
    fn rejects_invalid_local_package_filenames() {
        for bad in [
            "a b.ipk",  // space
            "$(x).ipk", // payload
            "a'b",      // quote
            "a;b.ipk",  // shell metacharacter
            "",         // not a file path
        ] {
            let root = with_local_packages(empty_root(), vec![bad.to_string()]);
            let err = validate_root(&root).unwrap_err();
            assert!(
                format!("{err}").contains("Invalid local package file name")
                    || format!("{err}").contains("must be a file path"),
                "'{bad}' must be rejected: {err}"
            );
        }
    }

    #[test]
    fn accepts_valid_local_package_filenames() {
        for ok in [
            "./packages/luci-app-nlbwmon_0.3-1_all.ipk",
            "build/test_1.0_all.apk",
        ] {
            assert!(
                validate_root(&with_local_packages(empty_root(), vec![ok.to_string()])).is_ok(),
                "'{ok}' should be accepted"
            );
        }
    }

    fn with_ssh_keys(mut root: Root, keys: Vec<String>) -> Root {
        root.ssh_keys = keys;
        root
    }

    fn with_checksum_file(mut root: Root, path: &str, checksum: Option<&str>) -> Root {
        root.files = Some(vec![crate::config::models::File {
            path: path.into(),
            content: crate::config::models::FileContent::Text(String::new()),
            executable: false,
            checksum: checksum.map(|s| s.to_string()),
        }]);
        root
    }

    #[test]
    fn accepts_valid_ssh_keys() {
        for ok in [
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host",
            "ssh-rsa AAAAB3NzaC1yc2E... user@host",
            "restrict,command=\"true\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample",
            "ecdsa-sha2-nistp256 AAAA...",
        ] {
            assert!(
                validate_root(&with_ssh_keys(empty_root(), vec![ok.to_string()])).is_ok(),
                "'{ok}' should be accepted"
            );
        }
    }

    #[test]
    fn rejects_invalid_ssh_keys() {
        for bad in [
            "",
            "ssh-ed25519",
            "ssh-ed25519 AAAA; rm -rf /",
            "ssh-ed25519 AAAA$(id)",
            "ssh-ed25519 AAAA`id`",
            "ssh-ed25519 AAAA\nsecond",
        ] {
            let root = with_ssh_keys(empty_root(), vec![bad.to_string()]);
            let err = validate_root(&root).unwrap_err();
            assert!(
                format!("{err}").contains("authorized_keys"),
                "'{bad}' must be rejected: {err}"
            );
        }
    }

    #[test]
    fn accepts_valid_checksum() {
        let sum = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(validate_root(&with_checksum_file(empty_root(), "/etc/x", Some(sum))).is_ok());
    }

    #[test]
    fn rejects_invalid_checksum() {
        for bad in [
            "abc",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824Z",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b98240",
        ] {
            let root = with_checksum_file(empty_root(), "/etc/x", Some(bad));
            let err = validate_root(&root).unwrap_err();
            assert!(
                format!("{err}").contains("checksum"),
                "'{bad}' must be rejected: {err}"
            );
        }
    }

    #[test]
    fn allows_empty_file_content() {
        use crate::config::models::FileContent;

        let ok = Root {
            files: Some(vec![crate::config::models::File {
                path: "/etc/flag".into(),
                content: FileContent::Text(String::new()),
                executable: false,
                checksum: None,
            }]),
            ..empty_root()
        };
        assert!(validate_root(&ok).is_ok(), "empty file content is allowed");

        let rel = Root {
            files: Some(vec![crate::config::models::File {
                path: "etc/flag".into(),
                content: FileContent::Text("x".into()),
                executable: false,
                checksum: None,
            }]),
            ..empty_root()
        };
        assert_rejects(&rel, "must be absolute");

        let dotdot = Root {
            files: Some(vec![crate::config::models::File {
                path: "/etc/../flag".into(),
                content: FileContent::Text("x".into()),
                executable: false,
                checksum: None,
            }]),
            ..empty_root()
        };
        assert_rejects(&dotdot, "must not contain '..'");
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
