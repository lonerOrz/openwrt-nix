use crate::config::models::{PackageSources, PkgBackend, Section, SectionData};
use crate::config::package::{PackageAction, RemotePath};
use crate::config::uci_key::{anonymous_option_key, named_option_key};
use crate::utils::error::ConfigError;
use crate::utils::helpers::{extract_package_name, push_escaped_single_quotes, shell_quote};
use indexmap::IndexMap;
use serde_json::Value;
use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::path::Path;

impl PkgBackend {
    pub(crate) fn is_installed_cmd(&self, pkg: &str) -> String {
        let quoted = shell_quote(pkg);
        match self {
            PkgBackend::Opkg => {
                format!("opkg status {quoted} 2>/dev/null | grep -q 'Status:.*installed'")
            }
            PkgBackend::Apk => format!("apk info -e {quoted} >/dev/null 2>&1"),
        }
    }

    pub(crate) fn install_expr(&self, pkgs: &[String]) -> String {
        debug_assert!(!pkgs.is_empty());
        let quoted: Vec<String> = pkgs.iter().map(|p| shell_quote(p)).collect();
        match self {
            PkgBackend::Opkg => format!(
                "if [ \"$NEED_INSTALL\" = true ]; then\n\
                 for __nuci_try in 1 2 3 4 5; do\n\
                 {{ opkg update && opkg install {}; }} && break\n\
                 sleep 1\n\
                 done\nfi",
                quoted.join(" ")
            ),
            PkgBackend::Apk => format!(
                "if [ \"$NEED_INSTALL\" = true ]; then\n\
                 for __nuci_try in 1 2 3 4 5; do\n\
                 apk update && apk add {} && break\n\
                 sleep 1\n\
                 done\nfi",
                quoted.join(" ")
            ),
        }
    }

    pub(crate) fn remove_expr(&self, pkgs: &[String]) -> String {
        debug_assert!(!pkgs.is_empty());
        let quoted: Vec<String> = pkgs.iter().map(|p| shell_quote(p)).collect();
        match self {
            PkgBackend::Opkg => format!(
                "if [ \"$NEED_REMOVE\" = true ]; then\n\
                 __nuci_rc=1\n\
                 for __nuci_try in 1 2 3 4 5; do\n\
                 opkg remove {} && {{ __nuci_rc=0; break; }}\n\
                 sleep 1\n\
                 done\n\
                 if [ $__nuci_rc -ne 0 ]; then exit 1; fi\nfi",
                quoted.join(" ")
            ),
            PkgBackend::Apk => format!(
                "if [ \"$NEED_REMOVE\" = true ]; then\n\
                 __nuci_rc=1\n\
                 for __nuci_try in 1 2 3 4 5; do\n\
                 apk del {} && {{ __nuci_rc=0; break; }}\n\
                 sleep 1\n\
                 done\n\
                 if [ $__nuci_rc -ne 0 ]; then exit 1; fi\nfi",
                quoted.join(" ")
            ),
        }
    }

    pub(crate) fn local_install_block(&self, pkg_name: &str, remote: &RemotePath) -> String {
        let probe = self.is_installed_cmd(pkg_name);
        let path = shell_quote(remote.as_str());
        match self {
            PkgBackend::Opkg => {
                format!("\nif ! {probe}; then\n    opkg install {path}\nfi")
            }
            // If extract_package_name fails, probe stays false and apk reinstalls
            // every deploy — safe, just not idempotent.
            PkgBackend::Apk => {
                format!("\nif ! {probe}; then\n    apk add --allow-untrusted {path}\nfi")
            }
        }
    }

    pub(crate) fn feed_lines(&self, feeds: &[String]) -> String {
        match self {
            PkgBackend::Opkg => {
                let mut out = String::from("\nprintf '' > /etc/opkg/customfeeds.conf");
                for feed in feeds {
                    out.push_str("\nprintf '%s\\n' '");
                    push_escaped_single_quotes(&mut out, feed);
                    out.push_str("' >> /etc/opkg/customfeeds.conf");
                }
                out
            }
            PkgBackend::Apk => {
                let mut out = String::from(
                    "\nmkdir -p /etc/apk/repositories.d\nprintf '' > /etc/apk/repositories.d/customfeeds.list",
                );
                for feed in feeds {
                    out.push_str("\nprintf '%s\\n' '");
                    push_escaped_single_quotes(&mut out, feed);
                    out.push_str("' >> /etc/apk/repositories.d/customfeeds.list");
                }
                out
            }
        }
    }
}

/// `opkgFeeds` is only valid on an opkg target; `apkRepositories` only on an apk
/// target. Supplying a field for the wrong backend is a configuration error (the
/// two are mutually exclusive per target). An empty list means "no custom sources".
fn resolve_feeds(
    backend: PkgBackend,
    sources: &PackageSources,
) -> Result<Option<Vec<String>>, ConfigError> {
    match backend {
        PkgBackend::Opkg => {
            if sources
                .apk_repositories
                .as_ref()
                .is_some_and(|r| !r.is_empty())
            {
                return Err(ConfigError::Validation(
                    "packageSources.apkRepositories is set but the target uses opkg; remove it \
                     (opkg targets use packageSources.opkgFeeds)"
                        .into(),
                ));
            }
            Ok(sources.opkg_feeds.clone())
        }
        PkgBackend::Apk => {
            if sources.opkg_feeds.as_ref().is_some_and(|f| !f.is_empty()) {
                return Err(ConfigError::Validation(
                    "packageSources.opkgFeeds is set but the target uses apk; remove it (apk \
                     targets use packageSources.apkRepositories)"
                        .into(),
                ));
            }
            Ok(sources.apk_repositories.clone())
        }
    }
}

fn serialize_option_val(writer: &mut String, key: &str, val: &Value) -> Result<(), ConfigError> {
    match val {
        Value::String(s) => {
            write!(writer, "set {key}='").unwrap();
            push_escaped_single_quotes(writer, s);
            writeln!(writer, "'").unwrap();
        }
        Value::Number(n) => {
            writeln!(writer, "set {key}='{n}'").unwrap();
        }
        Value::Bool(b) => {
            let bool_str = if *b { "1" } else { "0" };
            writeln!(writer, "set {key}='{bool_str}'").unwrap();
        }
        Value::Array(arr) => {
            for item in arr {
                let s = match item {
                    Value::String(s) => Cow::Borrowed(s.as_str()),
                    Value::Number(n) => Cow::Owned(n.to_string()),
                    Value::Bool(b) => Cow::Borrowed(if *b { "1" } else { "0" }),
                    _ => {
                        return Err(ConfigError::Validation(format!(
                            "{:?} is not a supported list value type",
                            item
                        )));
                    }
                };
                write!(writer, "add_list {key}='").unwrap();
                push_escaped_single_quotes(writer, &s);
                writeln!(writer, "'").unwrap();
            }
        }
        _ => {
            return Err(ConfigError::Validation(format!(
                "{:?} is not a supported option value type",
                val
            )));
        }
    }
    Ok(())
}

fn list_type_of<'a>(item: &'a SectionData, fallback: &'a str) -> &'a str {
    if item.section_type.is_empty() {
        fallback
    } else {
        &item.section_type
    }
}

pub(crate) fn serialize_uci(
    writer: &mut String,
    configs: &IndexMap<String, IndexMap<String, Section>>,
) -> Result<(), ConfigError> {
    for (config_name, sections) in configs {
        let mut shell_cmds = String::new();
        let mut uci_cmds = String::new();

        for (section_name, section) in sections {
            match section {
                Section::List(arr) => {
                    let mut wiped_types = std::collections::HashSet::new();
                    for list_obj in arr {
                        let list_ty = list_type_of(list_obj, section_name);
                        if wiped_types.insert(list_ty) {
                            writeln!(
                                shell_cmds,
                                "while uci -q delete {config_name}.@{list_ty}[0]; do :; done"
                            )
                            .unwrap();
                        }
                    }

                    // Anonymous sections are indexed per-type in UCI, so track a
                    // separate counter for each type rather than the global position.
                    let mut type_index: std::collections::HashMap<&str, usize> =
                        std::collections::HashMap::new();
                    for list_obj in arr.iter() {
                        let ty = list_type_of(list_obj, section_name);
                        let idx = *type_index.entry(ty).or_insert(0);
                        type_index.insert(ty, idx + 1);
                        writeln!(uci_cmds, "add {config_name} {ty}").unwrap();

                        for (option_name, option) in &list_obj.options {
                            let key = anonymous_option_key(config_name, ty, idx, option_name);
                            serialize_option_val(&mut uci_cmds, &key, option)?;
                        }
                    }
                }
                Section::Named(section) => {
                    let ty = &section.section_type;
                    // `-q` keeps delete idempotent: a missing named section must
                    // not abort the script under `set -e` (the target may not
                    // yet have the section). Anonymous wipes above already use -q.
                    writeln!(uci_cmds, "delete -q {config_name}.{section_name}").unwrap();
                    writeln!(uci_cmds, "set {config_name}.{section_name}={ty}").unwrap();

                    for (option_name, option) in &section.options {
                        let key = named_option_key(config_name, section_name, option_name);
                        serialize_option_val(&mut uci_cmds, &key, option)?;
                    }
                }
            }
        }

        // Invariant: shell_cmds (anonymous-section wipe loops) is only emitted
        // alongside uci_cmds, never on its own.
        debug_assert!(shell_cmds.is_empty() || !uci_cmds.is_empty());
        write!(writer, "{shell_cmds}").unwrap();

        if !uci_cmds.is_empty() {
            // Each UCI op is emitted as its own `uci <verb>` line (not a batch
            // heredoc). `set -e` then aborts the script at the first failing
            // command — `uci batch` can swallow an inner failure and still exit
            // 0, which would wrongly report success. We `touch` the config first
            // so the file exists for `set`/`add`, then commit per-config so a
            // failing op can't reach this config's commit.
            writeln!(writer, "touch /etc/config/{config_name}").unwrap();
            for line in uci_cmds.lines() {
                writeln!(writer, "uci {line}").unwrap();
            }
            writeln!(writer, "uci commit {config_name}").unwrap();
        }
    }

    Ok(())
}

pub(crate) fn serialize_package_management(
    writer: &mut String,
    backend: PkgBackend,
    sources: Option<&PackageSources>,
    packages: Option<&[String]>,
) -> Result<(), ConfigError> {
    // Configure package feeds BEFORE install/remove so the manager can resolve
    // the configured repositories. The apk/opkg test images clear their default
    // distfeeds, so a configured feed must be on disk before `apk add`/`opkg
    // install` runs — otherwise the fetch fails with no repositories configured.
    let mut wrote_feeds = false;
    if let Some(src_val) = sources
        && let Some(feeds) = resolve_feeds(backend, src_val)?
        && !feeds.is_empty()
    {
        writeln!(writer, "{}", backend.feed_lines(&feeds)).unwrap();
        wrote_feeds = true;
    }

    if wrote_feeds {
        // Refresh the index after custom feeds/repos are written, so local
        // packages depending on feed-provided packages resolve. Idempotent.
        writeln!(
            writer,
            "{}",
            match backend {
                PkgBackend::Opkg => "opkg update",
                PkgBackend::Apk => "apk update",
            }
        )
        .unwrap();
    }

    if let Some(src_val) = sources
        && let Some(local_pkgs) = &src_val.local_packages
    {
        for ipk_path_str in local_pkgs {
            let file_name = Path::new(ipk_path_str)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    ConfigError::Validation(format!(
                        "Invalid localPackages entry '{ipk_path_str}': not a file path"
                    ))
                })?;
            let remote = RemotePath::tmp(file_name)?;
            let pkg_name = extract_package_name(file_name);
            writeln!(writer, "{}", backend.local_install_block(pkg_name, &remote)).unwrap();
        }
    }

    if let Some(pkgs) = packages
        && !pkgs.is_empty()
    {
        let actions: Vec<PackageAction> = pkgs
            .iter()
            .map(|p| PackageAction::parse(p))
            .collect::<Result<_, _>>()?;
        let (removes, installs): (Vec<PackageAction>, Vec<PackageAction>) =
            actions.into_iter().partition(|a| a.is_remove());

        if !removes.is_empty() {
            let names: Vec<String> = removes.iter().map(|a| a.name().to_string()).collect();
            let quoted_names: Vec<String> = removes.iter().map(|a| a.quoted_name()).collect();

            writeln!(writer, "\nNEED_REMOVE=false").unwrap();
            writeln!(writer, "for pkg in {}; do", quoted_names.join(" ")).unwrap();
            match backend {
                PkgBackend::Opkg => writeln!(
                    writer,
                    "    if opkg status \"$pkg\" 2>/dev/null | grep -q 'Status:.*installed'; then NEED_REMOVE=true; break; fi"
                ).unwrap(),
                PkgBackend::Apk => writeln!(
                    writer,
                    "    if apk info -e \"$pkg\" >/dev/null 2>&1; then NEED_REMOVE=true; break; fi"
                ).unwrap(),
            }
            writeln!(writer, "done").unwrap();
            writeln!(writer, "{}", backend.remove_expr(&names)).unwrap();
        }

        if !installs.is_empty() {
            let names: Vec<String> = installs.iter().map(|a| a.name().to_string()).collect();
            let quoted_names: Vec<String> = installs.iter().map(|a| a.quoted_name()).collect();

            writeln!(writer, "\nNEED_INSTALL=false").unwrap();
            writeln!(writer, "for pkg in {}; do", quoted_names.join(" ")).unwrap();
            match backend {
                PkgBackend::Opkg => writeln!(
                    writer,
                    "    if ! opkg status \"$pkg\" 2>/dev/null | grep -q 'Status:.*installed'; then NEED_INSTALL=true; break; fi"
                ).unwrap(),
                PkgBackend::Apk => writeln!(
                    writer,
                    "    if ! apk info -e \"$pkg\" >/dev/null 2>&1; then NEED_INSTALL=true; break; fi"
                ).unwrap(),
            };
            writeln!(writer, "done").unwrap();
            writeln!(writer, "{}", backend.install_expr(&names)).unwrap();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::SectionData;

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
        assert_eq!(w, "set wifi.enabled='1'\n");
    }

    #[test]
    fn serialize_bool_false_val() {
        let mut w = String::new();
        serialize_option_val(&mut w, "wifi.enabled", &Value::Bool(false)).unwrap();
        assert_eq!(w, "set wifi.enabled='0'\n");
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
        assert!(format!("{err}").contains("not a supported option value type"));
    }

    #[test]
    fn serialize_array_with_nested_object_errors() {
        let mut w = String::new();
        let arr = Value::Array(vec![serde_json::json!({"bad": true})]);
        let err = serialize_option_val(&mut w, "key", &arr).unwrap_err();
        assert!(format!("{err}").contains("not a supported list value type"));
    }

    #[test]
    fn serialize_null_val_errors() {
        let mut w = String::new();
        let err = serialize_option_val(&mut w, "key", &Value::Null).unwrap_err();
        assert!(format!("{err}").contains("not a supported option value type"));
    }

    #[test]
    fn serialize_with_quote_escaping() {
        let mut w = String::new();
        let val = Value::String("it's".into());
        serialize_option_val(&mut w, "sys.name", &val).unwrap();
        assert_eq!(w, "set sys.name='it'\\''s'\n");
    }

    #[test]
    fn serialize_named_section() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut options = IndexMap::new();
        options.insert("proto".into(), Value::String("static".into()));
        sections.insert(
            "lan".into(),
            Section::Named(SectionData {
                section_type: "interface".into(),
                options,
            }),
        );
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("uci delete -q network.lan"));
        assert!(w.contains("set network.lan=interface"));
        assert!(w.contains("set network.lan.proto='static'"));
        assert!(w.contains("uci set network.lan=interface"));
        assert!(w.contains("uci set network.lan.proto='static'"));
        assert!(w.contains("uci commit network"));
        assert!(
            !w.contains("uci batch"),
            "commands must be discrete, not batched"
        );
    }

    #[test]
    fn serialize_list_section() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut options = IndexMap::new();
        options.insert("Port".into(), Value::String("22".into()));
        sections.insert(
            "dropbear".into(),
            Section::List(vec![SectionData {
                section_type: "dropbear".into(),
                options,
            }]),
        );
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("add dropbear dropbear"));
        assert!(w.contains("set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("uci add dropbear dropbear"));
        assert!(w.contains("uci set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("uci commit dropbear"));
        assert!(!w.contains("uci batch"));
    }

    #[test]
    fn serialize_uci_emits_discrete_commands() {
        // Discrete `uci <verb>` lines (not a batch heredoc) so a deploy script
        // under `set -e` aborts at the first failing command.
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut options = IndexMap::new();
        options.insert("proto".into(), Value::String("static".into()));
        sections.insert(
            "lan".into(),
            Section::Named(SectionData {
                section_type: "interface".into(),
                options,
            }),
        );
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        let lines: Vec<&str> = w.lines().collect();
        let set_line = lines
            .iter()
            .find(|l| l.contains("uci set network.lan=interface"))
            .expect("set line present");
        let commit_line = lines
            .iter()
            .find(|l| l.contains("uci commit network"))
            .expect("commit line present");
        assert!(
            lines.iter().position(|l| l == set_line).unwrap()
                < lines.iter().position(|l| l == commit_line).unwrap(),
            "commit must follow set within the same config"
        );
        assert!(!w.contains("uci batch"), "must not batch");
    }

    #[test]
    fn serialize_named_delete_is_quiet() {
        // Named-section delete uses -q so a missing section doesn't abort the
        // script under `set -e`.
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "lan".into(),
            Section::Named(SectionData {
                section_type: "interface".into(),
                options: IndexMap::new(),
            }),
        );
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();
        assert!(w.contains("uci delete -q network.lan"));
    }

    #[test]
    fn serialize_heterogeneous_list_wipes_each_type() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "firewall".into(),
            Section::List(vec![
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("ACCEPT".into()))]),
                },
                SectionData {
                    section_type: "redirect".into(),
                    options: IndexMap::from([("dest".into(), Value::String("lan".into()))]),
                },
            ]),
        );
        configs.insert("firewall".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete firewall.@rule[0]; do :; done"));
        assert!(w.contains("while uci -q delete firewall.@redirect[0]; do :; done"));
        assert!(w.contains("add firewall rule"));
        assert!(w.contains("add firewall redirect"));
        assert!(w.contains("set firewall.@rule[0]"));
        assert!(w.contains("set firewall.@redirect[0]"));
    }

    #[test]
    fn serialize_heterogeneous_list_indexes_per_type() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "firewall".into(),
            Section::List(vec![
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("ACCEPT".into()))]),
                },
                SectionData {
                    section_type: "redirect".into(),
                    options: IndexMap::from([("dest".into(), Value::String("lan".into()))]),
                },
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("DROP".into()))]),
                },
            ]),
        );
        configs.insert("firewall".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("set firewall.@rule[0]"));
        assert!(w.contains("set firewall.@redirect[0]"));
        assert!(w.contains("set firewall.@rule[1]"));
        assert!(!w.contains("@rule[2]"));
    }

    #[test]
    fn serialize_heterogeneous_list_reversed_order() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "firewall".into(),
            Section::List(vec![
                SectionData {
                    section_type: "redirect".into(),
                    options: IndexMap::from([("dest".into(), Value::String("lan".into()))]),
                },
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("ACCEPT".into()))]),
                },
                SectionData {
                    section_type: "redirect".into(),
                    options: IndexMap::from([("dest".into(), Value::String("wan".into()))]),
                },
            ]),
        );
        configs.insert("firewall".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("set firewall.@redirect[0]"));
        assert!(w.contains("set firewall.@rule[0]"));
        assert!(w.contains("set firewall.@redirect[1]"));
    }

    #[test]
    fn serialize_homogeneous_list_counts_from_zero() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "firewall".into(),
            Section::List(vec![
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("ACCEPT".into()))]),
                },
                SectionData {
                    section_type: "rule".into(),
                    options: IndexMap::from([("target".into(), Value::String("DROP".into()))]),
                },
            ]),
        );
        configs.insert("firewall".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("set firewall.@rule[0]"));
        assert!(w.contains("set firewall.@rule[1]"));
        assert!(!w.contains("@rule[2]"));
    }

    #[test]
    fn serialize_heterogeneous_list_is_deterministic() {
        let build = || {
            let mut configs = IndexMap::new();
            let mut sections = IndexMap::new();
            sections.insert(
                "firewall".into(),
                Section::List(vec![
                    SectionData {
                        section_type: "rule".into(),
                        options: IndexMap::from([(
                            "target".into(),
                            Value::String("ACCEPT".into()),
                        )]),
                    },
                    SectionData {
                        section_type: "redirect".into(),
                        options: IndexMap::from([("dest".into(), Value::String("lan".into()))]),
                    },
                    SectionData {
                        section_type: "rule".into(),
                        options: IndexMap::from([("target".into(), Value::String("DROP".into()))]),
                    },
                ]),
            );
            configs.insert("firewall".into(), sections);
            configs
        };

        let mut a = String::new();
        serialize_uci(&mut a, &build()).unwrap();
        let mut b = String::new();
        serialize_uci(&mut b, &build()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn serialize_named_section_empty_type_succeeds() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "lan".into(),
            Section::Named(SectionData {
                section_type: String::new(),
                options: IndexMap::new(),
            }),
        );
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();
        assert!(w.contains("set network.lan="));
    }

    #[test]
    fn serialize_list_section_empty_type_succeeds() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        sections.insert(
            "dropbear".into(),
            Section::List(vec![SectionData {
                section_type: String::new(),
                options: IndexMap::new(),
            }]),
        );
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();
        assert!(w.contains("add dropbear "));
    }

    #[test]
    fn serialize_multiple_list_items() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut opts1 = IndexMap::new();
        opts1.insert("Port".into(), Value::String("22".into()));
        let mut opts2 = IndexMap::new();
        opts2.insert("Port".into(), Value::String("2222".into()));
        sections.insert(
            "dropbear".into(),
            Section::List(vec![
                SectionData {
                    section_type: "dropbear".into(),
                    options: opts1,
                },
                SectionData {
                    section_type: "dropbear".into(),
                    options: opts2,
                },
            ]),
        );
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert_eq!(w.matches("add dropbear dropbear").count(), 2);
        assert!(w.contains("set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("set dropbear.@dropbear[1].Port='2222'"));
    }

    #[test]
    fn serialize_list_section_type_mismatch() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut options = IndexMap::new();
        options.insert("proto".into(), Value::String("static".into()));
        sections.insert(
            "interfaces".into(),
            Section::List(vec![SectionData {
                section_type: "interface".into(),
                options,
            }]),
        );
        configs.insert("network".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete network.@interface[0]; do :; done"));
        assert!(w.contains("add network interface"));
        assert!(w.contains("set network.@interface[0].proto='static'"));
    }

    #[test]
    fn test_serialize_opkg_empty() {
        let mut w = String::new();
        serialize_package_management(&mut w, PkgBackend::Opkg, None, None).unwrap();
        assert!(w.is_empty());
    }

    #[test]
    fn test_serialize_opkg_feeds_opkg() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: Some(vec!["src/gz custom 'test' https://example.com".into()]),
            apk_repositories: None,
            local_packages: None,
        };
        serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None).unwrap();
        assert!(w.contains("/etc/opkg/customfeeds.conf"));
        assert!(w.contains("printf '%s\\n' 'src/gz custom '\\''test'\\'' https://example.com'"));
    }

    #[test]
    fn test_apk_repositories_written_on_apk() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: Some(vec!["https://example.com/packages".into()]),
            local_packages: None,
        };
        serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None).unwrap();
        assert!(w.contains("/etc/apk/repositories.d/customfeeds.list"));
        assert!(w.contains("printf '%s\\n' 'https://example.com/packages'"));
    }

    #[test]
    fn test_opkg_feeds_rejected_on_apk_target() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: Some(vec!["src/gz custom https://example.com".into()]),
            apk_repositories: None,
            local_packages: None,
        };
        let err = serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None)
            .unwrap_err();
        assert!(format!("{err}").contains("opkgFeeds is set but the target uses apk"));
    }

    #[test]
    fn test_apk_repositories_rejected_on_opkg_target() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: Some(vec!["https://example.com/packages".into()]),
            local_packages: None,
        };
        let err = serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None)
            .unwrap_err();
        assert!(format!("{err}").contains("apkRepositories is set but the target uses opkg"));
    }

    #[test]
    fn test_no_feeds_writes_nothing() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: None,
            local_packages: None,
        };
        serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None).unwrap();
        assert!(!w.contains("customfeeds"));
    }

    #[test]
    fn test_empty_source_lists_are_allowed_on_either_backend() {
        // The Nix module always emits both lists (empty by default); an empty list
        // for the other backend must not be treated as a misconfiguration.
        let sources = PackageSources {
            opkg_feeds: Some(vec![]),
            apk_repositories: Some(vec![]),
            local_packages: None,
        };
        for backend in [PkgBackend::Opkg, PkgBackend::Apk] {
            let mut w = String::new();
            serialize_package_management(&mut w, backend, Some(&sources), None).unwrap();
            assert!(!w.contains("customfeeds"));
        }
    }

    #[test]
    fn test_serialize_opkg_packages_opkg() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_package_management(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("opkg status"));
        assert!(w.contains("opkg update && opkg install 'luci' 'tcpdump'"));
    }

    #[test]
    fn test_serialize_opkg_packages_apk() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_package_management(&mut w, PkgBackend::Apk, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("apk info -e"));
        assert!(w.contains("apk add 'luci' 'tcpdump'"));
        assert!(w.contains("apk update && apk add 'luci' 'tcpdump'"));
    }

    #[test]
    fn test_serialize_remove_before_install_opkg() {
        let mut w = String::new();
        let pkgs = vec!["-wpad-basic-mbedtls".into(), "wpad-mbedtls".into()];
        serialize_package_management(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_REMOVE=false"));
        assert!(w.contains("opkg remove 'wpad-basic-mbedtls'"));
        assert!(w.contains("opkg update && opkg install 'wpad-mbedtls'"));
        assert!(
            w.find("opkg remove").unwrap() < w.find("opkg update").unwrap(),
            "removal must be emitted before install"
        );
        assert!(!w.contains("opkg install -wpad"));
    }

    #[test]
    fn test_serialize_remove_before_install_apk() {
        let mut w = String::new();
        let pkgs = vec!["-wpad-basic-mbedtls".into(), "wpad-mbedtls".into()];
        serialize_package_management(&mut w, PkgBackend::Apk, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_REMOVE=false"));
        assert!(w.contains("apk del 'wpad-basic-mbedtls'"));
        assert!(w.contains("apk add 'wpad-mbedtls'"));
        assert!(
            w.find("apk del").unwrap() < w.find("apk add").unwrap(),
            "removal must be emitted before install"
        );
        assert!(!w.contains("apk add -wpad"));
    }

    #[test]
    fn test_serialize_remove_only_skips_install_block() {
        let mut w = String::new();
        let pkgs = vec!["-wpad-basic-mbedtls".into()];
        serialize_package_management(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_REMOVE=false"));
        assert!(w.contains("opkg remove 'wpad-basic-mbedtls'"));
        assert!(!w.contains("NEED_INSTALL"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_opkg() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: None,

            local_packages: Some(vec!["./packages/test_1.0_all.ipk".into()]),
        };
        serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None).unwrap();
        assert!(w.contains("opkg status 'test'"));
        assert!(w.contains("opkg install '/tmp/test_1.0_all.ipk'"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_apk() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: None,

            local_packages: Some(vec!["./packages/test_1.0_all.apk".into()]),
        };
        serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None).unwrap();
        assert!(w.contains("if ! apk info -e 'test' >/dev/null 2>&1; then"));
        assert!(w.contains("apk add --allow-untrusted '/tmp/test_1.0_all.apk'"));
    }

    #[test]
    fn serialize_local_package_with_traversal_filename_errors() {
        let mut w = String::new();
        let sources = PackageSources {
            opkg_feeds: None,
            apk_repositories: None,
            local_packages: Some(vec!["a b.ipk".into()]),
        };
        let res = serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None);
        assert!(res.is_err());
        assert!(format!("{res:?}").contains("Invalid local package file name"));
    }

    #[test]
    fn serialize_list_rebuilds_every_item() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut options = IndexMap::new();
        options.insert("Port".into(), Value::String("22".into()));
        sections.insert(
            "dropbear".into(),
            Section::List(vec![SectionData {
                section_type: "dropbear".into(),
                options,
            }]),
        );
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete dropbear.@dropbear[0]; do :; done"));
        let add_count = w.matches("add dropbear dropbear").count();
        assert_eq!(add_count, 1);
    }

    #[test]
    fn install_expr_quotes_every_package_name() {
        let normal = PkgBackend::Apk.install_expr(&["luci".into(), "tcpdump".into()]);
        assert!(normal.contains("apk add 'luci' 'tcpdump'"));

        let malicious = PkgBackend::Apk.install_expr(&["$(touch /tmp/pwned)".into(), "a;b".into()]);
        // Each name must be wrapped in single quotes and escaped, never interpolated bare.
        assert!(malicious.contains("'$(touch /tmp/pwned)'"));
        assert!(malicious.contains("'a;b'"));
        // The payload is wrapped in single quotes, so it cannot be executed by the shell.
        assert!(!malicious.contains("; touch"));
    }

    #[test]
    fn serialize_array_bool_val() {
        let mut w = String::new();
        let arr = Value::Array(vec![Value::Bool(true), Value::Bool(false)]);
        serialize_option_val(&mut w, "net.flag", &arr).unwrap();
        assert!(w.contains("add_list net.flag='1'"));
        assert!(w.contains("add_list net.flag='0'"));
    }

    #[test]
    fn is_installed_cmd_quotes_package_name() {
        let opkg = PkgBackend::Opkg.is_installed_cmd("$(touch x)");
        assert!(opkg.contains("opkg status '$(touch x)'"));
        let apk = PkgBackend::Apk.is_installed_cmd("a;b");
        assert!(apk.contains("apk info -e 'a;b'"));
        assert!(!opkg.contains("; touch"));
    }
}
