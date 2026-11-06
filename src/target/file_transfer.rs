//! Local-package transfer and file-deploy rendering.
//!
//! Two responsibilities live here:
//! - `render_file_block`: pure rendering of the remote file-write section of the
//!   deploy script (moved out of `remote_script::build_remote_script`).
//! - `transfer_packages`: bundles declared local packages into a tar stream and
//!   ships it to the target through the injected `SshExec` transport, so it no
//!   longer bypasses the trait by calling the free `ssh_exec` function.

use std::path::Path;

use crate::config::models::{File, FileContent, Root};
use crate::target::deploy::{DeployConfig, SshExec};
use crate::utils::error::ConfigError;

/// Render the remote file-write block: per-file `mkdir`, guarded write (POSIX
/// heredoc for text, `base64 -d` for binary), mode bits, and optional sha256
/// guard. Pure string assembly — no IO.
pub fn render_file_block(files: &[File]) -> String {
    let mut script = String::new();
    for (i, file) in files.iter().enumerate() {
        let dest = &file.path;
        let dir = Path::new(dest).parent().unwrap_or_else(|| Path::new("/"));
        let dir_str = dir.to_string_lossy();
        script.push_str(&format!("mkdir -p '{dir_str}'\n"));

        let guard_open = match &file.checksum {
            Some(sum) => format!(
                "if [ \"$(sha256sum '{dest}' 2>/dev/null | cut -d' ' -f1)\" != '{sum}' ]; then\n"
            ),
            None => String::new(),
        };
        let indent = if file.checksum.is_some() { "    " } else { "" };
        script.push_str(&guard_open);

        // Unrecoverable OpenWrt Busybox limitation: base64 may not be available in
        // minimal rootfs images. Text files use POSIX cat heredoc for zero-dependency.
        // Only explicit base64 payloads use decode to avoid re-encoding.
        match &file.content {
            FileContent::Text(text) => {
                script.push_str(&format!("{indent}cat > '{dest}' <<'NUCI_FILE_{i}_EOF'\n"));
                script.push_str(indent);
                script.push_str(text);
                if !text.ends_with('\n') {
                    script.push('\n');
                }
                script.push_str(&format!("{indent}NUCI_FILE_{i}_EOF\n"));
            }
            FileContent::Base64(b64) => {
                script.push_str(&format!("{indent}echo '{b64}' | base64 -d > '{dest}'\n"));
            }
        }

        if file.executable {
            script.push_str(&format!("{indent}chmod 755 '{dest}'\n"));
        } else {
            script.push_str(&format!("{indent}chmod 644 '{dest}'\n"));
        }

        if file.checksum.is_some() {
            script.push_str("fi\n");
        }
    }
    script
}

/// Bundle declared local packages into a tar and stream them to
/// `{target}:/tmp/` through the injected `SshExec` transport. Uses the trait
/// (not the free `ssh_exec`) so test fakes are honored and the transport stays
/// swappable.
pub fn transfer_packages(
    json_path: &Path,
    target: &str,
    root: &Root,
    config: &DeployConfig,
    ssh: &dyn SshExec,
) -> Result<(), ConfigError> {
    let local_pkgs = match &root.package_sources {
        Some(sources) => match &sources.local_packages {
            Some(pkgs) => pkgs,
            None => return Ok(()),
        },
        None => return Ok(()),
    };

    if local_pkgs.is_empty() {
        return Ok(());
    }

    let staging = tempfile::tempdir()?;
    let base_dir = json_path.parent().unwrap_or_else(|| Path::new("."));
    for pkg in local_pkgs {
        let path = Path::new(pkg);
        let resolved = if path.is_relative() {
            let candidate = base_dir.join(path);
            if candidate.exists() {
                candidate
            } else {
                path.to_path_buf()
            }
        } else {
            path.to_path_buf()
        };
        if !resolved.exists() {
            return Err(ConfigError::Validation(format!(
                "Local package not found: {pkg}"
            )));
        }
        let filename = resolved.file_name().and_then(|n| n.to_str()).unwrap_or(pkg);
        std::fs::copy(&resolved, staging.path().join(filename))?;
    }

    eprintln!("Bundling {} local package(s)...", local_pkgs.len());
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        builder
            .append_dir_all(".", staging.path())
            .map_err(|e| ConfigError::Deploy(format!("Failed to build tar archive: {e}")))?;
        builder
            .finish()
            .map_err(|e| ConfigError::Deploy(format!("Failed to finalize tar archive: {e}")))?;
    }

    eprintln!("Transferring to {target}:/tmp/ via SSH stream...");
    ssh.exec(target, "tar -xf - -C /tmp", Some(&tar_bytes), config)?;
    Ok(())
}
