use crate::compile::generator::{serialize_package_management, serialize_uci};
use crate::compile::secrets::SecretMap;
use crate::compile::secrets::{decrypt_sops_mem, load_secrets_dir, merge_secrets, resolve_secrets};
use crate::config::models::PkgBackend;
use crate::config::validation::validate_root;
use crate::utils::error::ConfigError;
use std::collections::HashMap;
use std::path::Path;

pub struct CompiledConfig {
    /// UCI operations (typed sections, rawUci) only — no package-manager commands.
    pub uci_batch: String,
    /// Package operations, rendered after the UCI "apply done" sentinel.
    pub package_commands: String,
    pub resolved_root: crate::config::models::Root,
    /// Decrypted secrets; read one value via `secrets.get(name)`.
    pub secrets: SecretMap,
}

impl std::fmt::Debug for CompiledConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledConfig")
            .field("uci_batch_len", &self.uci_batch.len())
            .field("package_commands_len", &self.package_commands.len())
            .finish()
    }
}

/// Declared backend plus whether package commands are emitted at all.
pub struct PackageHint {
    pub declared: String,
    pub needs_pm: bool,
}

pub fn read_package_manager_hint(json_path: &Path) -> Result<PackageHint, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: crate::config::models::Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    // `is_some()` alone is wrong: Nix modules emit `packages = []` and
    // `packageSources = { opkgFeeds = []; apkRepositories = []; localPackages = []; }`,
    // which `is_some()` treats as "needs package manager" and triggers an
    // unnecessary runtime capability probe. Require at least one declared entry.
    let needs_pm = root.packages.as_ref().is_some_and(|p| !p.is_empty())
        || root.package_sources.as_ref().is_some_and(|s| !s.is_empty());
    Ok(PackageHint {
        declared: root.package_manager,
        needs_pm,
    })
}

pub fn compile_config(
    json_path: &Path,
    secrets_dir: Option<&Path>,
    skip_sops: bool,
    backend: Option<PkgBackend>,
) -> Result<CompiledConfig, ConfigError> {
    let file = std::fs::File::open(json_path)?;
    let root: crate::config::models::Root = serde_json::from_reader(std::io::BufReader::new(file))?;
    validate_root(&root)?;

    let mut secrets = if skip_sops {
        HashMap::new()
    } else {
        decrypt_sops_mem(&root)?
    };

    if let Some(dir) = secrets_dir {
        let dir_secrets =
            load_secrets_dir(dir.to_str().ok_or_else(|| {
                ConfigError::Validation("Invalid secrets directory path".into())
            })?)?;
        merge_secrets(&mut secrets, dir_secrets)?;
    }

    let resolved_root = resolve_secrets(root, &secrets)?;

    // Re-validate the escape hatch AFTER secret interpolation: a secret value
    // substituted into a rawUci line can introduce shell metacharacters that
    // were absent from the original config text. rawUci lines are emitted
    // verbatim into the deploy script, so this must be checked on the final
    // text or an injected secret becomes a shell injection.
    crate::config::validation::validate_raw_uci(resolved_root.raw_uci.as_deref())?;

    let mut uci_batch = String::with_capacity(4096);
    serialize_uci(&mut uci_batch, &resolved_root.settings)?;

    if let Some(raw) = &resolved_root.raw_uci
        && !raw.is_empty()
    {
        let mut needed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in raw {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("uci set ") {
                if let Some(cfg) = rest.split('.').next().filter(|c| !c.is_empty()) {
                    if !crate::config::validation::is_valid_config_name(cfg) {
                        return Err(ConfigError::Validation(format!(
                            "rawUci references an invalid UCI config name '{cfg}'"
                        )));
                    }
                    needed.insert(cfg);
                }
            } else if let Some(rest) = trimmed.strip_prefix("uci add ") {
                // `uci add <config> <type>` — config is the first whitespace token
                if let Some(cfg) = rest.split_whitespace().next().filter(|c| !c.is_empty()) {
                    if !crate::config::validation::is_valid_config_name(cfg) {
                        return Err(ConfigError::Validation(format!(
                            "rawUci references an invalid UCI config name '{cfg}'"
                        )));
                    }
                    needed.insert(cfg);
                }
            }
        }

        if !needed.is_empty() {
            uci_batch.push_str("\n# Ensure config files exist for raw UCI lines below\n");
            for cfg in &needed {
                uci_batch.push_str(&format!("touch /etc/config/{}\n", cfg));
            }
        }

        uci_batch.push_str("\n# Raw UCI escape hatch (verbatim)\n");
        for line in raw {
            uci_batch.push_str(line.trim_end());
            uci_batch.push('\n');
        }
    }

    let backend = match backend {
        Some(b) => b,
        None => PkgBackend::try_from(resolved_root.package_manager.as_str())?,
    };
    let mut package_commands = String::with_capacity(1024);
    serialize_package_management(
        &mut package_commands,
        backend,
        resolved_root.package_sources.as_ref(),
        resolved_root.packages.as_deref(),
    )?;

    Ok(CompiledConfig {
        uci_batch,
        package_commands,
        resolved_root,
        secrets: SecretMap::new(secrets),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_json(s: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f
    }

    #[test]
    fn raw_uci_emitted_verbatim_after_typed_batch() {
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "system": { "system": [ { "_type": "system", "hostname": "rauter" } ] }
                },
                "rawUci": [ "uci rename system.@system[0]=sys0" ]
            }"#,
        );
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.contains("uci rename system.@system[0]=sys0"));
        let raw_pos = out.uci_batch.find("uci rename").unwrap();
        let typed_pos = out.uci_batch.find("add system system").unwrap();
        assert!(
            raw_pos > typed_pos,
            "rawUci should follow typed uci commands"
        );
    }

    #[test]
    fn raw_uci_add_touches_single_config() {
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": [ "uci add dropbear dropbear", "uci set network.lan.proto='static'" ]
            }"#,
        );
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.contains("touch /etc/config/dropbear\n"));
        assert!(out.uci_batch.contains("touch /etc/config/network\n"));
        assert!(
            !out.uci_batch
                .contains("touch /etc/config/dropbear dropbear")
        );
    }

    /// A secret substituted into a rawUci line must not smuggle in shell metacharacters.
    #[test]
    fn raw_uci_secret_cannot_inject_shell_metacharacters() {
        let secrets_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            secrets_dir.path().join("evil.json"),
            r#"{"proto": "static'; touch /tmp/pwned; echo '"}"#,
        )
        .unwrap();
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": [ "uci set network.lan.proto='@proto@'" ]
            }"#,
        );
        let err = compile_config(json.path(), Some(secrets_dir.path()), true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("shell metacharacter"),
            "expected the metacharacter guard to fire, got: {msg}"
        );
    }

    /// A benign secret must still interpolate into rawUci normally.
    #[test]
    fn raw_uci_secret_without_metacharacters_still_interpolates() {
        let secrets_dir = tempfile::tempdir().unwrap();
        std::fs::write(secrets_dir.path().join("ok.json"), r#"{"proto": "static"}"#).unwrap();
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": [ "uci set network.lan.proto='@proto@'" ]
            }"#,
        );
        let out = compile_config(json.path(), Some(secrets_dir.path()), true, None).unwrap();
        assert!(
            out.uci_batch.contains("uci set network.lan.proto='static'"),
            "got: {}",
            out.uci_batch
        );
    }

    /// A NUL byte is silently deleted by the transport, landing a wrong value.
    #[test]
    fn option_value_with_nul_byte_is_rejected() {
        let json = write_json(
            "{\n\
                \"packageManager\": \"opkg\",\n\
                \"settings\": { \"network\": { \"lan\": { \"_type\": \"interface\", \"proto\": \"a\\u0000b\" } } }\n\
            }",
        );
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("cannot be stored reliably"),
            "expected the charset guard to fire, got: {msg}"
        );
    }

    /// A newline splits one command into two, corrupting the value.
    #[test]
    fn option_value_with_newline_is_rejected() {
        let json = write_json(
            "{\n\
                \"packageManager\": \"opkg\",\n\
                \"settings\": { \"network\": { \"lan\": { \"_type\": \"interface\", \"proto\": \"a\\nb\" } } }\n\
            }",
        );
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("cannot be stored reliably"),
            "expected the charset guard to fire, got: {msg}"
        );
    }

    /// Values UCI genuinely accepts must keep working.
    #[test]
    fn option_value_with_uci_valid_specials_is_accepted() {
        let json = write_json(
            "{\n\
                \"packageManager\": \"opkg\",\n\
                \"settings\": { \"wireless\": { \"radio0\": { \"_type\": \"wifi-iface\",\n\
                    \"ssid\": \"My Net\\u00e9\",\n\
                    \"note\": \"a\\\\b $c 'd' \\\"e\\\"; f # g = h\",\n\
                    \"key\": \"pass word\\twith tab\" } } }\n\
            }",
        );
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(
            out.uci_batch
                .contains("set wireless.radio0.ssid='My Net\u{e9}'"),
            "got: {}",
            out.uci_batch
        );
    }

    /// A NUL byte in a list element must be rejected too.
    #[test]
    fn list_value_with_nul_byte_is_rejected() {
        let json = write_json(
            "{\n\
                \"packageManager\": \"opkg\",\n\
                \"settings\": { \"system\": { \"ntp\": { \"_type\": \"ntp\",\n\
                    \"server\": [\"ok\", \"bad\\u0000value\"] } } }\n\
            }",
        );
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("cannot be stored reliably"),
            "expected the charset guard to fire, got: {msg}"
        );
    }

    #[test]
    fn raw_uci_invalid_config_name_errors() {
        // `uci set a b.c=1` yields config token "a b" which is not a valid UCI
        // config name; it must be rejected rather than producing `touch /etc/config/a b`.
        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {},
                "rawUci": [ "uci set a b.c=1" ]
            }"#,
        );
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        assert!(
            format!("{err}").contains("invalid UCI config name"),
            "expected config-name error, got: {err}"
        );
        assert!(
            !format!("{err}").contains(" b.c=1"),
            "error must not echo the secret-bearing raw line contents"
        );
    }
    #[test]
    fn raw_uci_absent_when_not_declared() {
        let json = write_json(r#"{ "packageManager": "opkg", "settings": {} }"#);
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(!out.uci_batch.contains("Raw UCI escape hatch"));
    }

    #[test]
    fn unknown_package_manager_is_rejected() {
        let json = write_json(r#"{ "packageManager": "apt", "settings": {} }"#);
        let err = compile_config(json.path(), None, true, None).unwrap_err();
        assert!(
            format!("{err}").contains("Unsupported package manager: apt"),
            "expected rejection, got: {err}"
        );
    }

    #[test]
    fn default_package_manager_is_opkg() {
        // packageManager omitted -> opkg default, compiles without error.
        let json = write_json(r#"{ "settings": {} }"#);
        let out = compile_config(json.path(), None, true, None).unwrap();
        assert!(out.uci_batch.is_empty());
    }

    #[test]
    fn secret_interpolation_whitelist_e2e() {
        // rawUci / files.text / sshKeys interpolate alongside settings and feeds.
        let dir = tempfile::tempdir().unwrap();
        let secret_path = dir.path().join("secrets.json");
        std::fs::write(
            &secret_path,
            r#"{ "wan_user": "alice", "api_token": "s3cr3t", "pubkey": "AAAAC3NzaC1lZDI1NTE5AAAAIExample" }"#,
        )
        .unwrap();

        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "network": { "wan": { "_type": "interface", "username": "@wan_user@" } }
                },
                "rawUci": [ "uci set network.wan.password='@wan_user@'" ],
                "files": [
                    { "path": "/etc/app/conf", "content": "token=@api_token@" }
                ],
                "sshKeys": [ "ssh-ed25519 @pubkey@ user@host" ]
            }"#,
        );

        let out = compile_config(json.path(), Some(dir.path()), true, None).unwrap();

        // rawUci interpolated
        assert!(
            out.uci_batch
                .contains("uci set network.wan.password='alice'"),
            "rawUci should interpolate: {}",
            out.uci_batch
        );
        // settings interpolated
        assert!(out.uci_batch.contains("set network.wan.username='alice'"));
        // sshKeys interpolated in the resolved root
        assert_eq!(
            out.resolved_root.ssh_keys[0],
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample user@host"
        );
        // files.text interpolated in the resolved root
        match &out.resolved_root.files.unwrap()[0].content {
            crate::config::models::FileContent::Text(s) => assert_eq!(s, "token=s3cr3t"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn secrets_dir_cross_file_conflict_errors() {
        // Two files in the secrets dir claim the same key with different values.
        // The merge into the (skipped) SOPS map must reject, not silently keep
        // one value. No sops binary needed (skip_sops = true).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("b.json"), r#"{"wifi_pass": "beta"}"#).unwrap();

        let json = write_json(r#"{ "packageManager": "opkg", "settings": {} }"#);
        let err = compile_config(json.path(), Some(dir.path()), true, None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("conflicts"), "expected conflict, got: {msg}");
        assert!(msg.contains("wifi_pass"), "expected key name, got: {msg}");
        // Plaintext must never leak into the error message.
        assert!(!msg.contains("alpha"), "plaintext leaked: {msg}");
        assert!(!msg.contains("beta"), "plaintext leaked: {msg}");
    }

    #[test]
    fn secrets_dir_same_value_idempotent_merge_ok() {
        // Same key with identical value across two files -> no conflict.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("b.json"), r#"{"wifi_pass": "alpha"}"#).unwrap();

        let json = write_json(
            r#"{
                "packageManager": "opkg",
                "settings": {
                    "wireless": { "radio0": { "_type": "wifi-iface", "key": "@wifi_pass@" } }
                }
            }"#,
        );
        let out = compile_config(json.path(), Some(dir.path()), true, None).unwrap();
        assert!(out.uci_batch.contains("set wireless.radio0.key='alpha'"));
    }
}

/// Compile-output regression baselines.
///
/// Pin the emitted shape for the samples under `tests/samples/`. Snapshots only
/// catch accidental changes; the in-container round-trip tests prove correctness.
#[cfg(test)]
mod snapshots {
    use super::*;
    use std::path::PathBuf;

    fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("samples")
            .join(name)
    }

    fn compile_sample(name: &str, backend: PkgBackend) -> CompiledConfig {
        compile_config(&sample(name), None, true, Some(backend)).unwrap()
    }

    #[test]
    fn snapshot_uci_basic() {
        let out = compile_sample("uci-basic.json", PkgBackend::Opkg);
        assert!(
            out.uci_batch
                .contains("set system.system.hostname='test-router'")
        );
        assert!(out.uci_batch.contains("set network.lan.ifname='eth0'"));
        assert!(out.uci_batch.contains("set network.lan.proto='static'"));
        assert!(
            out.uci_batch
                .contains("set network.lan.ipaddr='192.168.1.1'")
        );
        // Discrete `uci <verb>` lines, not a `uci batch` heredoc.
        assert!(!out.uci_batch.contains("uci batch"));
        // No packages declared -> no package commands.
        assert!(out.package_commands.is_empty());
    }

    /// Shell metacharacters are valid UCI content, but must stay inside the
    /// single-quoted value so the deploy script cannot execute them.
    #[test]
    fn snapshot_uci_metacharacters_stay_quoted() {
        let out = compile_sample("uci-metachars.json", PkgBackend::Opkg);
        assert!(
            out.uci_batch
                .contains("set system.system.hostname='OpenWrt-$(evil)'")
        );
        assert!(
            out.uci_batch
                .contains("set wireless.default_radio0.key='pa$$w0rd$(whoami)$(curl evil.com)'")
        );
        // Every option value must be single-quoted. The section-type assignment
        // (`uci set system.system=system`) is a fixed identifier, not a value,
        // so it is the only `set` line without quotes.
        let mut checked = 0;
        for line in out.uci_batch.lines() {
            let Some(rest) = line.strip_prefix("uci set ") else {
                continue;
            };
            let Some((path, value)) = rest.split_once('=') else {
                continue;
            };
            if path == "system.system" || path == "wireless.default_radio0" {
                continue; // section-type assignments
            }
            assert!(
                value.starts_with('\'') && value.ends_with('\''),
                "value is not single-quoted: {line}"
            );
            checked += 1;
        }
        assert!(
            checked >= 4,
            "expected several quoted values, got {checked}"
        );
    }

    #[test]
    fn snapshot_uci_heterogeneous_per_type_indexing() {
        // Anonymous-section indexes count per type, not across the array.
        let out = compile_sample("uci-heterogeneous.json", PkgBackend::Opkg);
        assert!(out.uci_batch.contains("add firewall rule"));
        assert!(out.uci_batch.contains("add firewall redirect"));
        assert!(
            out.uci_batch
                .contains("set firewall.@rule[0].target='ACCEPT'")
        );
        assert!(
            out.uci_batch
                .contains("set firewall.@redirect[0].dest='lan'")
        );
        assert!(
            out.uci_batch
                .contains("set firewall.@rule[1].target='DROP'")
        );
        assert!(!out.uci_batch.contains("@rule[2]"));
        assert!(!out.uci_batch.contains("@redirect[1]"));
    }

    #[test]
    fn snapshot_raw_uci_verbatim_and_config_touch() {
        let out = compile_sample("raw-uci.json", PkgBackend::Opkg);
        // Typed section emitted first.
        let typed = out.uci_batch.find("set system").unwrap();
        let raw = out.uci_batch.find("# Raw UCI escape hatch").unwrap();
        assert!(typed < raw, "typed UCI must precede rawUci");
        // rawUci references network.lan -> the generator touches that config.
        assert!(out.uci_batch.contains("touch /etc/config/network"));
        assert!(out.uci_batch.contains("uci rename system.@system[0]=sys0"));
        assert!(out.uci_batch.contains("uci set network.lan.proto='static'"));
    }

    #[test]
    fn snapshot_packages_opkg() {
        let out = compile_config(
            &sample("packages-opkg.json"),
            None,
            true,
            Some(PkgBackend::Opkg),
        )
        .unwrap();
        assert!(
            out.package_commands
                .contains("printf '' > /etc/opkg/customfeeds.conf")
        );
        assert!(
            out.package_commands
                .contains("src/gz custom https://example.com/packages")
        );
        // Package names are shell-quoted in the generated commands.
        assert!(out.package_commands.contains("opkg install 'luci'"));
        assert!(out.package_commands.contains("opkg remove 'luci-ssl'"));
    }

    #[test]
    fn snapshot_packages_apk() {
        let out = compile_config(
            &sample("packages-apk.json"),
            None,
            true,
            Some(PkgBackend::Apk),
        )
        .unwrap();
        assert!(
            out.package_commands
                .contains("mkdir -p /etc/apk/repositories.d")
        );
        assert!(
            out.package_commands
                .contains("https://example.com/packages")
        );
        assert!(out.package_commands.contains("apk add 'luci'"));
        assert!(out.package_commands.contains("apk del 'luci-ssl'"));
    }

    #[test]
    fn snapshot_files_text_and_binary() {
        let out = compile_sample("files.json", PkgBackend::Opkg);
        assert!(out.uci_batch.is_empty(), "files produce no UCI commands");
        let resolved = &out.resolved_root.files.unwrap();
        // The resolved model carries the original text.
        match &resolved[0].content {
            crate::config::models::FileContent::Text(t) => assert_eq!(t, "hello world"),
            _ => panic!("expected text content"),
        }
        match &resolved[1].content {
            crate::config::models::FileContent::Base64(b) => assert_eq!(b, "aGVsbG8="),
            _ => panic!("expected base64 content"),
        }
        assert_eq!(
            resolved[1].checksum.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    #[test]
    fn snapshot_secrets_interpolation() {
        let dir = sample("secrets_dir");
        let out = compile_config(
            &sample("secrets.json"),
            Some(&dir),
            true,
            Some(PkgBackend::Opkg),
        )
        .unwrap();
        // The @wifi_pass@ placeholder must be resolved from the secrets dir.
        assert!(
            out.uci_batch
                .contains("set wireless.default_radio0.key='s3cr3t-wpa-psk'")
        );
        assert!(!out.uci_batch.contains("@wifi_pass@"), "placeholder leaked");
    }
}
