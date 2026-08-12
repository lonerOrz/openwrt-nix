use crate::config::models::{PackageAction, PackageSources, PkgBackend, Section, SectionData};
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
        match self {
            PkgBackend::Opkg => {
                format!("opkg status {pkg} 2>/dev/null | grep -q 'Status:.*installed'")
            }
            PkgBackend::Apk => format!("apk info -e {pkg} >/dev/null 2>&1"),
        }
    }

    pub(crate) fn install_expr(&self, pkgs: &[String]) -> String {
        match self {
            PkgBackend::Opkg => format!(
                "if [ \"$NEED_INSTALL\" = true ]; then opkg update && opkg install {}; fi",
                pkgs.join(" ")
            ),
            PkgBackend::Apk => format!(
                "if [ \"$NEED_INSTALL\" = true ]; then apk add {}; fi",
                pkgs.join(" ")
            ),
        }
    }

    pub(crate) fn remove_expr(&self, pkgs: &[String]) -> String {
        let quoted: Vec<String> = pkgs.iter().map(|p| shell_quote(p)).collect();
        match self {
            PkgBackend::Opkg => format!(
                "if [ \"$NEED_REMOVE\" = true ]; then opkg remove {}; \
                 if [ $? -ne 0 ]; then exit 1; fi; fi",
                quoted.join(" ")
            ),
            PkgBackend::Apk => format!(
                "if [ \"$NEED_REMOVE\" = true ]; then apk del {}; \
                 if [ $? -ne 0 ]; then exit 1; fi; fi",
                quoted.join(" ")
            ),
        }
    }

    pub(crate) fn local_install_block(&self, pkg_name: &str, file_name: &str) -> String {
        let probe = self.is_installed_cmd(pkg_name);
        match self {
            PkgBackend::Opkg => {
                format!("\nif ! {probe}; then\n    opkg install /tmp/{file_name}\nfi")
            }
            // ponytail: if extract_package_name fails, probe stays false and apk reinstalls
            // every deploy — safe, just not idempotent.
            PkgBackend::Apk => {
                format!("\nif ! {probe}; then\n    apk add --allow-untrusted /tmp/{file_name}\nfi")
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
                    Value::Bool(b) => Cow::Owned(b.to_string()),
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
                    // Heterogeneous lists: wipe every distinct type that appears
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

                    for (idx, list_obj) in arr.iter().enumerate() {
                        let ty = list_type_of(list_obj, section_name);
                        writeln!(uci_cmds, "add {config_name} {ty}").unwrap();

                        for (option_name, option) in &list_obj.options {
                            let key = anonymous_option_key(config_name, ty, idx, option_name);
                            serialize_option_val(&mut uci_cmds, &key, option)?;
                        }
                    }
                }
                Section::Named(section) => {
                    let ty = &section.section_type;
                    writeln!(uci_cmds, "delete {config_name}.{section_name}").unwrap();
                    writeln!(uci_cmds, "set {config_name}.{section_name}={ty}").unwrap();

                    for (option_name, option) in &section.options {
                        let key = named_option_key(config_name, section_name, option_name);
                        serialize_option_val(&mut uci_cmds, &key, option)?;
                    }
                }
            }
        }

        write!(writer, "{shell_cmds}").unwrap();

        if !uci_cmds.is_empty() {
            // Unrecoverable OpenWrt API limitation: `uci batch` silently fails
            // for set/add commands when /etc/config/<file> does not exist on disk.
            writeln!(writer, "touch /etc/config/{config_name}").unwrap();
            writeln!(writer, "uci -q batch <<'UCI_EOF'").unwrap();
            write!(writer, "{uci_cmds}").unwrap();
            writeln!(writer, "commit {config_name}").unwrap();
            writeln!(writer, "UCI_EOF").unwrap();
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
    if let Some(pkgs) = packages
        && !pkgs.is_empty()
    {
        let actions: Vec<PackageAction> = pkgs.iter().map(|p| PackageAction::parse(p)).collect();
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

    if let Some(src_val) = sources
        && let Some(local_pkgs) = &src_val.local_packages
    {
        for ipk_path_str in local_pkgs {
            let ipk_path = Path::new(ipk_path_str);
            if let Some(file_name) = ipk_path.file_name().and_then(|n| n.to_str()) {
                let pkg_name = extract_package_name(file_name);
                writeln!(
                    writer,
                    "{}",
                    backend.local_install_block(pkg_name, file_name)
                )
                .unwrap();
            }
        }
    }

    if let Some(src_val) = sources
        && let Some(feeds) = &src_val.feeds
        && !feeds.is_empty()
    {
        writeln!(writer, "{}", backend.feed_lines(feeds)).unwrap();
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

        assert!(w.contains("uci -q batch <<'UCI_EOF'"));
        assert!(w.contains("delete network.lan"));
        assert!(w.contains("set network.lan=interface"));
        assert!(w.contains("set network.lan.proto='static'"));
        assert!(w.contains("commit network"));
        assert!(w.contains("UCI_EOF"));
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

        assert!(w.contains("while uci -q delete dropbear.@dropbear[0]; do :; done"));
        assert!(w.contains("uci -q batch <<'UCI_EOF'"));
        assert!(w.contains("add dropbear dropbear"));
        assert!(w.contains("set dropbear.@dropbear[0].Port='22'"));
        assert!(w.contains("commit dropbear"));
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
        assert!(w.contains("set firewall.@redirect[1]"));
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
            feeds: Some(vec!["src/gz custom 'test' https://example.com".into()]),
            local_packages: None,
        };
        serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None).unwrap();
        assert!(w.contains("/etc/opkg/customfeeds.conf"));
        assert!(w.contains("printf '%s\\n' 'src/gz custom '\\''test'\\'' https://example.com'"));
    }

    #[test]
    fn test_serialize_opkg_feeds_apk() {
        let mut w = String::new();
        let sources = PackageSources {
            feeds: Some(vec!["https://example.com/packages".into()]),
            local_packages: None,
        };
        serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None).unwrap();
        assert!(w.contains("/etc/apk/repositories.d/customfeeds.list"));
        assert!(w.contains("printf '%s\\n' 'https://example.com/packages'"));
    }

    #[test]
    fn test_serialize_opkg_packages_opkg() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_package_management(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("opkg status"));
        assert!(w.contains("opkg update && opkg install luci tcpdump"));
    }

    #[test]
    fn test_serialize_opkg_packages_apk() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_package_management(&mut w, PkgBackend::Apk, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("apk info -e"));
        assert!(w.contains("apk add luci tcpdump"));
    }

    #[test]
    fn test_serialize_remove_before_install_opkg() {
        let mut w = String::new();
        let pkgs = vec!["-wpad-basic-mbedtls".into(), "wpad-mbedtls".into()];
        serialize_package_management(&mut w, PkgBackend::Opkg, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_REMOVE=false"));
        assert!(w.contains("opkg remove 'wpad-basic-mbedtls'"));
        assert!(w.contains("opkg update && opkg install wpad-mbedtls"));
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
        assert!(w.contains("apk add wpad-mbedtls"));
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
            feeds: None,
            local_packages: Some(vec!["./packages/test_1.0_all.ipk".into()]),
        };
        serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None).unwrap();
        assert!(w.contains("opkg status test"));
        assert!(w.contains("opkg install /tmp/test_1.0_all.ipk"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_apk() {
        let mut w = String::new();
        let sources = PackageSources {
            feeds: None,
            local_packages: Some(vec!["./packages/test_1.0_all.apk".into()]),
        };
        serialize_package_management(&mut w, PkgBackend::Apk, Some(&sources), None).unwrap();
        assert!(w.contains("if ! apk info -e test >/dev/null 2>&1; then"));
        assert!(w.contains("apk add --allow-untrusted /tmp/test_1.0_all.apk"));
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
}
