use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
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
    #[serde(default = "default_package_manager")]
    #[serde(rename = "packageManager")]
    package_manager: String,
    settings: BTreeMap<String, BTreeMap<String, Section>>,
    packages: Option<Vec<String>>,
    opkg: Option<Opkg>,
}

fn default_package_manager() -> String {
    "opkg".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PkgBackend {
    Opkg,
    Apk,
}

impl PkgBackend {
    fn from_str(s: &str) -> Self {
        match s {
            "apk" => PkgBackend::Apk,
            _ => PkgBackend::Opkg,
        }
    }
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

fn is_valid_uci_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_valid_uci_type(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn validate_root(root: &Root) -> Result<(), ConfigError> {
    for (config_name, sections) in &root.settings {
        if !is_valid_uci_identifier(config_name) {
            return Err(ConfigError(format!(
                "Invalid config name '{}': only [a-zA-Z0-9_] allowed",
                config_name
            )));
        }

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    if !is_valid_uci_type(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid list identifier '{}' in config '{}': only [a-zA-Z0-9_-] allowed",
                            section_name, config_name
                        )));
                    }

                    for (idx, item) in arr.iter().enumerate() {
                        let ty = item.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                            ConfigError(format!(
                                "{}.@{}[{}] missing required '_type'",
                                config_name, section_name, idx
                            ))
                        })?;

                        if !is_valid_uci_type(ty) {
                            return Err(ConfigError(format!(
                                "Invalid type '{}' in {}.@{}[{}]: only [a-zA-Z0-9_-] allowed",
                                ty, config_name, section_name, idx
                            )));
                        }

                        for (opt_name, opt_val) in item {
                            if opt_name == "_type" {
                                continue;
                            }
                            if !is_valid_uci_identifier(opt_name) {
                                return Err(ConfigError(format!(
                                    "Invalid option '{}' in {}.@{}[{}]: only [a-zA-Z0-9_] allowed",
                                    opt_name, config_name, section_name, idx
                                )));
                            }
                            if matches!(opt_val, Value::Null) {
                                return Err(ConfigError(format!(
                                    "{}.@{}[{}].{} has null value",
                                    config_name, section_name, idx, opt_name
                                )));
                            }
                        }
                    }
                }
                Section::Named(map) => {
                    if !is_valid_uci_identifier(section_name) {
                        return Err(ConfigError(format!(
                            "Invalid section '{}' in config '{}': only [a-zA-Z0-9_] allowed",
                            section_name, config_name
                        )));
                    }

                    let ty = map.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                        ConfigError(format!(
                            "{}.{} missing required '_type'",
                            config_name, section_name
                        ))
                    })?;

                    if !is_valid_uci_type(ty) {
                        return Err(ConfigError(format!(
                            "Invalid type '{}' in {}.{}: only [a-zA-Z0-9_-] allowed",
                            ty, config_name, section_name
                        )));
                    }

                    for (opt_name, opt_val) in map {
                        if opt_name == "_type" {
                            continue;
                        }
                        if !is_valid_uci_identifier(opt_name) {
                            return Err(ConfigError(format!(
                                "Invalid option '{}' in {}.{}: only [a-zA-Z0-9_] allowed",
                                opt_name, config_name, section_name
                            )));
                        }
                        if matches!(opt_val, Value::Null) {
                            return Err(ConfigError(format!(
                                "{}.{}.{} has null value",
                                config_name, section_name, opt_name
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
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
    configs: &BTreeMap<String, BTreeMap<String, Section>>,
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

fn serialize_opkg(
    writer: &mut String,
    backend: PkgBackend,
    opkg: Option<&Opkg>,
    packages: Option<&[String]>,
) -> Result<(), ConfigError> {
    if let Some(opkg_val) = opkg
        && let Some(feeds) = &opkg_val.feeds
        && !feeds.is_empty()
    {
        match backend {
            PkgBackend::Opkg => {
                writeln!(writer, "\nprintf '' > /etc/opkg/customfeeds.conf").unwrap();
                for feed in feeds {
                    writeln!(
                        writer,
                        "printf '%s\\n' '{}' >> /etc/opkg/customfeeds.conf",
                        escape_single_quotes(feed)
                    )
                    .unwrap();
                }
            }
            PkgBackend::Apk => {
                writeln!(writer, "\nmkdir -p /etc/apk/repositories.d").unwrap();
                writeln!(
                    writer,
                    "printf '' > /etc/apk/repositories.d/customfeeds.list"
                )
                .unwrap();
                for feed in feeds {
                    writeln!(
                        writer,
                        "printf '%s\\n' '{}' >> /etc/apk/repositories.d/customfeeds.list",
                        escape_single_quotes(feed)
                    )
                    .unwrap();
                }
            }
        }
    }

    if let Some(pkgs) = packages
        && !pkgs.is_empty()
    {
        writeln!(writer, "\nNEED_INSTALL=false").unwrap();
        writeln!(writer, "for pkg in {}; do", pkgs.join(" ")).unwrap();
        match backend {
            PkgBackend::Opkg => {
                writeln!(
                    writer,
                    "    if ! opkg list-installed \"$pkg\" >/dev/null 2>&1; then NEED_INSTALL=true; break; fi"
                )
                .unwrap();
            }
            PkgBackend::Apk => {
                writeln!(
                    writer,
                    "    if ! apk info -e \"$pkg\" >/dev/null 2>&1; then NEED_INSTALL=true; break; fi"
                )
                .unwrap();
            }
        }
        writeln!(writer, "done").unwrap();

        match backend {
            PkgBackend::Opkg => {
                writeln!(
                    writer,
                    "if [ \"$NEED_INSTALL\" = true ]; then opkg update && opkg install {}; fi",
                    pkgs.join(" ")
                )
                .unwrap();
            }
            PkgBackend::Apk => {
                writeln!(
                    writer,
                    "if [ \"$NEED_INSTALL\" = true ]; then apk -U add {}; fi",
                    pkgs.join(" ")
                )
                .unwrap();
            }
        }
    }

    if let Some(opkg_val) = opkg
        && let Some(local_pkgs) = &opkg_val.local_packages
    {
        for ipk_path_str in local_pkgs {
            let ipk_path = Path::new(ipk_path_str);
            if let Some(file_name) = ipk_path.file_name().and_then(|n| n.to_str()) {
                let pkg_name = extract_package_name(file_name);
                match backend {
                    PkgBackend::Opkg => {
                        writeln!(
                            writer,
                            "\nif ! opkg list-installed \"{}\" >/dev/null 2>&1; then",
                            pkg_name
                        )
                        .unwrap();
                        writeln!(writer, "    opkg install /tmp/{}", file_name).unwrap();
                        writeln!(writer, "fi").unwrap();
                    }
                    PkgBackend::Apk => {
                        writeln!(
                            writer,
                            "\nif ! apk info -e \"{}\" >/dev/null 2>&1; then",
                            pkg_name
                        )
                        .unwrap();
                        writeln!(writer, "    apk add --allow-untrusted /tmp/{}", file_name)
                            .unwrap();
                        writeln!(writer, "fi").unwrap();
                    }
                }
            }
        }
    }

    Ok(())
}

fn convert_file(path: &Path, secrets_dir: Option<&str>) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let root: Root = serde_json::from_reader(reader)?;
    validate_root(&root)?;

    let mut secrets = HashMap::new();
    if let Some(dir_path) = secrets_dir {
        secrets = load_secrets_dir(dir_path)?;
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    let mut output_buffer = String::with_capacity(4096);
    serialize_uci(&mut output_buffer, &resolved_root.settings)?;

    let backend = PkgBackend::from_str(&resolved_root.package_manager);
    serialize_opkg(
        &mut output_buffer,
        backend,
        resolved_root.opkg.as_ref(),
        resolved_root.packages.as_deref(),
    )?;

    Ok(output_buffer)
}

fn extract_package_name(file_name: &str) -> &str {
    let without_ext = file_name
        .strip_suffix(".ipk")
        .or_else(|| file_name.strip_suffix(".apk"))
        .unwrap_or(file_name);
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
    fn extract_pkg_apk_extension() {
        assert_eq!(
            extract_package_name("luci-app-nlbwmon_0.3-1_all.apk"),
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

        let mut sections = BTreeMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
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

        let mut sections = BTreeMap::new();
        sections.insert("radio0".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("wireless".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
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

        let mut sections = BTreeMap::new();
        sections.insert("test".into(), Section::Named(obj));

        let mut settings = BTreeMap::new();
        settings.insert("config".into(), sections);

        let root = Root {
            package_manager: "opkg".into(),
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
        let mut sections = BTreeMap::new();
        sections.insert("s".into(), Section::Named(obj));
        let mut settings = BTreeMap::new();
        settings.insert("c".into(), sections);
        let root = Root {
            package_manager: "opkg".into(),
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
        let mut sections = BTreeMap::new();
        sections.insert("dropbear".into(), Section::List(vec![item]));
        let mut settings = BTreeMap::new();
        settings.insert("dropbear".into(), sections);
        let root = Root {
            package_manager: "opkg".into(),
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
            package_manager: "opkg".into(),
            settings: BTreeMap::new(),
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

    // ── validate_root ──

    #[test]
    fn validate_rejects_hyphen_in_config_name() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([("network-config".into(), BTreeMap::new())]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid config name"));
    }

    #[test]
    fn validate_allows_hyphen_in_type() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("wifi-iface".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "wireless".into(),
                BTreeMap::from([("radio0".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
        };
        assert!(validate_root(&root).is_ok());
    }

    #[test]
    fn validate_rejects_hyphen_in_option_name() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("ip-address".into(), Value::String("192.168.1.1".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid option"));
    }

    #[test]
    fn validate_rejects_hyphen_in_section_name() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("my-section".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid section"));
    }

    #[test]
    fn validate_rejects_null_value() {
        let mut obj = Map::new();
        obj.insert("_type".into(), Value::String("interface".into()));
        obj.insert("proto".into(), Value::Null);
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("null value"));
    }

    #[test]
    fn validate_rejects_missing_type() {
        let mut obj = Map::new();
        obj.insert("proto".into(), Value::String("static".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "network".into(),
                BTreeMap::from([("lan".into(), Section::Named(obj))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("missing required '_type'"));
    }

    #[test]
    fn validate_list_missing_type() {
        let mut item = Map::new();
        item.insert("Port".into(), Value::String("22".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "dropbear".into(),
                BTreeMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("missing required '_type'"));
    }

    #[test]
    fn validate_list_rejects_hyphen_in_option() {
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("dropbear".into()));
        item.insert("listen-port".into(), Value::String("22".into()));
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::from([(
                "dropbear".into(),
                BTreeMap::from([("dropbear".into(), Section::List(vec![item]))]),
            )]),
            packages: None,
            opkg: None,
        };
        let err = validate_root(&root).unwrap_err();
        assert!(err.0.contains("Invalid option"));
    }

    #[test]
    fn validate_empty_settings_ok() {
        let root = Root {
            package_manager: "opkg".into(),
            settings: BTreeMap::new(),
            packages: None,
            opkg: None,
        };
        assert!(validate_root(&root).is_ok());
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

    // ── serialize_uci ──

    #[test]
    fn serialize_named_section() {
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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
        let mut configs = BTreeMap::new();
        let mut sections = BTreeMap::new();
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

    // ── serialize_opkg (opkg and apk direct testing) ──

    #[test]
    fn test_serialize_opkg_empty() {
        let mut w = String::new();
        serialize_opkg(&mut w, PkgBackend::Opkg, None, None).unwrap();
        assert!(w.is_empty());
    }

    #[test]
    fn test_serialize_opkg_feeds_opkg() {
        let mut w = String::new();
        let opkg = Opkg {
            feeds: Some(vec!["src/gz custom 'test' https://example.com".into()]),
            local_packages: None,
        };
        serialize_opkg(&mut w, PkgBackend::Opkg, Some(&opkg), None).unwrap();
        assert!(w.contains("/etc/opkg/customfeeds.conf"));
        assert!(w.contains("printf '%s\\n' 'src/gz custom '\\''test'\\'' https://example.com'"));
    }

    #[test]
    fn test_serialize_opkg_feeds_apk() {
        let mut w = String::new();
        let opkg = Opkg {
            feeds: Some(vec!["https://example.com/packages".into()]),
            local_packages: None,
        };
        serialize_opkg(&mut w, PkgBackend::Apk, Some(&opkg), None).unwrap();
        assert!(w.contains("/etc/apk/repositories.d/customfeeds.list"));
        assert!(w.contains("printf '%s\\n' 'https://example.com/packages'"));
    }

    #[test]
    fn test_serialize_opkg_packages_opkg() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_opkg(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("opkg list-installed"));
        assert!(w.contains("opkg update && opkg install luci tcpdump"));
    }

    #[test]
    fn test_serialize_opkg_packages_apk() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_opkg(&mut w, PkgBackend::Apk, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("apk info -e"));
        assert!(w.contains("apk -U add luci tcpdump"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_opkg() {
        let mut w = String::new();
        let opkg = Opkg {
            feeds: None,
            local_packages: Some(vec!["./packages/test_1.0_all.ipk".into()]),
        };
        serialize_opkg(&mut w, PkgBackend::Opkg, Some(&opkg), None).unwrap();
        assert!(w.contains("opkg list-installed \"test\""));
        assert!(w.contains("opkg install /tmp/test_1.0_all.ipk"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_apk() {
        let mut w = String::new();
        let opkg = Opkg {
            feeds: None,
            local_packages: Some(vec!["./packages/test_1.0_all.apk".into()]),
        };
        serialize_opkg(&mut w, PkgBackend::Apk, Some(&opkg), None).unwrap();
        assert!(w.contains("apk info -e \"test\""));
        assert!(w.contains("apk add --allow-untrusted /tmp/test_1.0_all.apk"));
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
            "packageManager": "opkg",
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
            "packageManager": "opkg",
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
            "packageManager": "opkg",
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
            "packageManager": "opkg",
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
            "packageManager": "opkg",
            "settings": {},
            "opkg": { "feeds": ["src/gz test it's a feed"] }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
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
            "opkg": {
                "feeds": ["https://example.com/packages"],
                "localPackages": ["./pkg/foo_1.0_all.apk"]
            }
        }"#,
        )
        .unwrap();
        let output = convert_file(&json_path, None).unwrap();
        assert!(output.contains("/etc/apk/repositories.d/customfeeds.list"));
        assert!(output.contains("apk -U add luci"));
        assert!(output.contains("apk add --allow-untrusted /tmp/foo_1.0_all.apk"));
    }
}
