//! Local-package transfer and file-deploy rendering.
//!
//! `render_file_block` renders the remote file-write section; `transfer_packages`
//! ships local packages as a tar through the injected `SshExec` transport.

use crate::utils::helpers::shell_quote;
use std::path::Path;

use crate::config::models::{File, FileContent, Root};
use crate::target::deploy::{DeployConfig, SshExec};
use crate::utils::error::ConfigError;

/// Renders the remote file-write block (per-file `mkdir`, guarded write, mode bits).
pub fn render_file_block(files: &[File]) -> String {
    let mut script = String::new();
    for (i, file) in files.iter().enumerate() {
        let dest = &file.path;
        let dir = Path::new(dest).parent().unwrap_or_else(|| Path::new("/"));
        let dir_str = dir.to_string_lossy();
        // Paths are user-supplied and may contain spaces or single quotes, so
        // every occurrence is shell-quoted (matching how packages are hardened
        // via `RemotePath`). Naked `'{dest}'` would let a quote in a path break
        // out of the argument and inject shell syntax.
        script.push_str(&format!("mkdir -p {}\n", shell_quote(&dir_str)));

        let dest_quoted = shell_quote(dest);
        let tmp_quoted = shell_quote(&format!("{dest}.tmp"));
        // Idempotency guard: skip the rewrite if the existing file already
        // matches the declared checksum. Inside, we write to `dest.tmp`, verify
        // the written bytes (so a corrupted transfer cannot land), then atomically
        // rename — a dropped connection or `set -e` abort mid-write leaves the
        // production path untouched.
        let guard_open = match &file.checksum {
            Some(sum) => format!(
                "if [ \"$(sha256sum {} 2>/dev/null | cut -d' ' -f1)\" != '{}' ]; then\n",
                dest_quoted, sum
            ),
            None => String::new(),
        };
        let indent = if file.checksum.is_some() { "    " } else { "" };
        script.push_str(&guard_open);

        // Busybox on minimal rootfs images may lack base64; text uses a POSIX
        // heredoc, and only explicit base64 payloads are decoded (never re-encoded).
        match &file.content {
            FileContent::Text(text) => {
                script.push_str(&format!(
                    "{indent}cat > {} <<'NUCI_FILE_{i}_EOF'\n",
                    tmp_quoted
                ));
                script.push_str(indent);
                script.push_str(text);
                if !text.ends_with('\n') {
                    script.push('\n');
                }
                script.push_str(&format!("{indent}NUCI_FILE_{i}_EOF\n"));
            }
            FileContent::Base64(b64) => {
                script.push_str(&format!(
                    "{indent}echo '{}' | base64 -d > {}\n",
                    b64, tmp_quoted
                ));
            }
        }

        if let Some(sum) = &file.checksum {
            script.push_str(&format!(
                "{indent}if [ \"$(sha256sum {} | cut -d' ' -f1)\" != '{}' ]; then echo 'checksum mismatch after write: {}' >&2; exit 1; fi\n",
                tmp_quoted, sum, dest
            ));
        }

        if file.executable {
            script.push_str(&format!("{indent}chmod 755 {}\n", tmp_quoted));
        } else {
            script.push_str(&format!("{indent}chmod 644 {}\n", tmp_quoted));
        }
        script.push_str(&format!("{indent}mv -f {} {}\n", tmp_quoted, dest_quoted));

        if file.checksum.is_some() {
            script.push_str("fi\n");
        }
    }
    script
}

/// Bundles local packages into a tar and streams them to `{target}:/tmp/`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::FileContent;

    fn file(path: &str, content: &str) -> File {
        File {
            path: path.to_string(),
            content: FileContent::Text(content.to_string()),
            executable: false,
            checksum: None,
        }
    }

    #[test]
    fn quotes_path_with_single_quote() {
        // A `'` in the path must be escaped inside the argument, not break the
        // surrounding single quotes (which would let it inject shell syntax).
        // `dir` is the parent (`/etc/foo'bar`); the file target is the full path.
        let script = render_file_block(&[file("/etc/foo'bar/baz", "hi")]);
        assert!(
            script.contains("mkdir -p '/etc/foo'\\''bar'"),
            "got: {script}"
        );
        // Writes to a .tmp staging path, then atomically renames into place.
        assert!(
            script.contains("cat > '/etc/foo'\\''bar/baz.tmp' <<'NUCI_FILE_0_EOF'"),
            "got: {script}"
        );
        assert!(
            script.contains("mv -f '/etc/foo'\\''bar/baz.tmp' '/etc/foo'\\''bar/baz'"),
            "got: {script}"
        );
        // Must NOT contain a bare unescaped quote that splits the argument.
        assert!(!script.contains("'/etc/foo'bar/baz"), "got: {script}");
    }

    #[test]
    fn quotes_path_with_space() {
        // `mkdir -p` receives the *parent* dir; the file target keeps the full
        // path. Both must be quoted when they contain spaces.
        let script = render_file_block(&[file("/etc/my dir/rc.local", "x")]);
        assert!(script.contains("mkdir -p '/etc/my dir'"), "got: {script}");
        assert!(
            script.contains("cat > '/etc/my dir/rc.local.tmp'"),
            "got: {script}"
        );
        assert!(
            script.contains("mv -f '/etc/my dir/rc.local.tmp' '/etc/my dir/rc.local'"),
            "got: {script}"
        );
    }

    #[test]
    fn plain_path_renders_unchanged() {
        // No special chars -> output still writes through a .tmp staging file
        // and renames into place (byte-identical result, safer update).
        let script = render_file_block(&[file("/etc/rc.local", "#!/bin/sh")]);
        assert!(script.contains("mkdir -p '/etc'"), "got: {script}");
        assert!(
            script.contains("cat > '/etc/rc.local.tmp'"),
            "got: {script}"
        );
        assert!(
            script.contains("mv -f '/etc/rc.local.tmp' '/etc/rc.local'"),
            "got: {script}"
        );
    }
}
