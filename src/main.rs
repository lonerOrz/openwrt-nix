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
    #[command(about = "Compile a config JSON into a deployable UCI/package/file plan (no SSH)")]
    Compile {
        #[arg(help = "Path to the source UCI config JSON")]
        json: PathBuf,
        #[arg(short, long, help = "Directory of per-key SOPS-encrypted secret files")]
        secrets_dir: Option<PathBuf>,
        #[arg(long, help = "Skip SOPS decryption (compile without secrets)")]
        no_sops: bool,
        #[arg(long, help = "Also emit package-management commands in the plan")]
        with_packages: bool,
    },

    #[command(about = "Deploy a config JSON to a target router over SSH with rollback watchdog")]
    Deploy {
        #[arg(help = "Path to the source UCI config JSON")]
        json: PathBuf,
        #[arg(short, long, help = "Target host (user@host) to deploy to")]
        target: String,
        #[arg(
            short,
            long,
            default_value_t = 22,
            overrides_with = "port",
            help = "SSH port of the target [default: 22]"
        )]
        port: u16,
        #[arg(
            short,
            long,
            overrides_with = "identity",
            help = "SSH identity (private key) file path"
        )]
        identity: Option<PathBuf>,
        #[arg(short, long, help = "Directory of per-key SOPS-encrypted secret files")]
        secrets_dir: Option<PathBuf>,
        #[arg(short, long, help = "Force apply even if target already matches")]
        force: bool,
        #[arg(short, long, help = "Skip SOPS decryption")]
        no_sops: bool,
        #[arg(
            long,
            default_value_t = 60,
            overrides_with = "watchdog_timeout",
            help = "Watchdog timeout in seconds before rollback triggers [default: 60]"
        )]
        watchdog_timeout: u64,
        #[arg(
            short,
            long,
            overrides_with = "verbose",
            help = "Verbose: per-stage banners and remote set -x tracing"
        )]
        verbose: bool,
    },

    #[command(
        about = "Show the diff between the local config and the live target state (no changes applied)"
    )]
    Diff {
        #[arg(help = "Path to the source UCI config JSON")]
        json: PathBuf,
        #[arg(short, long, help = "Target host (user@host) to compare against")]
        target: String,
        #[arg(
            short,
            long,
            default_value_t = 22,
            overrides_with = "port",
            help = "SSH port of the target [default: 22]"
        )]
        port: u16,
        #[arg(
            short,
            long,
            overrides_with = "identity",
            help = "SSH identity (private key) file path"
        )]
        identity: Option<PathBuf>,
        #[arg(short, long, help = "Directory of per-key SOPS-encrypted secret files")]
        secrets_dir: Option<PathBuf>,
        #[arg(long, help = "Skip SOPS decryption")]
        no_sops: bool,
        #[arg(
            short,
            long,
            overrides_with = "verbose",
            help = "Verbose: per-stage banners and remote set -x tracing"
        )]
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
            with_packages,
        }) => {
            run_compile(&json, secrets_dir.as_deref(), no_sops, with_packages);
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

fn run_compile(json_path: &Path, secrets_dir: Option<&Path>, skip_sops: bool, with_packages: bool) {
    match compile_config(json_path, secrets_dir, skip_sops, None) {
        Ok(compiled) => {
            // Default: emit only the UCI batch. `nuci compile` output is consumed
            // verbatim as a shell script by `99-nuci-bootstrap` (firmware builds),
            // which must not contain package-manager commands — those are baked
            // into the image by the imagebuilder and would fail at first boot.
            // `--with-packages` adds the package operations for human/CI audit.
            print!("{}", compiled.uci_batch);
            if with_packages && !compiled.package_commands.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deploy(args: &[&str]) -> (u16, Option<String>, bool, u64) {
        let mut argv = vec!["nuci", "deploy", "cfg.json", "--target", "root@host"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().command {
            Some(Command::Deploy {
                port,
                identity,
                verbose,
                watchdog_timeout,
                ..
            }) => (
                port,
                identity.map(|p| p.to_string_lossy().into_owned()),
                verbose,
                watchdog_timeout,
            ),
            _ => panic!("expected a deploy command"),
        }
    }

    /// A repeated flag must resolve to the last occurrence, so a CLI value can
    /// override the same flag injected by the Nix module.
    #[test]
    fn repeated_flags_resolve_to_the_last_occurrence() {
        let (port, identity, verbose, _) = deploy(&[
            "--port",
            "1111",
            "--port",
            "2222",
            "--identity",
            "/a",
            "--identity",
            "/b",
        ]);
        assert_eq!(port, 2222);
        assert_eq!(identity.as_deref(), Some("/b"));
        assert!(!verbose);

        let (port, _, _, timeout) =
            deploy(&["--watchdog-timeout", "10", "--watchdog-timeout", "99"]);
        assert_eq!(port, 22);
        assert_eq!(timeout, 99);
    }

    /// The Nix module injects its flags before the CLI passthrough, so the CLI
    /// value must win. This mirrors the generated script's argument order.
    #[test]
    fn cli_value_overrides_the_module_injected_one() {
        let (port, identity, verbose, timeout) = deploy(&[
            // module-injected
            "--port",
            "2222",
            "--identity",
            "/tmp/from-module",
            "--watchdog-timeout",
            "60",
            // CLI passthrough
            "--port",
            "3333",
            "--identity",
            "/tmp/from-cli",
            "--watchdog-timeout",
            "90",
        ]);
        assert_eq!(port, 3333);
        assert_eq!(identity.as_deref(), Some("/tmp/from-cli"));
        assert_eq!(timeout, 90);
        assert!(!verbose);
    }

    /// A repeated boolean flag stays enabled instead of erroring out.
    #[test]
    fn repeated_bool_flag_stays_enabled() {
        let (_, _, verbose, _) = deploy(&["--verbose", "--verbose"]);
        assert!(verbose);
    }

    /// Without a repeat, the defaults still apply.
    #[test]
    fn defaults_apply_when_flags_are_absent() {
        let (port, identity, verbose, timeout) = deploy(&[]);
        assert_eq!(port, 22);
        assert_eq!(identity, None);
        assert!(!verbose);
        assert_eq!(timeout, 60);
    }
}
