use crate::compile::generator::{serialize_package_management, serialize_uci};
use crate::compile::secrets::{decrypt_sops_mem, load_secrets_dir, resolve_secrets};
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

pub fn compile_config(
    json_path: &Path,
    secrets_dir: Option<&Path>,
    skip_sops: bool,
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
        secrets.extend(load_secrets_dir(dir.to_str().ok_or_else(|| {
            ConfigError::Validation("Invalid secrets directory path".into())
        })?)?);
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

    let backend = PkgBackend::from_name(&resolved_root.package_manager);
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
        let out = compile_config(json.path(), None, true).unwrap();
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
        let out = compile_config(json.path(), None, true).unwrap();
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
        let out = compile_config(json.path(), None, true).unwrap();
        assert!(!out.uci_batch.contains("Raw UCI escape hatch"));
    }
}
