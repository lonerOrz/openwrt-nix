mod deploy;
mod error;
mod generator;
mod helpers;
mod models;
mod secrets;
mod validation;

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use error::ConfigError;
use generator::{serialize_package_management, serialize_uci};
use models::{PkgBackend, Root};
use secrets::{load_secrets_dir, resolve_secrets};
use validation::validate_root;

#[derive(Parser)]
#[command(
    name = "nuci",
    about = "Declarative OpenWrt UCI configuration compiler and deployer"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Compile Nix JSON config into UCI batch commands
    Compile {
        /// Path to the JSON config file
        json: PathBuf,

        /// Optional directory containing secrets files
        secrets_dir: Option<PathBuf>,
    },

    /// Deploy configuration to a remote OpenWrt device via SSH
    Deploy {
        /// Path to the JSON config file
        json: PathBuf,

        /// SSH target host (user@host)
        #[arg(short, long)]
        target: String,

        /// SSH port
        #[arg(short, long, default_value_t = 22)]
        port: u16,

        /// Path to SSH identity file
        #[arg(short, long)]
        identity: Option<PathBuf>,
    },
}

fn compile(path: &PathBuf, secrets_dir: Option<&Path>) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let root: Root = serde_json::from_reader(reader)?;
    validate_root(&root)?;

    // Decrypt SOPS files embedded in the JSON (if any)
    let mut secrets = secrets::decrypt_sops_mem(&root)?;
    // Merge with directory-based secrets (if provided)
    if let Some(dir_path) = secrets_dir {
        secrets.extend(load_secrets_dir(dir_path.to_str().unwrap())?);
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    let mut output_buffer = String::with_capacity(4096);
    serialize_uci(&mut output_buffer, &resolved_root.settings)?;

    let backend = PkgBackend::from_str(&resolved_root.package_manager);
    serialize_package_management(
        &mut output_buffer,
        backend,
        resolved_root.package_sources.as_ref(),
        resolved_root.packages.as_deref(),
    )?;

    Ok(output_buffer)
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Compile { json, secrets_dir }) => {
            run_compile(&json, secrets_dir.as_deref());
        }
        Some(Command::Deploy {
            json,
            target,
            port,
            identity,
        }) => {
            let config = deploy::DeployConfig {
                port,
                identity_file: identity.map(|p| p.to_string_lossy().into_owned()),
            };
            if let Err(e) = deploy::run(&json, &target, &config) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        None => {
            eprintln!(
                "USAGE:\n  nuci compile <JSON_FILE> [SECRETS_DIR]\n  nuci deploy <JSON_FILE> --target <HOST> [--port PORT] [--identity FILE]"
            );
            std::process::exit(1);
        }
    }
}

fn run_compile(json_path: &PathBuf, secrets_dir: Option<&Path>) {
    match compile(json_path, secrets_dir) {
        Ok(output) => print!("{output}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn convert_file_full() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {
                "system": {
                    "system": { "_type": "system", "hostname": "test" }
                }
            }
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("delete system.system"));
        assert!(output.contains("set system.system.hostname='test'"));
        assert!(output.contains("commit system"));
    }

    #[test]
    fn convert_file_with_secrets() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        let secrets_path = dir.path().join("secrets");
        fs::create_dir(&secrets_path).unwrap();
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {
                "wifi": {
                    "radio0": { "_type": "wifi-iface", "key": "@wifi_pass@" }
                }
            }
        }"#,
        )
        .unwrap();
        fs::write(secrets_path.join("s.json"), r#"{"wifi_pass": "secret123"}"#).unwrap();
        let output = compile(&json_path, Some(&secrets_path)).unwrap();
        assert!(output.contains("set wifi.radio0.key='secret123'"));
    }

    #[test]
    fn convert_file_missing_file() {
        let err = compile(&PathBuf::from("/tmp/nonexistent_xyz.json"), None).unwrap_err();
        assert!(err.0.contains("No such file"));
    }

    #[test]
    fn convert_file_invalid_json() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("bad.json");
        fs::write(&json_path, "not json").unwrap();
        let err = compile(&json_path, None).unwrap_err();
        assert!(err.0.contains("Failed to parse JSON"));
    }

    #[test]
    fn convert_file_opkg_feeds() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {},
            "packageSources": { "feeds": ["src/gz custom https://example.com/repo"] }
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("printf '' > /etc/opkg/customfeeds.conf"));
        assert!(output.contains("src/gz custom https://example.com/repo"));
    }

    #[test]
    fn convert_file_packages() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {},
            "packages": ["luci", "tcpdump"]
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("for pkg in luci tcpdump"));
        assert!(output.contains("opkg update && opkg install luci tcpdump"));
    }

    #[test]
    fn convert_file_local_packages() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {},
            "packageSources": { "localPackages": ["./pkg/foo_1.0.ipk"] }
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("opkg list-installed \"foo\""));
        assert!(output.contains("opkg install /tmp/foo_1.0.ipk"));
    }

    #[test]
    fn convert_file_feed_single_quote_escaping() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "opkg",
            "settings": {},
            "packageSources": { "feeds": ["src/gz test it's a feed"] }
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("'\\''"));
    }

    #[test]
    fn convert_file_apk_backend() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "packageManager": "apk",
            "settings": {},
            "packages": ["luci"],
            "packageSources": {
                "feeds": ["https://example.com/packages"],
                "localPackages": ["./pkg/foo_1.0_all.apk"]
            }
        }"#,
        )
        .unwrap();
        let output = compile(&json_path, None).unwrap();
        assert!(output.contains("/etc/apk/repositories.d/customfeeds.list"));
        assert!(output.contains("apk -U add"));
        assert!(output.contains("apk add --allow-untrusted /tmp/foo_1.0_all.apk"));
    }
}
