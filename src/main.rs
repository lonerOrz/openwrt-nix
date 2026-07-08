use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::thread;

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
    secrets: Option<Secrets>,
}

#[derive(Deserialize, Debug)]
struct Secrets {
    sops: Option<Sops>,
}

#[derive(Deserialize, Debug)]
struct Sops {
    files: Option<Vec<String>>,
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
    if !option_val.contains('@') {
        return Ok(Cow::Borrowed(option_val));
    }

    let mut result = String::with_capacity(option_val.len());
    let mut last_pos = 0;

    while let Some(start) = option_val[last_pos..].find('@') {
        let absolute_start = last_pos + start;
        result.push_str(&option_val[last_pos..absolute_start]);

        let remaining = &option_val[absolute_start + 1..];
        if let Some(end) = remaining.find('@') {
            let secret_name = &remaining[..end];
            if let Some(secret_val) = secrets.get(secret_name) {
                result.push_str(secret_val);
            } else {
                return Err(ConfigError(format!(
                    "Tried to use secret {}, but no secret with this name specified.",
                    secret_name
                )));
            }
            last_pos = absolute_start + 1 + end + 1;
        } else {
            result.push('@');
            last_pos = absolute_start + 1;
        }
    }
    result.push_str(&option_val[last_pos..]);
    Ok(Cow::Owned(result))
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
            writeln!(writer, "set {}='{}'", key, interpolated).unwrap();
        }
        Value::Number(n) => {
            let s = n.to_string();
            let interpolated = interpolate_secrets(&s, secrets)?;
            writeln!(writer, "set {}='{}'", key, interpolated).unwrap();
        }
        Value::Bool(b) => {
            let s = b.to_string();
            let interpolated = interpolate_secrets(&s, secrets)?;
            writeln!(writer, "set {}='{}'", key, interpolated).unwrap();
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
                        )))
                    }
                };
                let interpolated = interpolate_secrets(&s, secrets)?;
                writeln!(writer, "add_list {}='{}'", key, interpolated).unwrap();
            }
        }
        _ => {
            return Err(ConfigError(format!(
                "{:?} is not a supported option value type",
                val
            )))
        }
    }
    Ok(())
}

fn serialize_uci(
    writer: &mut String,
    configs: &HashMap<String, HashMap<String, Section>>,
    secrets: &HashMap<String, String>,
) -> Result<(), ConfigError> {
    for (config_name, sections) in configs {
        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    for _ in 0..10 {
                        writeln!(writer, "delete {}.@{}[0]", config_name, section_name).unwrap();
                    }
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
                }
                Section::Named(obj) => {
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
                }
            }
        }
    }
    Ok(())
}

fn load_sops_file(file_path: &str) -> Result<HashMap<String, String>, ConfigError> {
    let output = Command::new("sops")
        .args(["-d", "--output-type", "json", file_path])
        .output()
        .map_err(|e| ConfigError(format!("Failed to run sops command: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConfigError(format!(
            "Cannot decrypt '{}' with sops:\n{}",
            file_path, stderr
        )));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        ConfigError(format!(
            "Failed to parse sops output for '{}': {}",
            file_path, e
        ))
    })?;

    let mut secrets = HashMap::new();
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
    Ok(secrets)
}

fn convert_file(path: &Path) -> Result<String, ConfigError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let root: Root = serde_json::from_reader(reader).map_err(|e| {
        ConfigError(format!(
            "Failed to parse JSON into Uci Root structure: {}",
            e
        ))
    })?;

    let mut secrets = HashMap::new();
    if let Some(sec) = root.secrets {
        if let Some(sops) = sec.sops {
            if let Some(files) = sops.files {
                let mut handles = vec![];
                for file_path in files {
                    let handle = thread::spawn(move || load_sops_file(&file_path));
                    handles.push(handle);
                }

                for handle in handles {
                    let res = handle.join().map_err(|_| {
                        ConfigError("Thread panicked during sops decryption".to_string())
                    })??;
                    secrets.extend(res);
                }
            }
        }
    }

    let mut output_buffer = String::with_capacity(4096);
    serialize_uci(&mut output_buffer, &root.settings, &secrets)?;
    Ok(output_buffer)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("USAGE: {} JSON_FILE", args[0]);
        std::process::exit(1);
    }

    match convert_file(Path::new(&args[1])) {
        Ok(output) => print!("{}", output),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}
