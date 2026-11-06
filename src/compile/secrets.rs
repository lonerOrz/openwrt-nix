use crate::config::models::{FileContent, Root, Section};
use crate::utils::error::ConfigError;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

fn insert_secrets(
    secrets: &mut HashMap<String, String>,
    id_path: &mut Vec<String>,
    k: String,
    v: Value,
) -> Result<(), String> {
    let secret = match v {
        Value::String(s) => Some((k, s)),
        Value::Number(n) => Some((k, n.to_string())),
        Value::Bool(b) => Some((k, b.to_string())),

        Value::Array(a) => {
            a.into_iter().enumerate().try_for_each(|(i, elem)| {
                insert_secrets(secrets, id_path, format!("{}[{}]", k, i), elem)
            })?;

            None
        }

        Value::Object(o) => {
            id_path.push(k);

            let result = o
                .into_iter()
                .try_for_each(|(k, v)| insert_secrets(secrets, id_path, k, v));

            id_path.pop();
            result?;

            None
        }

        _ => Some((k, v.to_string())),
    };

    if let Some((k, secret)) = secret {
        id_path.push(k);
        let secret_id = id_path.join(".");

        let result = if secrets
            .get(&secret_id)
            .is_some_and(|existing| existing != secret.as_str())
        {
            // Return the conflicting key id, never the plaintext secret value.
            Err(secret_id)
        } else {
            secrets.insert(secret_id, secret.clone());
            Ok(())
        };

        id_path.pop();
        result
    } else {
        Ok(())
    }
}

pub(crate) fn interpolate_secrets<'a>(
    option_val: &'a str,
    secrets: &HashMap<String, String>,
) -> Result<Cow<'a, str>, ConfigError> {
    if !option_val.contains('@') {
        return Ok(Cow::Borrowed(option_val));
    }

    let mut result = String::with_capacity(option_val.len());
    let mut last_pos = 0;
    let mut current_pos = 0;

    while let Some(start_offset) = option_val[current_pos..].find('@') {
        let start = current_pos + start_offset;
        let remaining = &option_val[start + 1..];

        if let Some(end_offset) = remaining.find('@') {
            let end = start + 1 + end_offset;
            let secret_name = &option_val[start + 1..end];

            if let Some(secret_val) = secrets.get(secret_name) {
                result.push_str(&option_val[last_pos..start]);
                result.push_str(secret_val);
                last_pos = end + 1;
                current_pos = end + 1;
            } else {
                let is_valid_identifier = !secret_name.is_empty()
                    && secret_name.chars().all(|c| {
                        c.is_ascii_alphanumeric() || ['_', '-', '.', '[', ']'].contains(&c)
                    });

                if is_valid_identifier {
                    return Err(ConfigError::Validation(format!(
                        "Tried to use secret {}, but no secret with this name specified.",
                        secret_name
                    )));
                } else {
                    current_pos = start + 1;
                }
            }
        } else {
            break;
        }
    }

    if last_pos == 0 {
        Ok(Cow::Borrowed(option_val))
    } else {
        result.push_str(&option_val[last_pos..]);
        Ok(Cow::Owned(result))
    }
}

fn resolve_value(val: &mut Value, secrets: &HashMap<String, String>) -> Result<(), ConfigError> {
    match val {
        Value::String(s) => {
            let interpolated = interpolate_secrets(s, secrets)?;
            *s = interpolated.into_owned();
        }
        Value::Array(arr) => {
            for item in arr {
                resolve_value(item, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Resolve `@name@` secret placeholders across an explicit field whitelist.
///
/// Supported (secrets MAY appear here):
/// - `settings[*].options[*]` — UCI option values (scalars and list elements).
/// - `packageSources.opkgFeeds` / `packageSources.apkRepositories` — feed/repo URLs.
/// - `rawUci[*]` — custom UCI command lines.
/// - `files[*].content` — only the `Text` variant; never `Base64` (it would corrupt
///   the encoded bytes).
/// - `sshKeys[*]` — authorized-key lines.
///
/// Deliberately NOT interpolated (not secret-bearing, `@` is not valid there):
/// - `packages` / `packageSources.localPackages` — package names and paths.
/// - `files[*].content` `Base64` variant.
///
/// The placeholder syntax is `@name@` (a project-local convention, distinct from
/// sops-nix's `{{name}}` templates). Unknown-but-valid identifiers error; sequences
/// that are not valid identifiers are left verbatim.
pub(crate) fn resolve_secrets(
    mut root: Root,
    secrets: &HashMap<String, String>,
) -> Result<Root, ConfigError> {
    for sections in root.settings.values_mut() {
        for section in sections.values_mut() {
            match section {
                Section::List(arr) => {
                    for section in arr {
                        for (_, v) in section.options.iter_mut() {
                            resolve_value(v, secrets)?;
                        }
                    }
                }
                Section::Named(section) => {
                    for (_, v) in section.options.iter_mut() {
                        resolve_value(v, secrets)?;
                    }
                }
            }
        }
    }

    if let Some(sources) = &mut root.package_sources {
        for feeds in [
            sources.opkg_feeds.as_mut(),
            sources.apk_repositories.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            for feed in feeds {
                let interpolated = interpolate_secrets(feed, secrets)?;
                *feed = interpolated.into_owned();
            }
        }
    }

    if let Some(raw) = &mut root.raw_uci {
        for line in raw.iter_mut() {
            let interpolated = interpolate_secrets(line, secrets)?;
            *line = interpolated.into_owned();
        }
    }

    if let Some(files) = &mut root.files {
        for file in files.iter_mut() {
            if let FileContent::Text(text) = &mut file.content {
                let interpolated = interpolate_secrets(text, secrets)?;
                file.content = FileContent::Text(interpolated.into_owned());
            }
        }
    }

    for key in root.ssh_keys.iter_mut() {
        let interpolated = interpolate_secrets(key, secrets)?;
        *key = interpolated.into_owned();
    }

    Ok(root)
}

pub(crate) fn load_secrets_dir(dir_path: &str) -> Result<HashMap<String, String>, ConfigError> {
    let dir = Path::new(dir_path);
    let mut secrets = HashMap::new();
    if !dir.is_dir() {
        return Ok(secrets);
    }

    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            let sec_file = File::open(&path)?;
            let parsed: Value = serde_json::from_reader(BufReader::new(sec_file))?;

            if let Value::Object(obj) = parsed {
                let mut id_path = Vec::new();

                for (k, v) in obj {
                    if k == "sops" {
                        continue;
                    }

                    if let Err(conflict) = insert_secrets(&mut secrets, &mut id_path, k, v) {
                        return Err(ConfigError::Validation(format!(
                            "Secret key '{}' conflicts with different values across files. File causing conflict: '{}'",
                            conflict,
                            path.display()
                        )));
                    }
                }
            }
        }
    }
    Ok(secrets)
}

pub(crate) fn decrypt_sops_mem(root: &Root) -> Result<HashMap<String, String>, ConfigError> {
    let mut secrets = HashMap::new();
    let sops_files = match root.secrets.as_ref().and_then(|s| s.sops.as_ref()) {
        Some(sops) => &sops.files,
        None => return Ok(secrets),
    };

    for file in sops_files {
        if !Path::new(file).exists() {
            return Err(ConfigError::Sops(format!(
                "Configured SOPS file not found: {file}"
            )));
        }

        let output = Command::new("sops")
            .args(["-d", "--output-type", "json", file])
            .output()
            .map_err(|e| ConfigError::Sops(format!("Failed to run sops: {e}")))?;

        if !output.status.success() {
            return Err(ConfigError::Sops(format!(
                "Failed to decrypt sops file: {file}"
            )));
        }

        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| ConfigError::Sops(format!("Failed to parse decrypted JSON: {e}")))?;

        if let Value::Object(obj) = parsed {
            let mut id_path = Vec::new();

            for (k, v) in obj {
                if k == "sops" {
                    continue;
                }

                if let Err(conflict) = insert_secrets(&mut secrets, &mut id_path, k, v) {
                    return Err(ConfigError::Sops(format!(
                        "Secret key '{conflict}' conflicts with different values across SOPS files. File causing conflict: '{file}'"
                    )));
                }
            }
        }
    }
    Ok(secrets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::{PackageSources, Section, SectionData, SopsConfig, SopsFiles};
    use indexmap::IndexMap;
    use std::fs;
    use tempfile::TempDir;

    fn secrets(map: &[(&str, &str)]) -> HashMap<String, String> {
        map.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn root(settings: IndexMap<String, IndexMap<String, Section>>) -> Root {
        Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
            raw_uci: None,
            files: None,
        }
    }

    fn section(config: &str, section: &str, ty: &str, options: IndexMap<String, Value>) -> Root {
        root(IndexMap::from([(
            config.into(),
            IndexMap::from([(
                section.into(),
                Section::Named(SectionData {
                    section_type: ty.into(),
                    options,
                }),
            )]),
        )]))
    }

    fn list_section(
        config: &str,
        section: &str,
        ty: &str,
        options: IndexMap<String, Value>,
    ) -> Root {
        root(IndexMap::from([(
            config.into(),
            IndexMap::from([(
                section.into(),
                Section::List(vec![SectionData {
                    section_type: ty.into(),
                    options,
                }]),
            )]),
        )]))
    }

    #[test]
    fn interpolate_no_at() {
        let s = interpolate_secrets("plain text", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "plain text");
    }

    #[test]
    fn interpolate_empty_secrets_errors_on_valid_placeholder() {
        let err = interpolate_secrets("@secret@", &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("secret"));
    }

    #[test]
    fn interpolate_single_secret() {
        let s =
            interpolate_secrets("key=@wifi_key@", &secrets(&[("wifi_key", "hunter2")])).unwrap();
        assert_eq!(s.as_ref(), "key=hunter2");
    }

    #[test]
    fn interpolate_multiple_secrets() {
        let s = interpolate_secrets("@a@_@b@", &secrets(&[("a", "x"), ("b", "y")])).unwrap();
        assert_eq!(s.as_ref(), "x_y");
    }

    #[test]
    fn interpolate_missing_secret_errors() {
        let err = interpolate_secrets("@missing@", &secrets(&[("other", "v")])).unwrap_err();
        assert!(format!("{err}").contains("missing"));
    }

    #[test]
    fn interpolate_non_identifier_passthrough() {
        let s = interpolate_secrets("@not valid@", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "@not valid@");
    }

    #[test]
    fn interpolate_at_boundary_only() {
        let s = interpolate_secrets("@@", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "@@");
    }

    #[test]
    fn resolve_secrets_success() {
        let root = section(
            "wireless",
            "radio0",
            "wifi-iface",
            IndexMap::from([("key".into(), Value::String("@wifi_pass@".into()))]),
        );
        let secs = secrets(&[("wifi_pass", "secret123")]);
        let resolved = resolve_secrets(root, &secs).unwrap();

        if let Section::Named(section) = &resolved.settings["wireless"]["radio0"] {
            assert_eq!(section.options["key"], "secret123");
        } else {
            panic!("Expected Section::Named");
        }
    }

    #[test]
    fn resolve_secrets_missing_secret_errors() {
        let root = section(
            "wireless",
            "radio0",
            "wifi-iface",
            IndexMap::from([("key".into(), Value::String("@missing_secret@".into()))]),
        );
        let err = resolve_secrets(root, &secrets(&[("other", "v")])).unwrap_err();
        assert!(format!("{err}").contains("missing_secret"));
    }

    #[test]
    fn resolve_secrets_preserves_type_field() {
        let root = section(
            "config",
            "test",
            "wifi-iface",
            IndexMap::from([("key".into(), Value::String("plain".into()))]),
        );
        let resolved = resolve_secrets(root, &HashMap::new()).unwrap();
        if let Section::Named(section) = &resolved.settings["config"]["test"] {
            assert_eq!(section.section_type, "wifi-iface");
            assert_eq!(section.options["key"], "plain");
        } else {
            panic!("Expected Section::Named");
        }
    }

    #[test]
    fn resolve_secrets_empty_map_errors_on_placeholder() {
        let root = section(
            "c",
            "s",
            "test",
            IndexMap::from([("key".into(), Value::String("@secret@".into()))]),
        );
        let err = resolve_secrets(root, &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("secret"));
    }

    #[test]
    fn resolve_secrets_list_section() {
        let root = list_section(
            "dropbear",
            "dropbear",
            "dropbear",
            IndexMap::from([("Port".into(), Value::String("@port@".into()))]),
        );
        let secs = secrets(&[("port", "22")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        if let Section::List(arr) = &resolved.settings["dropbear"]["dropbear"] {
            assert_eq!(arr[0].options["Port"], "22");
            assert_eq!(arr[0].section_type, "dropbear");
        } else {
            panic!("Expected Section::List");
        }
    }

    #[test]
    fn resolve_secrets_opkg_feeds() {
        let root = Root {
            package_sources: Some(PackageSources {
                opkg_feeds: Some(vec!["src/gz @repo_name@ https://example.com".into()]),
                apk_repositories: None,
                local_packages: None,
            }),
            ..root(IndexMap::new())
        };

        let secs = secrets(&[("repo_name", "custom")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        let feeds = resolved.package_sources.unwrap().opkg_feeds.unwrap();
        assert_eq!(feeds[0], "src/gz custom https://example.com");
    }

    #[test]
    fn resolve_secrets_apk_repositories() {
        let root = Root {
            package_sources: Some(PackageSources {
                opkg_feeds: None,
                apk_repositories: Some(vec!["https://@repo_host@/packages".into()]),
                local_packages: None,
            }),
            ..root(IndexMap::new())
        };

        let secs = secrets(&[("repo_host", "downloads.openwrt.org")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        let repos = resolved.package_sources.unwrap().apk_repositories.unwrap();
        assert_eq!(repos[0], "https://downloads.openwrt.org/packages");
    }

    #[test]
    fn resolve_secrets_raw_uci() {
        let root = Root {
            raw_uci: Some(vec!["uci set network.wan.username='@wan_user@'".into()]),
            ..root(IndexMap::new())
        };
        let secs = secrets(&[("wan_user", "alice")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        assert_eq!(
            resolved.raw_uci.unwrap()[0],
            "uci set network.wan.username='alice'"
        );
    }

    #[test]
    fn resolve_secrets_file_text_content() {
        let root = Root {
            files: Some(vec![crate::config::models::File {
                path: "/etc/app/conf".into(),
                content: FileContent::Text("token=@api_token@".into()),
                executable: false,
                checksum: None,
            }]),
            ..root(IndexMap::new())
        };
        let secs = secrets(&[("api_token", "s3cr3t")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        match &resolved.files.unwrap()[0].content {
            FileContent::Text(s) => assert_eq!(s, "token=s3cr3t"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn resolve_secrets_file_base64_not_interpolated() {
        // Base64 must not be interpolated — substitution would corrupt the encoded bytes.
        let root = Root {
            files: Some(vec![crate::config::models::File {
                path: "/usr/bin/blob".into(),
                content: FileContent::Base64("QG5vdF9hX3NlY3JldEA=".into()),
                executable: true,
                checksum: None,
            }]),
            ..root(IndexMap::new())
        };
        let secs = secrets(&[("not_a_secret", "leaked")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        match &resolved.files.unwrap()[0].content {
            FileContent::Base64(b64) => assert_eq!(b64, "QG5vdF9hX3NlY3JldEA="),
            other => panic!("expected Base64, got {other:?}"),
        }
    }

    #[test]
    fn resolve_secrets_ssh_keys() {
        let root = Root {
            ssh_keys: vec!["ssh-ed25519 @pubkey@ user@host".into()],
            ..root(IndexMap::new())
        };
        let secs = secrets(&[("pubkey", "AAAAC3NzaC1lZDI1NTE5AAAAIExample")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        assert_eq!(
            resolved.ssh_keys[0],
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host"
        );
    }

    #[test]
    fn load_secrets_nonexistent_dir() {
        let result = load_secrets_dir("/tmp/nonexistent_secrets_dir_xyz").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn load_secrets_normal() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("a.json"),
            r#"{"key1": "val1", "key2": "val2"}"#,
        )
        .unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["key1"], "val1");
        assert_eq!(result["key2"], "val2");
    }

    #[test]
    fn load_secrets_skips_sops_key() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("sec.json"),
            r#"{"key": "val", "sops": {"encrypted": "data"}}"#,
        )
        .unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["key"], "val");
        assert!(!result.contains_key("sops"));
    }

    #[test]
    fn load_secrets_deterministic_order() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("z.json"), r#"{"key": "same_val"}"#).unwrap();
        fs::write(dir.path().join("a.json"), r#"{"key": "same_val"}"#).unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["key"], "same_val");
    }

    #[test]
    fn load_secrets_multiple_files_different_keys() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.json"), r#"{"key_a": "from_a"}"#).unwrap();
        fs::write(dir.path().join("b.json"), r#"{"key_b": "from_b"}"#).unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["key_a"], "from_a");
        assert_eq!(result["key_b"], "from_b");
    }

    #[test]
    fn load_secrets_conflict_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.json"), r#"{"key": "val_a"}"#).unwrap();
        fs::write(dir.path().join("b.json"), r#"{"key": "val_b"}"#).unwrap();
        let err = load_secrets_dir(dir.path().to_str().unwrap()).unwrap_err();
        assert!(format!("{err}").contains("conflicts"));
        assert!(format!("{err}").contains("key"));
    }

    #[test]
    fn load_secrets_same_key_same_value_ok() {
        // Same key with identical value across files is an idempotent merge, not a
        // conflict: must succeed and keep the value.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.json"), r#"{"key": "same_val"}"#).unwrap();
        fs::write(dir.path().join("b.json"), r#"{"key": "same_val"}"#).unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["key"], "same_val");
    }

    #[test]
    fn load_secrets_nested_conflict_errors() {
        // Nested objects: db.password differs across files -> fully-qualified id
        // 'db.password' must conflict and surface in the error.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("a.json"),
            r#"{"db": {"password": "secret_a"}}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("b.json"),
            r#"{"db": {"password": "secret_b"}}"#,
        )
        .unwrap();
        let err = load_secrets_dir(dir.path().to_str().unwrap()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("conflicts"), "expected conflict, got: {msg}");
        assert!(
            msg.contains("db.password"),
            "expected qualified id, got: {msg}"
        );
        // Secret plaintext must never leak into the error.
        assert!(!msg.contains("secret_a"), "plaintext leaked: {msg}");
        assert!(!msg.contains("secret_b"), "plaintext leaked: {msg}");
    }

    // The conflict-detection logic lives in insert_secrets and is shared by both
    // load_secrets_dir and decrypt_sops_mem. These tests pin its semantics without
    // requiring the sops/age binaries (which may be absent in CI).

    #[test]
    fn insert_secrets_same_key_same_value_ok() {
        // Same id with identical value across files -> no conflict (idempotent merge).
        let mut secrets = HashMap::new();
        let mut id_path = Vec::new();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "key".into(),
            Value::String("v".into()),
        )
        .unwrap();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "key".into(),
            Value::String("v".into()),
        )
        .unwrap();
        assert_eq!(secrets["key"], "v");
    }

    #[test]
    fn insert_secrets_same_key_different_value_errors() {
        let mut secrets = HashMap::new();
        let mut id_path = Vec::new();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "key".into(),
            Value::String("a".into()),
        )
        .unwrap();
        let err = insert_secrets(
            &mut secrets,
            &mut id_path,
            "key".into(),
            Value::String("b".into()),
        )
        .unwrap_err();
        assert_eq!(err, "key");
        // Secret plaintext must never appear in the returned id.
        assert!(!err.contains('a'));
        assert!(!err.contains('b'));
    }

    #[test]
    fn insert_secrets_nested_conflict_errors() {
        // Two files both define db.password with different values, nested under
        // different top-level objects -> the fully-qualified id must conflict.
        let mut secrets = HashMap::new();
        let mut id_path = Vec::new();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "db".into(),
            serde_json::json!({ "password": "secret_a" }),
        )
        .unwrap();
        let err = insert_secrets(
            &mut secrets,
            &mut id_path,
            "db".into(),
            serde_json::json!({ "password": "secret_b" }),
        )
        .unwrap_err();
        assert_eq!(err, "db.password");
        assert!(!err.contains("secret_a"));
        assert!(!err.contains("secret_b"));
    }

    #[test]
    fn insert_secrets_nested_same_value_ok() {
        let mut secrets = HashMap::new();
        let mut id_path = Vec::new();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "db".into(),
            serde_json::json!({ "password": "same" }),
        )
        .unwrap();
        insert_secrets(
            &mut secrets,
            &mut id_path,
            "db".into(),
            serde_json::json!({ "password": "same" }),
        )
        .unwrap();
        assert_eq!(secrets["db.password"], "same");
    }
    #[test]
    fn decrypt_sops_mem_conflict_errors() {
        use std::process::Command;

        if Command::new("sops").arg("--version").output().is_err()
            || Command::new("age-keygen").arg("--help").output().is_err()
        {
            eprintln!("skipping decrypt_sops_mem_conflict_errors: sops/age not available");
            return;
        }

        let key_home = TempDir::new().unwrap();
        let key_file = key_home.path().join("keys.txt");
        let keygen = Command::new("age-keygen")
            .args(["-o", key_file.to_str().unwrap()])
            .output()
            .expect("age-keygen");
        let pub_key = String::from_utf8_lossy(&keygen.stderr)
            .lines()
            .find_map(|l| l.trim().strip_prefix("Public key:"))
            .expect("age public key")
            .trim()
            .to_string();

        let dir = TempDir::new().unwrap();

        // Encrypt two files that share a key but disagree on the value.
        let plain_a = dir.path().join("a.json");
        fs::write(&plain_a, r#"{"key": "val_a"}"#).unwrap();
        let enc_a = dir.path().join("a.enc.json");
        Command::new("sops")
            .env("SOPS_AGE_KEY_FILE", key_file.to_str().unwrap())
            .args([
                "--encrypt",
                "--age",
                &pub_key,
                "--output",
                enc_a.to_str().unwrap(),
                plain_a.to_str().unwrap(),
            ])
            .output()
            .expect("sops encrypt a");

        let plain_b = dir.path().join("b.json");
        fs::write(&plain_b, r#"{"key": "val_b"}"#).unwrap();
        let enc_b = dir.path().join("b.enc.json");
        Command::new("sops")
            .env("SOPS_AGE_KEY_FILE", key_file.to_str().unwrap())
            .args([
                "--encrypt",
                "--age",
                &pub_key,
                "--output",
                enc_b.to_str().unwrap(),
                plain_b.to_str().unwrap(),
            ])
            .output()
            .expect("sops encrypt b");

        // The decryption child process inherits this to locate the age key.
        unsafe {
            std::env::set_var("SOPS_AGE_KEY_FILE", key_file.to_str().unwrap());
        }

        let root = Root {
            secrets: Some(SopsConfig {
                sops: Some(SopsFiles {
                    files: vec![
                        enc_a.to_str().unwrap().to_string(),
                        enc_b.to_str().unwrap().to_string(),
                    ],
                }),
            }),
            ..root(IndexMap::new())
        };

        let err = decrypt_sops_mem(&root).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("conflicts"),
            "expected conflict error, got: {msg}"
        );
        assert!(
            msg.contains("key"),
            "expected key name in error, got: {msg}"
        );
        // Ensure secret plaintext never leaks into the error.
        assert!(!msg.contains("val_a"), "plaintext leaked: {msg}");
        assert!(!msg.contains("val_b"), "plaintext leaked: {msg}");
    }

    #[test]
    fn load_secrets_skips_non_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.txt"), "not json").unwrap();
        fs::write(dir.path().join("ok.json"), r#"{"k": "v"}"#).unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["k"], "v");
    }

    #[test]
    fn load_secrets_invalid_json_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bad.json"), "not valid json {{{").unwrap();
        let err = load_secrets_dir(dir.path().to_str().unwrap()).unwrap_err();
        assert!(format!("{err}").contains("Failed to parse"));
    }

    #[test]
    fn load_secrets_non_string_values() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("mix.json"),
            r#"{"str": "hello", "num": 42, "bool": true}"#,
        )
        .unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["str"], "hello");
        assert_eq!(result["num"], "42");
        assert_eq!(result["bool"], "true");
    }

    #[test]
    fn load_secrets_skips_subdirectories() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("top.json"), r#"{"top_key": "top_val"}"#).unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(
            dir.path().join("subdir/nested.json"),
            r#"{"nested_key": "nested_val"}"#,
        )
        .unwrap();
        let result = load_secrets_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result["top_key"], "top_val");
        assert!(!result.contains_key("nested_key"));
    }

    #[test]
    fn unclosed_marker_passthrough() {
        let s = interpolate_secrets("my@unclosed", &HashMap::new()).unwrap();
        assert_eq!(s.as_ref(), "my@unclosed");

        let err = interpolate_secrets("has@in@middle@here", &HashMap::new()).unwrap_err();
        assert!(format!("{err}").contains("in"));
    }
}
