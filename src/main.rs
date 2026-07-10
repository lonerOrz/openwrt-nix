use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Debug)]
struct ConfigError(String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError(e.to_string())
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(e: serde_json::Error) -> Self {
        ConfigError(format!("Failed to parse JSON: {}", e))
    }
}

#[derive(Deserialize, Debug)]
struct Root {
    settings: HashMap<String, HashMap<String, Section>>,
    packages: Option<Vec<String>>,
    opkg: Option<Opkg>,
}

#[derive(Deserialize, Debug)]
struct Opkg {
    feeds: Option<Vec<String>>,
    #[serde(rename = "localPackages")]
    local_packages: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum Section {
    List(Vec<Map<String, Value>>),
    Named(Map<String, Value>),
}

fn interpolate_secrets<'a>(
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

fn resolve_secrets(mut root: Root, secrets: &HashMap<String, String>) -> Result<Root, ConfigError> {
    if secrets.is_empty() {
        return Ok(root);
    }

    for sections in root.settings.values_mut() {
        for section in sections.values_mut() {
            match section {
                Section::List(arr) => {
                    for map in arr {
                        for (k, v) in map.iter_mut() {
                            if k == "_type" {
                                continue;
                            }
                            resolve_value(v, secrets)?;
                        }
                    }
                }
                Section::Named(map) => {
                    for (k, v) in map.iter_mut() {
                        if k == "_type" {
                            continue;
                        }
                        resolve_value(v, secrets)?;
                    }
                }
            }
        }
    }

    if let Some(opkg) = &mut root.opkg
        && let Some(feeds) = &mut opkg.feeds
    {
        for feed in feeds {
            let interpolated = interpolate_secrets(feed, secrets)?;
            *feed = interpolated.into_owned();
        }
    }

    Ok(root)
}

fn escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

fn serialize_option_val(writer: &mut String, key: &str, val: &Value) -> Result<(), ConfigError> {
    match val {
        Value::String(s) => {
            writeln!(writer, "set {}='{}'", key, escape_single_quotes(s)).unwrap();
        }
        Value::Number(n) => {
            writeln!(
                writer,
                "set {}='{}'",
                key,
                escape_single_quotes(&n.to_string())
            )
            .unwrap();
        }
        Value::Bool(b) => {
            writeln!(
                writer,
                "set {}='{}'",
                key,
                escape_single_quotes(&b.to_string())
            )
            .unwrap();
        }
        Value::Array(arr) => {
            for item in arr {
                let s = match item {
                    Value::String(s) => Cow::Borrowed(s.as_str()),
                    Value::Number(n) => Cow::Owned(n.to_string()),
                    Value::Bool(b) => Cow::Owned(b.to_string()),
                    _ => {
                        return Err(ConfigError(format!(
                            "{:?} is not a supported list value type",
                            item
                        )));
                    }
                };
                writeln!(writer, "add_list {}='{}'", key, escape_single_quotes(&s)).unwrap();
            }
        }
        _ => {
            return Err(ConfigError(format!(
                "{:?} is not a supported option value type",
                val
            )));
        }
    }
    Ok(())
}

fn serialize_uci(
    writer: &mut String,
    configs: &HashMap<String, HashMap<String, Section>>,
) -> Result<(), ConfigError> {
    for (config_name, sections) in configs {
        let mut shell_cmds = String::new();
        let mut uci_cmds = String::new();

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    let list_ty = if let Some(first) = arr.first() {
                        first
                            .get("_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or(section_name)
                    } else {
                        section_name
                    };

                    writeln!(
                        shell_cmds,
                        "while uci -q delete {}.@{}[0]; do :; done",
                        config_name, list_ty
                    )
                    .unwrap();

                    for (idx, list_obj) in arr.iter().enumerate() {
                        let ty =
                            list_obj
                                .get("_type")
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    ConfigError(format!(
                                        "{}.@{}[{}] has no type!",
                                        config_name, section_name, idx
                                    ))
                                })?;

                        writeln!(uci_cmds, "add {} {}", config_name, ty).unwrap();

                        for (option_name, option) in list_obj {
                            if option_name == "_type" {
                                continue;
                            }
                            let key = format!("{}.@{}[{}].{}", config_name, ty, idx, option_name);
                            serialize_option_val(&mut uci_cmds, &key, option)?;
                        }
                    }
                }
                Section::Named(obj) => {
                    let ty = obj.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                        ConfigError(format!("{}.{} has no type", config_name, section_name))
                    })?;

                    writeln!(uci_cmds, "delete {}.{}", config_name, section_name).unwrap();
                    writeln!(uci_cmds, "set {}.{}={}", config_name, section_name, ty).unwrap();

                    for (option_name, option) in obj {
                        if option_name == "_type" {
                            continue;
                        }
                        let key = format!("{}.{}.{}", config_name, section_name, option_name);
                        serialize_option_val(&mut uci_cmds, &key, option)?;
                    }
                }
            }
        }

        write!(writer, "{}", shell_cmds).unwrap();

        if !uci_cmds.is_empty() {
            writeln!(writer, "uci -q batch <<'UCI_EOF'").unwrap();
            write!(writer, "{}", uci_cmds).unwrap();
            writeln!(writer, "commit {}", config_name).unwrap();
            writeln!(writer, "UCI_EOF").unwrap();
        }
    }

    Ok(())
}

fn load_secrets_dir(dir_path: &str) -> Result<HashMap<String, String>, ConfigError> {
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

fn convert_file(path: &Path, secrets_dir: Option<&str>) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let root: Root = serde_json::from_reader(reader)?;

    let mut secrets = HashMap::new();
    if let Some(dir_path) = secrets_dir {
        secrets = load_secrets_dir(dir_path)?;
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    let mut output_buffer = String::with_capacity(4096);
    serialize_uci(&mut output_buffer, &resolved_root.settings)?;

    if let Some(opkg) = &resolved_root.opkg
        && let Some(feeds) = &opkg.feeds
        && !feeds.is_empty()
    {
        writeln!(
            &mut output_buffer,
            "\nprintf '' > /etc/opkg/customfeeds.conf"
        )
        .unwrap();
        for feed in feeds {
            writeln!(
                &mut output_buffer,
                "printf '%s\\n' '{}' >> /etc/opkg/customfeeds.conf",
                feed.replace('\'', "'\\''")
            )
            .unwrap();
        }
    }

    if let Some(pkgs) = &resolved_root.packages
        && !pkgs.is_empty()
    {
        writeln!(&mut output_buffer, "\nNEED_INSTALL=false").unwrap();
        writeln!(&mut output_buffer, "for pkg in {}; do", pkgs.join(" ")).unwrap();
        writeln!(
                &mut output_buffer,
                "    if ! opkg list-installed \"$pkg\" >/dev/null 2>&1; then NEED_INSTALL=true; break; fi"
            )
            .unwrap();
        writeln!(&mut output_buffer, "done").unwrap();
        writeln!(
            &mut output_buffer,
            "if [ \"$NEED_INSTALL\" = true ]; then opkg update && opkg install {}; fi",
            pkgs.join(" ")
        )
        .unwrap();
    }

    if let Some(opkg) = &resolved_root.opkg
        && let Some(local_pkgs) = &opkg.local_packages
    {
        for ipk_path_str in local_pkgs {
            let ipk_path = Path::new(ipk_path_str);
            if let Some(file_name) = ipk_path.file_name().and_then(|n| n.to_str()) {
                let pkg_name = extract_package_name(file_name);
                writeln!(
                    &mut output_buffer,
                    "\nif ! opkg list-installed \"{}\" >/dev/null 2>&1; then",
                    pkg_name
                )
                .unwrap();
                writeln!(&mut output_buffer, "    opkg install /tmp/{}", file_name).unwrap();
                writeln!(&mut output_buffer, "fi").unwrap();
            }
        }
    }

    Ok(output_buffer)
}

fn extract_package_name(file_name: &str) -> &str {
    let without_ext = file_name.strip_suffix(".ipk").unwrap_or(file_name);
    without_ext.split('_').next().unwrap_or(without_ext)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("USAGE: {} JSON_FILE [SECRETS_DIR]", args[0]);
        std::process::exit(1);
    }

    let secrets_dir = args.get(2).map(|s| s.as_str());

    match convert_file(Path::new(&args[1]), secrets_dir) {
        Ok(output) => print!("{}", output),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn secrets(map: &[(&str, &str)]) -> HashMap<String, String> {
        map.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── interpolate_secrets ──

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
        // "@not valid@" contains a space — not a valid identifier, so it's passed through
        let s = interpolate_secrets("@not valid@", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "@not valid@");
    }

    #[test]
    fn interpolate_at_boundary_only() {
        // "@@" — second @ starts a new search but finds nothing
        let s = interpolate_secrets("@@", &secrets(&[])).unwrap();
        assert_eq!(s.as_ref(), "@@");
    }

    // ── escape_single_quotes ──

    #[test]
    fn escape_no_quotes() {
        assert_eq!(escape_single_quotes("hello"), "hello");
    }

    #[test]
    fn escape_with_quotes() {
        assert_eq!(escape_single_quotes("it's"), "it'\\''s");
    }

    // ── extract_package_name ──

    #[test]
    fn extract_pkg_standard() {
        assert_eq!(
            extract_package_name("luci-app-nlbwmon_0.3-1_all.ipk"),
            "luci-app-nlbwmon"
        );
    }

    #[test]
    fn extract_pkg_no_version() {
        assert_eq!(extract_package_name("luci.ipk"), "luci");
    }

    #[test]
    fn extract_pkg_no_extension() {
        assert_eq!(extract_package_name("luci-app_1.0"), "luci-app");
    }

    // ── resolve_secrets ──

    #[test]
    fn resolve_secrets_success() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("wifi-iface".into()));
        obj.insert("key".into(), Value::String("@wifi_pass@".into()));

        let mut sections = HashMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = HashMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            settings,
            packages: None,
            opkg: None,
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

        let mut sections = HashMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = HashMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            settings,
            packages: None,
            opkg: None,
        };

        let err = resolve_secrets(root, &secrets(&[("other", "v")])).unwrap_err();
        assert!(err.0.contains("missing_secret"));
    }

    #[test]
    fn resolve_secrets_skips_type_field() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("@not_a_secret@".into()));
        obj.insert("key".into(), Value::String("plain".into()));

        let mut sections = HashMap::new();
        sections.insert("test".into(), Section::Named(obj));

        let mut settings = HashMap::new();
        settings.insert("config".into(), sections);

        let root = Root {
            settings,
            packages: None,
            opkg: None,
        };

        let resolved = resolve_secrets(root, &HashMap::new()).unwrap();
        if let Section::Named(map) = &resolved.settings["config"]["test"] {
            // _type is NOT interpolated even if it contains @
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
        let mut sections = HashMap::new();
        sections.insert("s".into(), Section::Named(obj));
        let mut settings = HashMap::new();
        settings.insert("c".into(), sections);
        let root = Root {
            settings,
            packages: None,
            opkg: None,
        };

        // Empty secrets → shortcircuit, no error even though @secret@ is present
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
        let mut sections = HashMap::new();
        sections.insert("dropbear".into(), Section::List(vec![item]));
        let mut settings = HashMap::new();
        settings.insert("dropbear".into(), sections);
        let root = Root {
            settings,
            packages: None,
            opkg: None,
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
            settings: HashMap::new(),
            packages: None,
            opkg: Some(Opkg {
                feeds: Some(vec!["src/gz @repo_name@ https://example.com".into()]),
                local_packages: None,
            }),
        };

        let secs = secrets(&[("repo_name", "custom")]);
        let resolved = resolve_secrets(root, &secs).unwrap();
        let feeds = resolved.opkg.unwrap().feeds.unwrap();
        assert_eq!(feeds[0], "src/gz custom https://example.com");
    }

    // ── serialize_option_val (no secrets param) ──

    #[test]
    fn serialize_string_val() {
        let mut w = String::new();
        serialize_option_val(&mut w, "system.hostname", &Value::String("test".into())).unwrap();
        assert_eq!(w, "set system.hostname='test'\n");
    }

    #[test]
    fn serialize_number_val() {
        let mut w = String::new();
        serialize_option_val(&mut w, "dhcp.start", &Value::Number(100.into())).unwrap();
        assert_eq!(w, "set dhcp.start='100'\n");
    }

    #[test]
    fn serialize_bool_val() {
        let mut w = String::new();
        serialize_option_val(&mut w, "wifi.enabled", &Value::Bool(true)).unwrap();
        assert_eq!(w, "set wifi.enabled='true'\n");
    }

    #[test]
    fn serialize_array_val() {
        let mut w = String::new();
        let arr = Value::Array(vec!["a".into(), "b".into()]);
        serialize_option_val(&mut w, "net.dns", &arr).unwrap();
        assert!(w.contains("add_list net.dns='a'"));
        assert!(w.contains("add_list net.dns='b'"));
    }

    #[test]
    fn serialize_nested_object_errors() {
        let mut w = String::new();
        let obj = serde_json::json!({"nested": "value"});
        let err = serialize_option_val(&mut w, "key", &obj).unwrap_err();
        assert!(err.0.contains("not a supported option value type"));
    }

    #[test]
    fn serialize_array_with_nested_object_errors() {
        let mut w = String::new();
        let arr = Value::Array(vec![serde_json::json!({"bad": true})]);
        let err = serialize_option_val(&mut w, "key", &arr).unwrap_err();
        assert!(err.0.contains("not a supported list value type"));
    }

    #[test]
    fn serialize_null_val_errors() {
        let mut w = String::new();
        let err = serialize_option_val(&mut w, "key", &Value::Null).unwrap_err();
        assert!(err.0.contains("not a supported option value type"));
    }

    #[test]
    fn serialize_with_quote_escaping() {
        let mut w = String::new();
        let val = Value::String("it's".into());
        serialize_option_val(&mut w, "sys.name", &val).unwrap();
        assert_eq!(w, "set sys.name='it'\\''s'\n");
    }

    // ── serialize_uci (no secrets param) ──

    #[test]
    fn serialize_named_section() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("proto".into(), Value::String("static".into()));
        sections.insert("lan".into(), Section::Named(obj));
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("uci -q batch <<'UCI_EOF'"));
        assert!(w.contains("delete network.lan"));
        assert!(w.contains("set network.lan=interface"));
        assert!(w.contains("set network.lan.proto='static'"));
        assert!(w.contains("commit network"));
        assert!(w.contains("UCI_EOF"));
        assert!(!w.contains("set -e"));
    }

    #[test]
    fn serialize_list_section() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("dropbear".into()));
        item.insert("Port".into(), Value::String("22".into()));
        sections.insert("dropbear".into(), Section::List(vec![item]));
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete dropbear.@dropbear[0]; do :; done"));
        assert!(w.contains("uci -q batch <<'UCI_EOF'"));
        assert!(w.contains("add dropbear dropbear"));
        assert!(w.contains("set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("commit dropbear"));
        assert!(!w.contains("set dropbear.@dropbear[0]=dropbear"));
    }

    #[test]
    fn serialize_named_section_missing_type_errors() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut obj = Map::new();
        obj.insert("proto".into(), Value::String("static".into()));
        sections.insert("lan".into(), Section::Named(obj));
        configs.insert("network".into(), sections);

        let mut w = String::new();
        let err = serialize_uci(&mut w, &configs).unwrap_err();
        assert!(err.0.contains("has no type"));
    }

    #[test]
    fn serialize_list_section_missing_type_errors() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut item = Map::new();
        item.insert("Port".into(), Value::String("22".into()));
        sections.insert("dropbear".into(), Section::List(vec![item]));
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        let err = serialize_uci(&mut w, &configs).unwrap_err();
        assert!(err.0.contains("has no type"));
    }

    #[test]
    fn serialize_multiple_list_items() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut item1 = Map::new();
        item1.insert("_type".into(), Value::String("dropbear".into()));
        item1.insert("Port".into(), Value::String("22".into()));
        let mut item2 = Map::new();
        item2.insert("_type".into(), Value::String("dropbear".into()));
        item2.insert("Port".into(), Value::String("2222".into()));
        sections.insert("dropbear".into(), Section::List(vec![item1, item2]));
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert_eq!(w.matches("add dropbear dropbear").count(), 2);
        assert!(w.contains("set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("set dropbear.@dropbear[1].Port='2222'"));
    }

    #[test]
    fn serialize_list_section_type_mismatch() {
        let mut configs = HashMap::new();
        let mut sections = HashMap::new();
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("interface".into()));
        item.insert("proto".into(), Value::String("static".into()));
        sections.insert("interfaces".into(), Section::List(vec![item]));
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete network.@interface[0]; do :; done"));
        assert!(!w.contains("while uci -q delete network.@interfaces[0]"));
        assert!(w.contains("add network interface"));
        assert!(!w.contains("add network interfaces"));
        assert!(w.contains("set network.@interface[0].proto='static'"));
        assert!(!w.contains("set network.@interfaces[0].proto"));
    }

    // ── load_secrets_dir ──

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
        // Same value in both files — no conflict, last writer wins (z after a)
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

    // ── convert_file end-to-end ──

    #[test]
    fn convert_file_full() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "settings": {
                "system": {
                    "system": { "_type": "system", "hostname": "test" }
                }
            }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
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
            "settings": {
                "wifi": {
                    "radio0": { "_type": "wifi-iface", "key": "@wifi_pass@" }
                }
            }
        }"#,
        )
        .unwrap();
        fs::write(secrets_path.join("s.json"), r#"{"wifi_pass": "secret123"}"#).unwrap();
        let output = convert_file(&json_path, Some(secrets_path.to_str().unwrap())).unwrap();
        assert!(output.contains("set wifi.radio0.key='secret123'"));
    }

    #[test]
    fn convert_file_missing_file() {
        let err = convert_file(Path::new("/tmp/nonexistent_xyz.json"), None).unwrap_err();
        assert!(err.0.contains("No such file"));
    }

    #[test]
    fn convert_file_invalid_json() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("bad.json");
        fs::write(&json_path, "not json").unwrap();
        let err = convert_file(&json_path, None).unwrap_err();
        assert!(err.0.contains("Failed to parse JSON"));
    }

    #[test]
    fn convert_file_opkg_feeds() {
        let dir = TempDir::new().unwrap();
        let json_path = dir.path().join("config.json");
        fs::write(
            &json_path,
            r#"{
            "settings": {},
            "opkg": { "feeds": ["src/gz custom https://example.com/repo"] }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
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
            "settings": {},
            "packages": ["luci", "tcpdump"]
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
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
            "settings": {},
            "opkg": { "localPackages": ["./pkg/foo_1.0.ipk"] }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
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
            "settings": {},
            "opkg": { "feeds": ["src/gz test it's a feed"] }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
        assert!(output.contains("'\\''"));
    }
}
