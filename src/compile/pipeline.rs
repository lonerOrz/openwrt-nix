use crate::compile::generator::{serialize_package_management, serialize_uci};
use crate::compile::secrets::{decrypt_sops_mem, load_secrets_dir, merge_secrets, resolve_secrets};
use crate::config::models::PkgBackend;
use crate::config::validation::validate_root;
use crate::utils::error::ConfigError;
use std::collections::HashMap;
use std::path::Path;

pub struct CompiledConfig {
    pub uci_batch: String,
    pub resolved_root: crate::config::models::Root,
    pub secrets: HashMap<String, String>,
}

impl std::fmt::Debug for CompiledConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledConfig")
            .field("uci_batch_len", &self.uci_batch.len())
            .finish()
    }
}

/// Declared `packageManager` hint plus whether the config actually emits any
/// package-manager commands (packages or package sources declared). Used to decide
/// whether runtime capability discovery is required before compiling.
pub struct PackageHint {
    pub declared: String,
    pub needs_pm: bool,
}

pub fn read_package_manager_hint(json_path: &Path) -> Result<PackageHint, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: crate::config::models::Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    let needs_pm = root.packages.is_some() || root.package_sources.is_some();
    Ok(PackageHint {
        declared: root.package_manager,
        needs_pm,
    })
}

pub fn compile_config(
    json_path: &Path,
    secrets_dir: Option<&Path>,
    skip_sops: bool,
    backend: Option<PkgBackend>,
) -> Result<CompiledConfig, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: crate::config::models::Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    validate_root(&root)?;

    let mut secrets = if skip_sops {
        HashMap::new()
    } else {
        decrypt_sops_mem(&root)?
    };

    if let Some(dir) = secrets_dir {
        let dir_secrets =
            load_secrets_dir(dir.to_str().ok_or_else(|| {
                ConfigError::Validation("Invalid secrets directory path".into())
            })?)?;
        merge_secrets(&mut secrets, dir_secrets)?;
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    let mut uci_batch = String::with_capacity(4096);
    serialize_uci(&mut uci_batch, &resolved_root.settings)?;

    if let Some(raw) = &resolved_root.raw_uci
        && !raw.is_empty()
    {
        let mut needed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in raw {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("uci set ") {
                if let Some(cfg) = rest.split('.').next().filter(|c| !c.is_empty()) {
                    needed.insert(cfg);
                }
            } else if let Some(rest) = trimmed.strip_prefix("uci add ") {
                // `uci add <config> <type>` — config is the first whitespace token
                if let Some(cfg) = rest.split_whitespace().next().filter(|c| !c.is_empty()) {
                    needed.insert(cfg);
                }
            }
        }

        if !needed.is_empty() {
            uci_batch.push_str("\n# Ensure config files exist for raw UCI lines below\n");
            for cfg in &needed {
                uci_batch.push_str(&format!("touch /etc/config/{}\n", cfg));
            }
        }

        uci_batch.push_str("\n# Raw UCI escape hatch (verbatim)\n");
        for line in raw {
            uci_batch.push_str(line.trim_end());
            uci_batch.push('\n');
        }
    }

    let backend = match backend {
        Some(b) => b,
        None => PkgBackend::try_from(resolved_root.package_manager.as_str())?,
    };
    serialize_package_management(
        &mut uci_batch,
        backend,
        resolved_root.package_sources.as_ref(),
        resolved_root.packages.as_deref(),
    )?;

    Ok(CompiledConfig {
        uci_batch,
        resolved_root,
        secrets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_json(s: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f
    }

    #[test]
    fn raw_uci_emitted_verbatim_after_typed_batch() {
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "system": { "system": [ { "_type": "system", "hostname": "rauter" } ] }
                },
                "rawUci": [ "uci rename system.@system[0]=sys0" ]
            }"#,
        );
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.contains("uci rename system.@system[0]=sys0"));
        let raw_pos = out.uci_batch.find("uci rename").unwrap();
        let typed_pos = out.uci_batch.find("add system system").unwrap();
        assert!(raw_pos > typed_pos, "rawUci should follow typed uci batch");
    }

    #[test]
    fn raw_uci_add_touches_single_config() {
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": [ "uci add dropbear dropbear", "uci set network.lan.proto='static'" ]
            }"#,
        );
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.contains("touch /etc/config/dropbear\n"));
        assert!(out.uci_batch.contains("touch /etc/config/network\n"));
        assert!(
            !out.uci_batch
                .contains("touch /etc/config/dropbear dropbear")
        );
    }

    #[test]
    fn raw_uci_absent_when_not_declared() {
        let json = write_json(r#"{ "packageManager": "opkg", "settings": {} }"#);
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(!out.uci_batch.contains("Raw UCI escape hatch"));
    }

    #[test]
    fn unknown_package_manager_is_rejected() {
        let json = write_json(r#"{ "packageManager": "apt", "settings": {} }"#);
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        assert!(
            format!("{err}").contains("Unsupported package manager: apt"),
            "expected rejection, got: {err}"
        );
    }

    #[test]
    fn default_package_manager_is_opkg() {
        // packageManager omitted -> opkg default, compiles without error.
        let json = write_json(r#"{ "settings": {} }"#);
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.is_empty());
    }

    #[test]
    fn secret_interpolation_whitelist_e2e() {
        // Drives `@name@` resolution through compile_config using a secrets dir,
        // proving rawUci / files.text / sshKeys are interpolated while settings
        // and feeds already were. Verifies the whitelist end-to-end.
        let dir = tempfile::tempdir().unwrap();
        let secret_path = dir.path().join("secrets.json");
        std::fs::write(
            &secret_path,
            r#"{ "wan_user": "alice", "api_token": "s3cr3t", "pubkey": "AAAAC3NzaC1lZDI1NTE5AAAAIExample" }"#,
        )
        .unwrap();

        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "network": { "wan": { "_type": "interface", "username": "@wan_user@" } }
                },
                "rawUci": [ "uci set network.wan.password='@wan_user@'" ],
                "files": [
                    { "path": "/etc/app/conf", "content": "token=@api_token@" }
                ],
                "sshKeys": [ "ssh-ed25519 @pubkey@ user@host" ]
            }"#,
        );

        let out = compile_config(json.path(), Some(dir.path()), true, None).unwrap();

        // rawUci interpolated
        assert!(
            out.uci_batch
                .contains("uci set network.wan.password='alice'"),
            "rawUci should interpolate: {}",
            out.uci_batch
        );
        // settings interpolated
        assert!(out.uci_batch.contains("set network.wan.username='alice'"));
        // sshKeys interpolated in the resolved root
        assert_eq!(
            out.resolved_root.ssh_keys[0],
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host"
        );
        // files.text interpolated in the resolved root
        match &out.resolved_root.files.unwrap()[0].content {
            crate::config::models::FileContent::Text(s) => assert_eq!(s, "token=s3cr3t"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn secrets_dir_cross_file_conflict_errors() {
        // Two files in the secrets dir claim the same key with different values.
        // The merge into the (skipped) SOPS map must reject, not silently keep
        // one value. No sops binary needed (skip_sops = true).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("b.json"), r#"{"wifi_pass": "beta"}"#).unwrap();

        let json = write_json(r#"{ "packageManager": "opkg", "settings": {} }"#);
        let err = compile_config(json.path(), Some(dir.path()), true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("conflicts"), "expected conflict, got: {msg}");
        assert!(msg.contains("wifi_pass"), "expected key name, got: {msg}");
        // Plaintext must never leak into the error message.
        assert!(!msg.contains("alpha"), "plaintext leaked: {msg}");
        assert!(!msg.contains("beta"), "plaintext leaked: {msg}");
    }

    #[test]
    fn secrets_dir_same_value_idempotent_merge_ok() {
        // Same key with identical value across two files -> no conflict.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("b.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();

        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "wireless": { "radio0": { "_type": "wifi-iface", "key": "@wifi_pass@" } }
                }
            }"#,
        );
        let out = compile_config(json.path(), Some(dir.path()), true, None).unwrap();
        assert!(out.uci_batch.contains("set wireless.radio0.key='alpha'"));
    }
}
