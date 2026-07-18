use crate::error::ConfigError;
use crate::generator::{serialize_package_management, serialize_uci};
use crate::models::{PkgBackend, Root};
use crate::secrets::{decrypt_sops_mem, load_secrets_dir, resolve_secrets};
use crate::validation::validate_root;
use std::collections::HashMap;
use std::path::Path;

pub(crate) struct CompiledConfig {
    pub(crate) uci_batch: String,
    pub(crate) resolved_root: Root,
    pub(crate) secrets: HashMap<String, String>,
}

pub(crate) fn compile_config(
    json_path: &Path,
    secrets_dir: Option<&Path>,
    skip_sops: bool,
) -> Result<CompiledConfig, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: Root = serde_json::from_reader(std::io::BufReader::new(file))?;
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

    // Escape hatch: raw `uci` lines the typed model can't express (rename,
    // reorder, deletes, etc.). Emitted verbatim, after the typed batch so the
    // model's set/del ordering still applies first.
    //
    // Before emitting, ensure any config files referenced by `uci set`/`uci add`
    // exist — UCI won't auto-create them, and a missing file causes silent
    // failure (the rawUci line executes but leaves no trace on the target).
    if let Some(raw) = &resolved_root.raw_uci {
        if !raw.is_empty() {
            // Collect config names from rawUci lines that reference them.
            let mut needed: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for line in raw {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("uci set ").or_else(|| trimmed.strip_prefix("uci add ")) {
                    // rest = "config_name.section..." or "config_name"
                    if let Some(cfg) = rest.split('.').next() {
                        if !cfg.is_empty() {
                            needed.insert(cfg);
                        }
                    }
                }
            }
            // Emit touch lines for each needed config.
            if !needed.is_empty() {
                uci_batch.push_str("\n# Ensure config files exist for raw UCI lines below\n");
                for cfg in &needed {
                    uci_batch.push_str(&format!("echo 'config system' > /etc/config/{}\n", cfg));
                }
            }
        }
    }

    if let Some(raw) = &resolved_root.raw_uci {
        if !raw.is_empty() {
            uci_batch.push_str("\n# Raw UCI escape hatch (verbatim)\n");
            for line in raw {
                uci_batch.push_str(line.trim_end());
                uci_batch.push('\n');
            }
        }
    }

    let backend = PkgBackend::from_str(&resolved_root.package_manager);
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
        // rawUci must come after the typed batch's header.
        let raw_pos = out.uci_batch.find("uci rename").unwrap();
        let typed_pos = out.uci_batch.find("add system system").unwrap();
        assert!(raw_pos > typed_pos, "rawUci should follow typed uci batch");
    }

    #[test]
    fn raw_uci_absent_when_not_declared() {
        let json = write_json(r#"{ "packageManager": "opkg", "settings": {} }"#);
        let out = compile_config(json.path(), None, true).unwrap();
        assert!(!out.uci_batch.contains("Raw UCI escape hatch"));
    }
}
