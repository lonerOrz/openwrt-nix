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
) -> Result<CompiledConfig, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    validate_root(&root)?;

    let mut secrets = decrypt_sops_mem(&root)?;

    if let Some(dir) = secrets_dir {
        secrets.extend(load_secrets_dir(dir.to_str().ok_or_else(|| {
            ConfigError::Validation("Invalid secrets directory path".into())
        })?)?);
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    let mut uci_batch = String::with_capacity(4096);
    serialize_uci(&mut uci_batch, &resolved_root.settings)?;

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
