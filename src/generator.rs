use crate::error::ConfigError;
use crate::helpers::{escape_single_quotes, extract_package_name, iter_options};
use crate::models::{PackageSources, PkgBackend, Section};
use crate::uci_key::{anonymous_option_key, named_option_key};
use indexmap::IndexMap;
use serde_json::Value;
use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::path::Path;

/// Backend-specific package-manager command fragments.
///
/// The opkg/apk branches used to be copy-pasted `match backend` blocks at every
/// call site in `serialize_package_management`; centralizing them here means a
/// third backend adds one method per concern instead of editing four sites.
impl PkgBackend {
    /// Command that probes whether a package is already installed.
    pub(crate) fn installed_probe(&self) -> &'static str {
        match self {
            PkgBackend::Opkg => "opkg list-installed",
            PkgBackend::Apk => "apk info -e",
        }
    }

    /// The install line run when at least one package is missing.
    pub(crate) fn install_expr(&self, pkgs: &[String]) -> String {
        match self {
            PkgBackend::Opkg => {
                format!(
                    "if [ \"$NEED_INSTALL\" = true ]; then opkg update && opkg install {}; fi",
                    pkgs.join(" ")
                )
            }
            PkgBackend::Apk => {
                format!(
                    "if [ \"$NEED_INSTALL\" = true ]; then apk -U add {}; fi",
                    pkgs.join(" ")
                )
            }
        }
    }

    /// The `if ! installed; then install /tmp/<file>; fi` block for a local package.
    ///
    /// For opkg the package name is reliably derivable from the filename
    /// (`name_version_arch.ipk`), so we guard with `opkg list-installed`.
    /// For apk the filename stem is NOT a reliable package name (e.g.
    /// `libfoo-bar-1.0-r1.apk`), and `apk info <file>` returns nothing for a
    /// bare local package, so a name-based probe would be guesswork that can
    /// silently skip a package (false positive) or re-add it every run (false
    /// negative). `apk add` is idempotent for an already-installed identical
    /// package, so we install the file directly with no name probe — see
    /// audit candidate #10.
    pub(crate) fn local_install_block(&self, pkg_name: &str, file_name: &str) -> String {
        match self {
            PkgBackend::Opkg => format!(
                "\nif ! opkg list-installed \"{pkg_name}\" >/dev/null 2>&1; then\n    opkg install /tmp/{file_name}\nfi"
            ),
            PkgBackend::Apk => format!("\napk add --allow-untrusted /tmp/{file_name}"),
        }
    }

    /// Shell lines that (re)write the custom-feed repository file.
    pub(crate) fn feed_lines(&self, feeds: &[String]) -> String {
        match self {
            PkgBackend::Opkg => {
                let mut out = String::from("\nprintf '' > /etc/opkg/customfeeds.conf");
                for feed in feeds {
                    out.push_str(&format!(
                        "\nprintf '%s\\n' '{}' >> /etc/opkg/customfeeds.conf",
                        escape_single_quotes(feed)
                    ));
                }
                out
            }
            PkgBackend::Apk => {
                let mut out = String::from("\nmkdir -p /etc/apk/repositories.d");
                out.push_str("\nprintf '' > /etc/apk/repositories.d/customfeeds.list");
                for feed in feeds {
                    out.push_str(&format!(
                        "\nprintf '%s\\n' '{}' >> /etc/apk/repositories.d/customfeeds.list",
                        escape_single_quotes(feed)
                    ));
                }
                out
            }
        }
    }
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
            let bool_str = if *b { "1" } else { "0" };
            writeln!(writer, "set {}='{}'", key, escape_single_quotes(bool_str)).unwrap();
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
                writeln!(writer, "add_list {}='{}'", key, escape_single_quotes(&s)).unwrap();
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
                                    ConfigError::Validation(format!(
                                        "{}.@{}[{}] has no type!",
                                        config_name, section_name, idx
                                    ))
                                })?;

                        writeln!(uci_cmds, "add {} {}", config_name, ty).unwrap();

                        for (option_name, option) in iter_options(list_obj) {
                            let key = anonymous_option_key(config_name, ty, idx, option_name);
                            serialize_option_val(&mut uci_cmds, &key, option)?;
                        }
                    }
                }
                Section::Named(obj) => {
                    let ty = obj.get("_type").and_then(|v| v.as_str()).ok_or_else(|| {
                        ConfigError::Validation(format!(
                            "{}.{} has no type",
                            config_name, section_name
                        ))
                    })?;

                    writeln!(uci_cmds, "delete {}.{}", config_name, section_name).unwrap();
                    writeln!(uci_cmds, "set {}.{}={}", config_name, section_name, ty).unwrap();

                    for (option_name, option) in iter_options(obj) {
                        let key = named_option_key(config_name, section_name, option_name);
                        serialize_option_val(&mut uci_cmds, &key, option)?;
                    }
                }
            }
        }

        write!(writer, "{}", shell_cmds).unwrap();

        if !uci_cmds.is_empty() {
            // Ensure the config file exists before running batch — UCI won't
            // accept set/add commands for a config whose file is missing on
            // disk (the file is created on first commit, but the batch
            // commands themselves silently fail if the file doesn't exist).
            writeln!(writer, "touch /etc/config/{}", config_name).unwrap();
            writeln!(writer, "uci -q batch <<'UCI_EOF'").unwrap();
            write!(writer, "{}", uci_cmds).unwrap();
            writeln!(writer, "commit {}", config_name).unwrap();
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
    // Install packages BEFORE injecting custom feeds. Package installs only
    // need the default repos; a dead/example custom feed must not poison the
    // `apk -U` cache refresh that precedes the install (apk updates every
    // configured repository, so writing the feed first makes repo installs
    // flaky when the feed is unreachable).
    if let Some(pkgs) = packages
        && !pkgs.is_empty()
    {
        writeln!(writer, "\nNEED_INSTALL=false").unwrap();
        writeln!(writer, "for pkg in {}; do", pkgs.join(" ")).unwrap();
        writeln!(
            writer,
            "    if ! {} \"$pkg\" >/dev/null 2>&1; then NEED_INSTALL=true; break; fi",
            backend.installed_probe()
        )
        .unwrap();
        writeln!(writer, "done").unwrap();

        writeln!(writer, "{}", backend.install_expr(pkgs)).unwrap();
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
    use serde_json::Map;

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
    }

    #[test]
    fn serialize_list_section() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
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
    }

    #[test]
    fn serialize_named_section_missing_type_errors() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut obj = Map::new();
        obj.insert("proto".into(), Value::String("static".into()));
        sections.insert("lan".into(), Section::Named(obj));
        configs.insert("network".into(), sections);

        let mut w = String::new();
        let err = serialize_uci(&mut w, &configs).unwrap_err();
        assert!(format!("{err}").contains("has no type"));
    }

    #[test]
    fn serialize_list_section_missing_type_errors() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut item = Map::new();
        item.insert("Port".into(), Value::String("22".into()));
        sections.insert("dropbear".into(), Section::List(vec![item]));
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        let err = serialize_uci(&mut w, &configs).unwrap_err();
        assert!(format!("{err}").contains("has no type"));
    }

    #[test]
    fn serialize_multiple_list_items() {
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
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
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("interface".into()));
        item.insert("proto".into(), Value::String("static".into()));
        sections.insert("interfaces".into(), Section::List(vec![item]));
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
        assert!(w.contains("opkg list-installed"));
        assert!(w.contains("opkg update && opkg install luci tcpdump"));
    }

    #[test]
    fn test_serialize_opkg_packages_apk() {
        let mut w = String::new();
        let pkgs = vec!["luci".into(), "tcpdump".into()];
        serialize_package_management(&mut w, PkgBackend::Apk, None, Some(&pkgs)).unwrap();
        assert!(w.contains("NEED_INSTALL=false"));
        assert!(w.contains("apk info -e"));
        assert!(w.contains("apk -U add luci tcpdump"));
    }

    #[test]
    fn test_serialize_opkg_local_packages_opkg() {
        let mut w = String::new();
        let sources = PackageSources {
            feeds: None,
            local_packages: Some(vec!["./packages/test_1.0_all.ipk".into()]),
        };
        serialize_package_management(&mut w, PkgBackend::Opkg, Some(&sources), None).unwrap();
        assert!(w.contains("opkg list-installed \"test\""));
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
        assert!(!w.contains("apk info -e"));
        assert!(w.contains("apk add --allow-untrusted /tmp/test_1.0_all.apk"));
    }

    #[test]
    fn serialize_list_rebuilds_every_item() {
        // Every list section emits a `delete @type[0]` clear loop, so removing an
        // item from the Nix config makes it disappear on the target (full rebuild).
        let mut configs = IndexMap::new();
        let mut sections = IndexMap::new();
        let mut item = Map::new();
        item.insert("_type".into(), Value::String("dropbear".into()));
        item.insert("Port".into(), Value::String("22".into()));
        sections.insert("dropbear".into(), Section::List(vec![item]));
        configs.insert("dropbear".into(), sections);

        let mut w = String::new();
        serialize_uci(&mut w, &configs).unwrap();

        assert!(w.contains("while uci -q delete dropbear.@dropbear[0]; do :; done"));
        let add_count = w.matches("add dropbear dropbear").count();
        assert_eq!(add_count, 1);
    }
}
