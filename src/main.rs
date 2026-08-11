use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use nuci::deploy;
use nuci::diff;
use nuci::pipeline::compile_config;

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

        /// Directory containing secrets files
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,

        /// Skip SOPS decryption (secrets must be in secrets_dir as plain JSON)
        #[arg(long)]
        no_sops: bool,
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

        /// Directory containing secrets files
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,

        /// Force deployment even if configuration is already up-to-date
        #[arg(short, long)]
        force: bool,

        /// Skip SOPS decryption (secrets must be in secrets_dir as plain JSON)
        #[arg(long)]
        no_sops: bool,

        /// Watchdog timeout in seconds before rollback triggers
        #[arg(long, default_value_t = 60)]
        watchdog_timeout: u64,
    },

    /// Preview differences between target and compiled config (read-only)
    Diff {
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

        /// Directory containing secrets files
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,

        /// Skip SOPS decryption (secrets must be in secrets_dir as plain JSON)
        #[arg(long)]
        no_sops: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Compile {
            json,
            secrets_dir,
            no_sops,
        }) => {
            run_compile(&json, secrets_dir.as_deref(), no_sops);
        }
        Some(Command::Deploy {
            json,
            target,
            port,
            identity,
            secrets_dir,
            force,
            no_sops,
            watchdog_timeout,
        }) => {
            let config = deploy::DeployConfig {
                port,
                identity_file: identity.map(|p| p.to_string_lossy().into_owned()),
                force,
                no_sops,
                watchdog_timeout,
            };
            if let Err(e) = deploy::run(
                &json,
                &target,
                &config,
                secrets_dir.as_deref(),
                &deploy::RealSsh,
            ) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some(Command::Diff {
            json,
            target,
            port,
            identity,
            secrets_dir,
            no_sops,
        }) => {
            let config = deploy::DeployConfig {
                port,
                identity_file: identity.map(|p| p.to_string_lossy().into_owned()),
                force: false,
                no_sops,
                watchdog_timeout: 60,
            };
            if let Err(e) = diff::run(
                &json,
                &target,
                &config,
                secrets_dir.as_deref(),
                &deploy::RealSsh,
            ) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        None => {
            eprintln!(
                "USAGE:\n  nuci compile <JSON_FILE> [OPTIONS]\n  nuci diff <JSON_FILE> --target <HOST> [OPTIONS]\n  nuci deploy <JSON_FILE> --target <HOST> [OPTIONS]\nRun `nuci <SUBCOMMAND> --help` for details."
            );
            std::process::exit(1);
        }
    }
}

fn run_compile(json_path: &Path, secrets_dir: Option<&Path>, skip_sops: bool) {
    match compile_config(json_path, secrets_dir, skip_sops).map(|c| c.uci_batch) {
        Ok(output) => print!("{output}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
