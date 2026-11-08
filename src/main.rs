use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use nuci::compile::pipeline::compile_config;
use nuci::target::{deploy, diff};

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
    Compile {
        json: PathBuf,
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,
        #[arg(long)]
        no_sops: bool,
    },

    Deploy {
        json: PathBuf,
        #[arg(short, long)]
        target: String,
        #[arg(short, long, default_value_t = 22)]
        port: u16,
        #[arg(short, long)]
        identity: Option<PathBuf>,
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,
        #[arg(short, long)]
        force: bool,
        #[arg(short, long)]
        no_sops: bool,
        #[arg(long, default_value_t = 60)]
        watchdog_timeout: u64,
        #[arg(short, long)]
        verbose: bool,
    },

    Diff {
        json: PathBuf,
        #[arg(short, long)]
        target: String,
        #[arg(short, long, default_value_t = 22)]
        port: u16,
        #[arg(short, long)]
        identity: Option<PathBuf>,
        #[arg(short, long)]
        secrets_dir: Option<PathBuf>,
        #[arg(long)]
        no_sops: bool,
        #[arg(short, long)]
        verbose: bool,
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
            verbose,
        }) => {
            let config = deploy::DeployConfig {
                port,
                identity_file: identity.map(|p| p.to_string_lossy().into_owned()),
                force,
                no_sops,
                watchdog_timeout,
                verbose,
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
            verbose,
        }) => {
            let config = deploy::DeployConfig {
                port,
                identity_file: identity.map(|p| p.to_string_lossy().into_owned()),
                force: false,
                no_sops,
                watchdog_timeout: 60,
                verbose,
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
    match compile_config(json_path, secrets_dir, skip_sops, None) {
        Ok(compiled) => {
            // UCI commands run first (per-command set -e, per-config commit).
            print!("{}", compiled.uci_batch);
            // Package-manager commands run after the UCI "apply done" sentinel on
            // the target, so they must be visible here too — a `compile` that only
            // printed UCI would hide every feed/install/remove operation.
            if !compiled.package_commands.is_empty() {
                println!(
                    "\n# Package-manager operations (run after UCI apply-done sentinel)\n{}",
                    compiled.package_commands
                );
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
