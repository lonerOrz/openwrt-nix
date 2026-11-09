//! Structured UCI command model and its renderer.
//!
//! Byte-equivalence contract: [`compile_and_render`] must emit exactly what
//! `generator::serialize_uci` emits. Output format changes belong elsewhere.

use crate::config::models::{Section, SectionData};
use crate::config::uci_key::{anonymous_option_key, named_option_key};
use crate::utils::error::ConfigError;
use crate::utils::helpers::push_escaped_single_quotes;
use indexmap::IndexMap;
use serde_json::Value;
use std::fmt::Write as FmtWrite;

/// A reference to a named UCI section (`config.name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UciSectionRef {
    Named { config: String, name: String },
}

impl UciSectionRef {
    /// Stable key addressing the section root (e.g. `network.lan`).
    fn key(&self) -> String {
        match self {
            UciSectionRef::Named { config, name } => format!("{config}.{name}"),
        }
    }
}

/// A scalar UCI value: a string, an integer, a boolean, or a list element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UciScalar {
    String(String),
    Integer(i64),
    Boolean(bool),
}

/// A UCI option value. A list option renders as repeated `add_list` operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UciValue {
    Scalar(UciScalar),
    List(Vec<UciScalar>),
}

impl UciValue {
    /// Build a [`UciValue`] from JSON, rejecting the types UCI cannot store.
    fn try_from_json(val: &Value) -> Result<Self, ConfigError> {
        fn scalar(item: &Value) -> Result<UciScalar, ConfigError> {
            match item {
                Value::String(s) => Ok(UciScalar::String(s.clone())),
                Value::Number(n) => {
                    let i = n.as_i64().ok_or_else(|| {
                        ConfigError::Validation(format!(
                            "{n} is not a supported (integer) value type"
                        ))
                    })?;
                    Ok(UciScalar::Integer(i))
                }
                Value::Bool(b) => Ok(UciScalar::Boolean(*b)),
                _ => Err(ConfigError::Validation(format!(
                    "{item:?} is not a supported list value type"
                ))),
            }
        }

        match val {
            Value::String(s) => Ok(UciValue::Scalar(UciScalar::String(s.clone()))),
            Value::Number(n) => {
                let i = n.as_i64().ok_or_else(|| {
                    ConfigError::Validation(format!("{n} is not a supported value type"))
                })?;
                Ok(UciValue::Scalar(UciScalar::Integer(i)))
            }
            Value::Bool(b) => Ok(UciValue::Scalar(UciScalar::Boolean(*b))),
            Value::Array(arr) => {
                let mut elems = Vec::with_capacity(arr.len());
                for item in arr {
                    elems.push(scalar(item)?);
                }
                Ok(UciValue::List(elems))
            }
            _ => Err(ConfigError::Validation(format!(
                "{val:?} is not a supported option value type"
            ))),
        }
    }
}

/// A single UCI operation, rendered later by [`UciRenderer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UciCommand {
    /// Existence-guarded delete: a missing section is a no-op under `set -e`.
    DeleteNamed { section: UciSectionRef },
    /// `add <config> <type>` for an anonymous section.
    Add {
        config: String,
        section_type: String,
    },
    /// Named-section type assignment, emitted unquoted (a fixed identifier).
    SetSectionType { path: String, type_name: String },
    /// `set <path>=<value>`.
    Set { path: String, value: UciValue },
    /// `add_list <path>=<value>`.
    AddList { path: String, value: UciScalar },
}

/// Wipes every anonymous section of a type before it is rebuilt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UciWipe {
    pub config: String,
    pub section_type: String,
}

/// The complete UCI program for one config file: wipe block, then section body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigUciCommands {
    pub config: String,
    pub wipes: Vec<UciWipe>,
    pub body: Vec<UciCommand>,
}

/// Compile `Root.settings` into one [`ConfigUciCommands`] per config.
pub fn compile_uci_commands(
    configs: &IndexMap<String, IndexMap<String, Section>>,
) -> Result<Vec<ConfigUciCommands>, ConfigError> {
    let mut out = Vec::new();

    for (config_name, sections) in configs {
        let mut wipes: Vec<UciWipe> = Vec::new();
        let mut body: Vec<UciCommand> = Vec::new();
        let mut wiped_types: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    for list_obj in arr {
                        let ty = list_type_of(list_obj, section_name);
                        if wiped_types.insert(ty.to_string()) {
                            wipes.push(UciWipe {
                                config: config_name.clone(),
                                section_type: ty.to_string(),
                            });
                        }
                    }

                    // Anonymous sections are indexed per-type in UCI, so track a
                    // separate counter for each type rather than the global position.
                    let mut type_index: std::collections::HashMap<&str, usize> =
                        std::collections::HashMap::new();
                    for list_obj in arr.iter() {
                        let ty = list_type_of(list_obj, section_name);
                        let idx = *type_index.entry(ty).or_insert(0);
                        *type_index.get_mut(ty).unwrap() = idx + 1;

                        body.push(UciCommand::Add {
                            config: config_name.clone(),
                            section_type: ty.to_string(),
                        });

                        for (option_name, option) in &list_obj.options {
                            let key = anonymous_option_key(config_name, ty, idx, option_name);
                            let value = UciValue::try_from_json(option)?;
                            append_option(&mut body, key, value);
                        }
                    }
                }
                Section::Named(section) => {
                    let name = section_name.clone();
                    let ty = section.section_type.clone();
                    let section_ref = UciSectionRef::Named {
                        config: config_name.clone(),
                        name: name.clone(),
                    };
                    body.push(UciCommand::DeleteNamed {
                        section: section_ref,
                    });
                    body.push(UciCommand::SetSectionType {
                        path: format!("{config_name}.{name}"),
                        type_name: ty,
                    });

                    for (option_name, option) in &section.options {
                        let key = named_option_key(config_name, &name, option_name);
                        let value = UciValue::try_from_json(option)?;
                        append_option(&mut body, key, value);
                    }
                }
            }
        }

        out.push(ConfigUciCommands {
            config: config_name.clone(),
            wipes,
            body,
        });
    }

    Ok(out)
}

fn append_option(body: &mut Vec<UciCommand>, key: String, value: UciValue) {
    match value {
        UciValue::Scalar(scalar) => body.push(UciCommand::Set {
            path: key,
            value: UciValue::Scalar(scalar),
        }),
        UciValue::List(elems) => {
            for elem in elems {
                body.push(UciCommand::AddList {
                    path: key.clone(),
                    value: elem,
                });
            }
        }
    }
}

fn list_type_of<'a>(item: &'a SectionData, fallback: &'a str) -> &'a str {
    if item.section_type.is_empty() {
        fallback
    } else {
        &item.section_type
    }
}

/// Renders a value as the quoted RHS of a `set`/`add_list` assignment.
fn render_value_scalar(out: &mut String, scalar: &UciScalar) {
    match scalar {
        UciScalar::String(s) => {
            out.push('\'');
            push_escaped_single_quotes(out, s);
            out.push('\'');
        }
        UciScalar::Integer(n) => {
            let _ = write!(out, "'{n}'");
        }
        UciScalar::Boolean(b) => {
            let _ = write!(out, "'{}'", if *b { "1" } else { "0" });
        }
    }
}

/// Renders [`UciCommand::Set`]'s payload without the `uci ` prefix.
fn render_set_line(out: &mut String, path: &str, value: &UciValue) {
    match value {
        UciValue::Scalar(scalar) => {
            let _ = write!(out, "set {path}=");
            render_value_scalar(out, scalar);
            out.push('\n');
        }
        UciValue::List(elems) => {
            for elem in elems {
                let _ = write!(out, "add_list {path}=");
                render_value_scalar(out, elem);
                out.push('\n');
            }
        }
    }
}

/// Renders one config into the bytes the previous `serialize_uci` produced.
pub fn render_config(out: &mut String, cmd: &ConfigUciCommands) {
    let config_name = &cmd.config;

    let mut shell_cmds = String::new();
    for wipe in &cmd.wipes {
        let _ = writeln!(
            shell_cmds,
            "while uci -q delete {}.@{}[0]; do :; done",
            wipe.config, wipe.section_type
        );
    }

    let mut uci_cmds = String::new();
    for command in &cmd.body {
        match command {
            UciCommand::DeleteNamed { section } => {
                let key = section.key();
                let _ = writeln!(
                    uci_cmds,
                    "if uci -q get {key}; then uci -q delete {key}; fi"
                );
            }
            UciCommand::Add {
                config,
                section_type,
            } => {
                let _ = writeln!(uci_cmds, "add {config} {section_type}");
            }
            UciCommand::SetSectionType { path, type_name } => {
                let _ = writeln!(uci_cmds, "set {path}={type_name}");
            }
            UciCommand::Set { path, value } => {
                render_set_line(&mut uci_cmds, path, value);
            }
            UciCommand::AddList { path, value } => {
                let _ = write!(uci_cmds, "add_list {path}=");
                render_value_scalar(&mut uci_cmds, value);
                uci_cmds.push('\n');
            }
        }
    }

    // Invariant: shell_cmds (anonymous-section wipe loops) is only emitted
    // alongside uci_cmds, never on its own.
    debug_assert!(shell_cmds.is_empty() || !uci_cmds.is_empty());
    out.push_str(&shell_cmds);

    if !uci_cmds.is_empty() {
        // Each UCI op is emitted as its own `uci <verb>` line (not a batch
        // heredoc). `set -e` then aborts the script at the first failing op —
        // `uci batch` can swallow an inner failure and still exit 0, which would
        // wrongly report success. We `touch` the config first so the file exists
        // for `set`/`add`, then commit per-config so a failing op can't reach
        // this config's commit.
        //
        // Lines that are shell guards (`if …; then`, `fi`, …) are passed through
        // verbatim; all other lines get the `uci ` prefix.
        let _ = writeln!(out, "touch /etc/config/{config_name}");
        for line in uci_cmds.lines() {
            if line.starts_with("if ")
                || line.starts_with("then ")
                || line.starts_with("else")
                || line.starts_with("fi")
            {
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "uci {line}");
            }
        }
        let _ = writeln!(out, "uci commit {config_name}");
    }
}

/// Compile + render all configs, preserving the original ordering.
pub fn compile_and_render(
    writer: &mut String,
    configs: &IndexMap<String, IndexMap<String, Section>>,
) -> Result<(), ConfigError> {
    for cmd in compile_uci_commands(configs)? {
        render_config(writer, &cmd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::SectionData;
    use indexmap::IndexMap;

    fn named(
        config: &str,
        name: &str,
        ty: &str,
        opts: &[(&str, &str)],
    ) -> IndexMap<String, IndexMap<String, Section>> {
        let mut options = IndexMap::new();
        for (k, v) in opts {
            options.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        let mut sections = IndexMap::new();
        sections.insert(
            name.to_string(),
            Section::Named(SectionData {
                section_type: ty.to_string(),
                options,
            }),
        );
        let mut configs = IndexMap::new();
        configs.insert(config.to_string(), sections);
        configs
    }

    #[test]
    fn scalar_value_renders_quoted() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "system.hostname",
            &UciValue::Scalar(UciScalar::String("test".into())),
        );
        assert_eq!(out, "set system.hostname='test'\n");
    }

    #[test]
    fn number_value_renders_quoted() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "dhcp.start",
            &UciValue::Scalar(UciScalar::Integer(100)),
        );
        assert_eq!(out, "set dhcp.start='100'\n");
    }

    #[test]
    fn bool_true_renders_one() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "wifi.enabled",
            &UciValue::Scalar(UciScalar::Boolean(true)),
        );
        assert_eq!(out, "set wifi.enabled='1'\n");
    }

    #[test]
    fn bool_false_renders_zero() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "wifi.enabled",
            &UciValue::Scalar(UciScalar::Boolean(false)),
        );
        assert_eq!(out, "set wifi.enabled='0'\n");
    }

    #[test]
    fn list_value_renders_add_list_per_item() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "net.dns",
            &UciValue::List(vec![
                UciScalar::String("a".into()),
                UciScalar::String("b".into()),
            ]),
        );
        assert!(out.contains("add_list net.dns='a'"));
        assert!(out.contains("add_list net.dns='b'"));
    }

    #[test]
    fn quote_in_value_is_escaped() {
        let mut out = String::new();
        render_set_line(
            &mut out,
            "sys.name",
            &UciValue::Scalar(UciScalar::String("it's".into())),
        );
        assert_eq!(out, "set sys.name='it'\\''s'\n");
    }

    #[test]
    fn json_rejects_null() {
        assert!(UciValue::try_from_json(&Value::Null).is_err());
    }

    #[test]
    fn json_rejects_object() {
        let obj = serde_json::json!({"nested": "value"});
        let err = UciValue::try_from_json(&obj).unwrap_err();
        assert!(format!("{err}").contains("not a supported option value type"));
    }

    #[test]
    fn json_rejects_list_of_objects() {
        let arr = Value::Array(vec![serde_json::json!({"bad": true})]);
        let err = UciValue::try_from_json(&arr).unwrap_err();
        assert!(format!("{err}").contains("not a supported list value type"));
    }

    #[test]
    fn named_section_renders_delete_guard_and_set() {
        let cfg = named("network", "lan", "interface", &[("proto", "static")]);
        let mut out = String::new();
        compile_and_render(&mut out, &cfg).unwrap();
        assert!(out.contains("if uci -q get network.lan; then uci -q delete network.lan; fi"));
        assert!(out.contains("set network.lan=interface"));
        assert!(out.contains("set network.lan.proto='static'"));
        assert!(out.contains("uci set network.lan=interface"));
        assert!(out.contains("uci set network.lan.proto='static'"));
        assert!(out.contains("uci commit network"));
    }

    #[test]
    fn anonymous_section_per_type_index() {
        let mut sections = IndexMap::new();
        let mut rule1 = IndexMap::new();
        rule1.insert("target".to_string(), Value::String("ACCEPT".into()));
        let mut redirect = IndexMap::new();
        redirect.insert("dest".to_string(), Value::String("lan".into()));
        let mut rule2 = IndexMap::new();
        rule2.insert("target".to_string(), Value::String("DROP".into()));
        sections.insert(
            "fw".to_string(),
            Section::List(vec![
                SectionData {
                    section_type: "rule".into(),
                    options: rule1,
                },
                SectionData {
                    section_type: "redirect".into(),
                    options: redirect,
                },
                SectionData {
                    section_type: "rule".into(),
                    options: rule2,
                },
            ]),
        );
        let mut configs = IndexMap::new();
        configs.insert("firewall".to_string(), sections);

        let mut out = String::new();
        compile_and_render(&mut out, &configs).unwrap();
        assert!(out.contains("set firewall.@rule[0].target='ACCEPT'"));
        assert!(out.contains("set firewall.@redirect[0].dest='lan'"));
        assert!(out.contains("set firewall.@rule[1].target='DROP'"));
        assert!(out.contains("while uci -q delete firewall.@rule[0]; do :; done"));
        assert!(out.contains("while uci -q delete firewall.@redirect[0]; do :; done"));
        assert!(out.contains("uci commit firewall"));
    }
    /// Byte-exact check for heterogeneous multi-type output.
    #[test]
    fn heterogeneous_output_is_byte_exact() {
        let mut sections = IndexMap::new();
        let mut rule1 = IndexMap::new();
        rule1.insert("target".to_string(), Value::String("ACCEPT".into()));
        let mut redirect = IndexMap::new();
        redirect.insert("dest".to_string(), Value::String("lan".into()));
        let mut rule2 = IndexMap::new();
        rule2.insert("target".to_string(), Value::String("DROP".into()));
        sections.insert(
            "fw".to_string(),
            Section::List(vec![
                SectionData {
                    section_type: "rule".into(),
                    options: rule1,
                },
                SectionData {
                    section_type: "redirect".into(),
                    options: redirect,
                },
                SectionData {
                    section_type: "rule".into(),
                    options: rule2,
                },
            ]),
        );
        let mut configs = IndexMap::new();
        configs.insert("firewall".to_string(), sections);

        let mut out = String::new();
        compile_and_render(&mut out, &configs).unwrap();

        let expected = "\
while uci -q delete firewall.@rule[0]; do :; done
while uci -q delete firewall.@redirect[0]; do :; done
touch /etc/config/firewall
uci add firewall rule
uci set firewall.@rule[0].target='ACCEPT'
uci add firewall redirect
uci set firewall.@redirect[0].dest='lan'
uci add firewall rule
uci set firewall.@rule[1].target='DROP'
uci commit firewall
";
        assert_eq!(out, expected, "byte-exact mismatch:\n{out}");
    }

    /// Byte-exact check for a named section, including the unquoted type line.
    #[test]
    fn named_section_output_is_byte_exact() {
        let cfg = named("network", "lan", "interface", &[("proto", "static")]);
        let mut out = String::new();
        compile_and_render(&mut out, &cfg).unwrap();

        let expected = "\
touch /etc/config/network
if uci -q get network.lan; then uci -q delete network.lan; fi
uci set network.lan=interface
uci set network.lan.proto='static'
uci commit network
";
        assert_eq!(out, expected, "byte-exact mismatch:\n{out}");
    }

    /// No `uci batch` heredoc may be emitted — ops stay discrete for `set -e`.
    #[test]
    fn never_emits_batch_heredoc() {
        let cfg = named("system", "system", "system", &[("hostname", "r")]);
        let mut out = String::new();
        compile_and_render(&mut out, &cfg).unwrap();
        assert!(!out.contains("batch"));
        assert!(!out.contains("<<"));
    }
}
