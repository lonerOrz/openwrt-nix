use crate::error::ConfigError;
use crate::helpers::iter_options_mut;
use crate::models::{Root, Section};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

pub(crate) fn interpolate_secrets<'a>(
    option_val: &'a str,
    secrets: &HashMap<String, String>,
) -> Result<Cow<'a, str>, ConfigError> {
    if !option_val.contains('@') || secrets.is_empty() {
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
                    && secret_name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_');

                if is_valid_identifier {
                    return Err(ConfigError(format!(
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

pub(crate) fn resolve_secrets(
    mut root: Root,
    secrets: &HashMap<String, String>,
) -> Result<Root, ConfigError> {
    if secrets.is_empty() {
        return Ok(root);
    }

    for sections in root.settings.values_mut() {
        for section in sections.values_mut() {
            match section {
                Section::List(arr) => {
                    for map in arr {
                        for (_, v) in iter_options_mut(map) {
                            resolve_value(v, secrets)?;
                        }
                    }
                }
                Section::Named(map) => {
                    for (_, v) in iter_options_mut(map) {
                        resolve_value(v, secrets)?;
                    }
                }
            }
        }
    }

    if let Some(sources) = &mut root.package_sources
        && let Some(feeds) = &mut sources.feeds
    {
        for feed in feeds {
            let interpolated = interpolate_secrets(feed, secrets)?;
            *feed = interpolated.into_owned();
        }
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

            if let Some(obj) = parsed.as_object() {
                for (k, v) in obj {
                    if k == "sops" {
                        continue;
                    }
                    let val_str = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => v.to_string(),
                    };

                    if let Some(old_val) = secrets.insert(k.clone(), val_str)
                        && old_val != secrets[k]
                    {
                        return Err(ConfigError(format!(
                            "Secret key '{}' conflicts with different values across files. File causing conflict: '{}'",
                            k,
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
            return Err(ConfigError(format!(
                "Configured SOPS file not found: {file}"
            )));
        }

        let output = Command::new("sops")
            .args(["-d", "--output-type", "json", file])
            .output()
            .map_err(|e| ConfigError(format!("Failed to run sops: {e}")))?;

        if !output.status.success() {
            return Err(ConfigError(format!("Failed to decrypt sops file: {file}")));
        }

        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| ConfigError(format!("Failed to parse decrypted JSON: {e}")))?;

        if let Some(obj) = parsed.as_object() {
            for (k, v) in obj {
                if k == "sops" {
                    continue;
                }
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => v.to_string(),
                };
                secrets.insert(k.clone(), val);
            }
        }
    }
    Ok(secrets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PackageSources;
    use serde_json::Map;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn secrets(map: &[(&str, &str)]) -> HashMap<String, String> {
        map.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn interpolate_no_at() {
        let s = interpolate_secrets("plain text", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "plain text");
    }

    #[test]
    fn interpolate_empty_secrets() {
        let s = interpolate_secrets("@secret@", &HashMap::new()).unwrap();
        assert_eq!(s.as_ref(), "@secret@");
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
        assert!(err.0.contains("missing"));
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
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("wifi-iface".into()));
        obj.insert("key".into(), Value::String("@wifi_pass@".into()));

        let mut sections = BTreeMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };

        let secs = secrets(&[("wifi_pass", "secret123")]);
        let resolved = resolve_secrets(root, &secs).unwrap();

        if let Section::Named(map) = &resolved.settings["wireless"]["radio0"] {
            assert_eq!(map["key"], "secret123");
        } else {
            panic!("Expected Section::Named");
        }
    }

    #[test]
    fn resolve_secrets_missing_secret_errors() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("wifi-iface".into()));
        obj.insert("key".into(), Value::String("@missing_secret@".into()));

        let mut sections = BTreeMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };

        let err = resolve_secrets(root, &secrets(&[("other", "v")])).unwrap_err();
        assert!(err.0.contains("missing_secret"));
    }

    #[test]
    fn resolve_secrets_skips_type_field() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("@not_a_secret@".into()));
        obj.insert("key".into(), Value::String("plain".into()));

        let mut sections = BTreeMap::new();
        sections.insert("test".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("config".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };

        let resolved = resolve_secrets(root, &HashMap::new()).unwrap();
        if let Section::Named(map) = &resolved.settings["config"]["test"] {
            assert_eq!(map["_type"], "@not_a_secret@");
            assert_eq!(map["key"], "plain");
        } else {
            panic!("Expected Section::Named");
        }
    }

    #[test]
    fn resolve_secrets_empty_map_shortcircuits() {
        let mut obj = Map::new();
        obj.insert("key".into(), Value::String("@secret@".into()));
        let mut sections = BTreeMap::new();
        sections.insert("s".into(), Section::Named(obj));
        let mut settings = BTreeMap::new();
        settings.insert("c".into(), sections);
        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };

        let resolved = resolve_secrets(root, &HashMap::new()).unwrap();
        if let Section::Named(map) = &resolved.settings["c"]["s"] {
            assert_eq!(map["key"], "@secret@");
        } else {
            panic!("Expected Section::Named");
        }
    }

    #[test]
    fn resolve_secrets_list_section() {
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("dropbear".into()));
        item.insert("Port".into(), Value::String("@port@".into()));
        let mut sections = BTreeMap::new();
        sections.insert("dropbear".into(), Section::List(vec![item]));
        let mut settings = BTreeMap::new();
        settings.insert("dropbear".into(), sections);
        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec![],
            secrets: None,
        };

        let secs = secrets(&[("port", "22")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        if let Section::List(arr) = &resolved.settings["dropbear"]["dropbear"] {
            assert_eq!(arr[0]["Port"], "22");
            assert_eq!(arr[0]["_type"], "dropbear");
        } else {
            panic!("Expected Section::List");
        }
    }

    #[test]
    fn resolve_secrets_feeds() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::new(),
            packages: None,
            package_sources: Some(PackageSources {
                feeds: Some(vec!["src/gz @repo_name@ https://example.com".into()]),
                local_packages: None,
            }),
            ssh_keys: vec![],
            secrets: None,
        };

        let secs = secrets(&[("repo_name", "custom")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        let feeds = resolved.package_sources.unwrap().feeds.unwrap();
        assert_eq!(feeds[0], "src/gz custom https://example.com");
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
        assert!(err.0.contains("conflicts"));
        assert!(err.0.contains("key"));
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
        assert!(err.0.contains("Failed to parse"));
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
        // @ in middle of string without matching closing @ → treated as plain text
        let s = interpolate_secrets("my@unclosed", &HashMap::new()).unwrap();
        assert_eq!(s.as_ref(), "my@unclosed");

        let s = interpolate_secrets("has@in@middle@here", &HashMap::new()).unwrap();
        assert_eq!(s.as_ref(), "has@in@middle@here");
    }
}
