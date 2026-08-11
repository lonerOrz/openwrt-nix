use super::target::Target;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tokio::sync::OnceCell;

// Performance Decision: Sandbox creates dynamic test artifacts in /tmp/nuci_test_sandbox_*
// to prevent Git working tree pollution and ensure parallel isolation across test sessions.
pub struct SessionArtifacts {
    #[allow(dead_code)]
    pub session_dir: PathBuf,
    pub ssh_key: PathBuf,
    pub sops_key_dir: PathBuf,
    pub opkg_json: PathBuf,
    pub apk_json: PathBuf,
}

static ARTIFACTS: OnceLock<SessionArtifacts> = OnceLock::new();
static OPKG_TARGET: OnceCell<Target> = OnceCell::const_new();
static APK_TARGET: OnceCell<Target> = OnceCell::const_new();

pub fn get_session_artifacts() -> &'static SessionArtifacts {
    ARTIFACTS.get_or_init(|| {
        let session_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let session_dir = PathBuf::from(format!("/tmp/nuci_test_sandbox_{session_id}"));
        std::fs::create_dir_all(&session_dir).expect("Failed to create test sandbox dir");

        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let src_tests = manifest_dir.join("tests");
        copy_tests_to_sandbox(&src_tests, &session_dir);

        let ssh_key = session_dir.join("ssh_key");
        let sops_key_dir = session_dir.join("sops_keys");
        let encrypted_secrets = session_dir.join("secrets.enc.json");

        generate_ssh_keypair(&ssh_key);
        let pub_key = std::fs::read_to_string(ssh_key.with_extension("pub")).expect("read pub key");

        let age_key_file = generate_sops_key(&sops_key_dir);
        let age_pubkey = extract_age_pubkey(&age_key_file);
        encrypt_mock_secrets(&age_key_file, &age_pubkey, &encrypted_secrets, &session_dir);

        let temp_opkg_nix = prepare_nix_file(
            &session_dir,
            "test_config.nix",
            &pub_key,
            &encrypted_secrets,
        );
        let temp_apk_nix = prepare_nix_file(
            &session_dir,
            "test_config_apk.nix",
            &pub_key,
            &encrypted_secrets,
        );

        // Performance Decision: nix build uses --impure --expr to evaluate dynamic sandbox nix configs without creating git flakes.
        let opkg_json = eval_nix_json(&manifest_dir, &temp_opkg_nix);
        let apk_json = eval_nix_json(&manifest_dir, &temp_apk_nix);

        SessionArtifacts {
            session_dir,
            ssh_key,
            sops_key_dir,
            opkg_json,
            apk_json,
        }
    })
}

fn generate_ssh_keypair(ssh_key: &Path) {
    if !ssh_key.exists() {
        let out = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                ssh_key.to_str().unwrap(),
                "-C",
                "openwrt-test",
                "-q",
            ])
            .output()
            .expect("ssh-keygen failed");
        assert!(out.status.success(), "ssh-keygen failed");
    }
}

fn generate_sops_key(sops_key_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(sops_key_dir).expect("Failed to create sops_key_dir");
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
        assert!(
            out.status.success(),
            "age-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(&age_key_file, &out.stdout).unwrap();
    }
    age_key_file
}

fn extract_age_pubkey(age_key_file: &Path) -> String {
    let keys_content = std::fs::read_to_string(age_key_file).unwrap();
    keys_content
        .split_whitespace()
        .find(|s| s.starts_with("age1"))
        .expect("Failed to extract age public key")
        .to_string()
}

fn encrypt_mock_secrets(
    age_key_file: &Path,
    age_pubkey: &str,
    encrypted_secrets: &Path,
    session_dir: &Path,
) {
    let mock_secrets_path = session_dir.join("mock_secrets/secrets.json");
    assert!(
        mock_secrets_path.exists(),
        "mock_secrets/secrets.json must exist in sandbox at {}",
        mock_secrets_path.display()
    );

    let status = Command::new("sops")
        .env("SOPS_AGE_KEY_FILE", age_key_file)
        .args([
            "--config",
            "/dev/null",
            "--encrypt",
            "--age",
            age_pubkey,
            "--input-type",
            "json",
            "--output-type",
            "json",
            "--output",
            encrypted_secrets.to_str().unwrap(),
            mock_secrets_path.to_str().unwrap(),
        ])
        .status()
        .or_else(|_| {
            Command::new("nix")
                .env("SOPS_AGE_KEY_FILE", age_key_file)
                .args([
                    "shell",
                    "nixpkgs#sops",
                    "-c",
                    "sops",
                    "--config",
                    "/dev/null",
                    "--encrypt",
                    "--age",
                    age_pubkey,
                    "--input-type",
                    "json",
                    "--output-type",
                    "json",
                    "--output",
                    encrypted_secrets.to_str().unwrap(),
                    mock_secrets_path.to_str().unwrap(),
                ])
                .status()
        })
        .expect("Failed to execute sops command");

    assert!(
        status.success() && encrypted_secrets.exists(),
        "SOPS encryption failed; encrypted secrets file was not created at {}",
        encrypted_secrets.display()
    );
}

fn copy_tests_to_sandbox(src_tests: &Path, session_dir: &Path) {
    copy_dir_all(src_tests, session_dir).expect("Failed to copy tests directory to sandbox");
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn prepare_nix_file(
    session_dir: &Path,
    dest_name: &str,
    pub_key: &str,
    encrypted_secrets: &Path,
) -> PathBuf {
    let dest_path = session_dir.join(dest_name);
    let content = std::fs::read_to_string(&dest_path).unwrap_or_else(|e| {
        panic!(
            "Read nix test template failed at {}: {e}",
            dest_path.display()
        )
    });

    let updated = content.replace(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEGPJpRJiBIHwzjGVJxKYGO8nCrhAbHnqHox3X+qkRM8 openwrt-test",
        pub_key.trim(),
    );
    let updated = updated.replace("./secrets.enc.json", encrypted_secrets.to_str().unwrap());
    let updated = if !updated.contains("nuci_test.marker") {
        updated.replace(
            "uci.sshKeys = [",
            "uci.rawUci = [ \"uci set nuci_test.marker=escaped\" \"uci commit nuci_test\" ];\n  uci.sshKeys = [",
        )
    } else {
        updated
    };
    std::fs::write(&dest_path, updated).unwrap();
    dest_path
}

fn eval_nix_json(manifest_dir: &Path, nix_file_path: &Path) -> PathBuf {
    let expr = format!(
        "let pkgs = import <nixpkgs> {{}}; uci = pkgs.callPackage {}/nix {{}}; in (uci.writeUci {}).json",
        manifest_dir.display(),
        nix_file_path.display()
    );
    let out = Command::new("nix")
        .args([
            "build",
            "--impure",
            "--expr",
            &expr,
            "--print-out-paths",
            "--no-link",
        ])
        .output()
        .expect("nix build failed");
    assert!(
        out.status.success(),
        "nix build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(
        String::from_utf8(out.stdout)
            .expect("invalid utf8")
            .trim()
            .to_string(),
    )
}

pub fn sops_key_file() -> PathBuf {
    get_session_artifacts().sops_key_dir.join("keys.txt")
}

pub async fn get_opkg_target() -> &'static Target {
    OPKG_TARGET
        .get_or_init(|| async {
            Target::new("opkg")
                .await
                .expect("FATAL: Failed to initialize OPKG container environment!")
        })
        .await
}

pub async fn get_apk_target() -> &'static Target {
    APK_TARGET
        .get_or_init(|| async {
            Target::new("apk")
                .await
                .expect("FATAL: Failed to initialize APK container environment!")
        })
        .await
}

pub fn opkg_json_path() -> PathBuf {
    get_session_artifacts().opkg_json.clone()
}

pub fn apk_json_path() -> PathBuf {
    get_session_artifacts().apk_json.clone()
}

pub fn ssh_key_path() -> PathBuf {
    get_session_artifacts().ssh_key.clone()
}

pub fn count_uci_sections(uci_show: &str) -> usize {
    uci_show
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, val) = line.split_once('=')?;
            let (_, section_id) = key.split_once('.')?;
            if !section_id.contains('.')
                && val.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                Some(())
            } else {
                None
            }
        })
        .count()
}
