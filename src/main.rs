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

#[derive(Deserialize, Debug)]
struct Root {
    settings: HashMap<String, HashMap<String, Section>>,
    packages: Option<Vec<String>>,
    opkg: Option<Opkg>,
}

#[derive(Deserialize, Debug)]
struct Opkg {
    feeds: Option<Vec<String>>,
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

fn escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

fn serialize_option_val(
    writer: &mut String,
    key: &str,
    val: &Value,
    secrets: &HashMap<String, String>,
) -> Result<(), ConfigError> {
    match val {
        Value::String(s) => {
            let interpolated = interpolate_secrets(s, secrets)?;
            writeln!(
                writer,
                "set {}='{}'",
                key,
                escape_single_quotes(&interpolated)
            )
            .unwrap();
        }
        Value::Number(n) => {
            let s = n.to_string();
            let interpolated = interpolate_secrets(&s, secrets)?;
            writeln!(
                writer,
                "set {}='{}'",
                key,
                escape_single_quotes(&interpolated)
            )
            .unwrap();
        }
        Value::Bool(b) => {
            let s = b.to_string();
            let interpolated = interpolate_secrets(&s, secrets)?;
            writeln!(
                writer,
                "set {}='{}'",
                key,
                escape_single_quotes(&interpolated)
            )
            .unwrap();
        }
        Value::Array(arr) => {
            for item in arr {
                let s = match item {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => {
                        return Err(ConfigError(format!(
                            "{:?} is not a supported list value type",
                            item
                        )));
                    }
                };
                let interpolated = interpolate_secrets(&s, secrets)?;
                writeln!(
                    writer,
                    "add_list {}='{}'",
                    key,
                    escape_single_quotes(&interpolated)
                )
                .unwrap();
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
    secrets: &HashMap<String, String>,
) -> Result<(), ConfigError> {
    writeln!(writer, "#!/bin/sh").unwrap();
    writeln!(writer, "set -e").unwrap();

    for (config_name, sections) in configs {
        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    writeln!(
                        writer,
                        "while uci -q delete {}.@{}[0]; do :; done",
                        config_name, section_name
                    )
                    .unwrap();

                    writeln!(writer, "uci batch <<EOF").unwrap();
                    for _ in 0..arr.len() {
                        writeln!(writer, "add {} {}", config_name, section_name).unwrap();
                    }
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

                        writeln!(
                            writer,
                            "set {}.@{}[{}]={}",
                            config_name, section_name, idx, ty
                        )
                        .unwrap();

                        for (option_name, option) in list_obj {
                            if option_name == "_type" {
                                continue;
                            }
                            let key = format!(
                                "{}.@{}[{}].{}",
                                config_name, section_name, idx, option_name
                            );
                            serialize_option_val(writer, &key, option, secrets)?;
                        }
                    }
                    writeln!(writer, "EOF").unwrap();
                }
                Section::Named(obj) => {
                    writeln!(writer, "uci batch <<EOF").unwrap();
                    writeln!(writer, "delete {}.{}", config_name, section_name).unwrap();

                    let ty = obj.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                        ConfigError(format!("{}.{} has no type", config_name, section_name))
                    })?;

                    writeln!(writer, "set {}.{}={}", config_name, section_name, ty).unwrap();

                    for (option_name, option) in obj {
                        if option_name == "_type" {
                            continue;
                        }
                        let key = format!("{}.{}.{}", config_name, section_name, option_name);
                        serialize_option_val(writer, &key, option, secrets)?;
                    }
                    writeln!(writer, "EOF").unwrap();
                }
            }
        }
    }

    writeln!(writer, "uci commit").unwrap();
    Ok(())
}

fn load_secrets_dir(dir_path: &str) -> Result<HashMap<String, String>, ConfigError> {
    let dir = Path::new(dir_path);
    let mut secrets = HashMap::new();
    if !dir.is_dir() {
        return Ok(secrets);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            let sec_file = File::open(path)?;
            let parsed: Value = serde_json::from_reader(BufReader::new(sec_file))
                .map_err(|e| ConfigError(format!("Failed to parse decrypted json: {}", e)))?;

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
                    secrets.insert(k.clone(), val_str);
                }
            }
        }
    }
    Ok(secrets)
}

fn convert_file(path: &Path, secrets_dir: Option<&str>) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let root: Root = serde_json::from_reader(reader).map_err(|e| {
        ConfigError(format!(
            "Failed to parse JSON into Uci Root structure: {}",
            e
        ))
    })?;

    let mut secrets = HashMap::new();
    if let Some(dir_path) = secrets_dir {
        secrets = load_secrets_dir(dir_path)?;
    }

    let mut output_buffer = String::with_capacity(4096);
    serialize_uci(&mut output_buffer, &root.settings, &secrets)?;

    if let Some(opkg) = &root.opkg {
        if let Some(feeds) = &opkg.feeds {
            if !feeds.is_empty() {
                writeln!(
                    &mut output_buffer,
                    "\ncat << 'EOF' > /etc/opkg/customfeeds.conf"
                )
                .unwrap();
                for feed in feeds {
                    writeln!(&mut output_buffer, "{}", feed).unwrap();
                }
                writeln!(&mut output_buffer, "EOF").unwrap();
            }
        }
    }

    if let Some(pkgs) = &root.packages {
        if !pkgs.is_empty() {
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
    }

    Ok(output_buffer)
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
