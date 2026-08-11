use super::target::Target;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use tokio::sync::OnceCell;

/// Session-scoped artifacts shared across all integration tests.
pub struct SessionArtifacts {
    pub ssh_key: PathBuf,
    pub sops_key_dir: PathBuf,
    pub opkg_json: PathBuf,
    pub apk_json: PathBuf,
}

static ARTIFACTS: OnceLock<SessionArtifacts> = OnceLock::new();
static OPKG_TARGET: OnceCell<Option<Target>> = OnceCell::const_new();
static APK_TARGET: OnceCell<Option<Target>> = OnceCell::const_new();

/// Cleanup hook registered via atexit — removes dynamic test artifacts.
extern "C" fn cleanup_session_artifacts() {
    eprintln!("DEBUG[cleanup]: HOOK CALLED!");
    let _ = std::fs::write("/tmp/nuci_cleanup_ran", "yes");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Remove dynamically generated SOPS-encrypted secrets file.
    let _ = std::fs::remove_file(manifest_dir.join("tests/secrets.enc.json"));

    // Restore Nix configs that were modified to inject SSH keys and rawUci.
    let _ = Command::new("git")
        .args([
            "restore",
            "tests/test_config.nix",
            "tests/test_config_apk.nix",
        ])
        .status();
}

pub fn get_session_artifacts() -> &'static SessionArtifacts {
    ARTIFACTS.get_or_init(|| {
        // Register atexit hook on first access.
        eprintln!("DEBUG: registering atexit hook");
        unsafe { libc::atexit(cleanup_session_artifacts); }

        let session_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ssh_key = PathBuf::from(format!("/tmp/nuci_key_{session_id}"));
        let sops_key_dir = PathBuf::from(format!("/tmp/nuci_sops_{session_id}"));
        let encrypted_secrets = manifest_dir.join("tests/secrets.enc.json");

        // 1. Generate SSH keypair.
        if !ssh_key.exists() {
            let out = Command::new("ssh-keygen")
                .args(["-t", "ed25519", "-N", "", "-f", ssh_key.to_str().unwrap(), "-C", "openwrt-test", "-q"])
                .output().expect("ssh-keygen failed");
            assert!(out.status.success(), "ssh-keygen failed");
        }
        let pub_key = std::fs::read_to_string(ssh_key.with_extension("pub")).expect("read pub key");

        // 2. Generate SOPS/age keypair and encrypt secrets.
        std::fs::create_dir_all(&sops_key_dir).unwrap();
        let age_key_file = sops_key_dir.join("keys.txt");
        if !age_key_file.exists() {
            let out = Command::new("age-keygen")
                .output()
                .or_else(|_| {
                    Command::new("nix")
                        .args(["shell", "nixpkgs#age", "-c", "age-keygen"])
                        .output()
                })
                .expect("Failed to run age-keygen");

            assert!(out.status.success(), "age-keygen failed: {}", String::from_utf8_lossy(&out.stderr));
            std::fs::write(&age_key_file, &out.stdout).unwrap();
        }
        let keys_content = std::fs::read_to_string(&age_key_file).unwrap();
        let age_pubkey = keys_content
            .split_whitespace()
            .find(|s| s.starts_with("age1"))
            .expect("Failed to extract age public key")
            .to_string();

        let _ = Command::new("sops")
            .env("SOPS_AGE_KEY_FILE", &age_key_file)
            .args([
                "--config", "/dev/null", "--encrypt", "--age", &age_pubkey,
                "--input-type", "json", "--output-type", "json",
                "--output", encrypted_secrets.to_str().unwrap(),
                manifest_dir.join("tests/mock_secrets/secrets.json").to_str().unwrap(),
            ])
            .status()
            .or_else(|_| {
                Command::new("nix")
                    .env("SOPS_AGE_KEY_FILE", &age_key_file)
                    .args([
                        "shell", "nixpkgs#sops", "-c", "sops",
                        "--config", "/dev/null",
                        "--encrypt", "--age", &age_pubkey,
                        "--input-type", "json", "--output-type", "json",
                        "--output", encrypted_secrets.to_str().unwrap(),
                        manifest_dir.join("tests/mock_secrets/secrets.json").to_str().unwrap(),
                    ])
                    .status()
            });

        // 3. Inject SSH key + rawUci escape hatch into Nix test configs.
        for cfg in ["tests/test_config.nix", "tests/test_config_apk.nix"] {
            let cfg_path = manifest_dir.join(cfg);
            if let Ok(content) = std::fs::read_to_string(&cfg_path) {
                let updated = content.replace(
                    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAvctZwmsE8Bxt0WYnHZAdRKERk0YKwwidsG32rY6cf2 openwrt-test",
                    pub_key.trim(),
                );
                let updated = if !updated.contains("nuci_test.marker") {
                    updated.replace(
                        "uci.sshKeys = [",
                        "uci.rawUci = [ \"uci set nuci_test.marker=escaped\" \"uci commit nuci_test\" ];\n  uci.sshKeys = [",
                    )
                } else {
                    updated
                };
                std::fs::write(&cfg_path, updated).unwrap();
            }
        }

        // 4. Build Nix JSON configs.
        let build_json = |attr: &str| -> PathBuf {
            let out = Command::new("nix")
                .args(["build", &format!("path:.#{attr}"), "--print-out-paths", "--no-link"])
                .output().expect("nix build failed");
            PathBuf::from(String::from_utf8(out.stdout).expect("invalid utf8").trim().to_string())
        };
        let opkg_json = build_json("test-json");
        let apk_json = build_json("test-json-apk");

        SessionArtifacts { ssh_key, sops_key_dir, opkg_json, apk_json }
    })
}

/// Returns the path to the SOPS age key file.
pub fn sops_key_file() -> PathBuf {
    get_session_artifacts().sops_key_dir.join("keys.txt")
}

/// Returns the shared opkg test container, lazily started once per test session.
pub async fn get_opkg_target() -> Option<&'static Target> {
    OPKG_TARGET
        .get_or_init(|| async { Target::new("opkg").await })
        .await
        .as_ref()
}

/// Returns the shared apk test container, lazily started once per test session.
pub async fn get_apk_target() -> Option<&'static Target> {
    APK_TARGET
        .get_or_init(|| async { Target::new("apk").await })
        .await
        .as_ref()
}

/// Returns the path to the opkg test JSON config.
pub fn opkg_json_path() -> PathBuf {
    get_session_artifacts().opkg_json.clone()
}

/// Returns the path to the apk test JSON config.
pub fn apk_json_path() -> PathBuf {
    get_session_artifacts().apk_json.clone()
}

/// Returns the SSH key path.
pub fn ssh_key_path() -> PathBuf {
    get_session_artifacts().ssh_key.clone()
}

/// Counts anonymous-section headers (`@name[idx]=`) in UCI show output.
pub fn count_uci_sections(uci_show: &str) -> usize {
    let mut count = 0;
    let bytes = uci_show.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Check if preceded by config name (not start of line)
            let config_end = if i == 0 {
                0
            } else {
                bytes[..i]
                    .iter()
                    .rev()
                    .position(|&b| b == b'.')
                    .map(|p| i - p)
                    .unwrap_or(i)
            };
            let config_name = &uci_show[config_end..i];
            if !config_name.is_empty()
                && config_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                // Look for [idx]=
                let rest = &uci_show[i + 1..];
                if let Some(bracket_start) = rest.find('[') {
                    let after_bracket = &rest[bracket_start + 1..];
                    if let Some(bracket_end) = after_bracket.find(']') {
                        let idx_str = &after_bracket[..bracket_end];
                        if idx_str.bytes().all(|b| b.is_ascii_digit())
                            && after_bracket[bracket_end..].starts_with("=")
                        {
                            count += 1;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    count
}
