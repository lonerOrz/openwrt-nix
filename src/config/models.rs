use crate::utils::error::ConfigError;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct Root {
    #[serde(default = "default_package_manager", rename = "packageManager")]
    pub package_manager: String,
    pub settings: IndexMap<String, IndexMap<String, Section>>,
    pub packages: Option<Vec<String>>,
    #[serde(rename = "packageSources")]
    pub package_sources: Option<PackageSources>,
    #[serde(rename = "sshKeys", default)]
    pub ssh_keys: Vec<String>,
    #[serde(default)]
    pub secrets: Option<SopsConfig>,
    #[serde(default, rename = "rawUci")]
    pub raw_uci: Option<Vec<String>>,
    #[serde(default, rename = "files")]
    pub files: Option<Vec<File>>,
}

/// Debug view of `Root.settings`: lists config / section / option *names* but
/// never option *values*. Option values are the place where interpolated
/// secret plaintext ends up, so names-only keeps the shape visible for
/// debugging without leaking credentials.
struct SettingsDebug<'a>(&'a IndexMap<String, IndexMap<String, Section>>);

fn write_option_names<'a, I>(f: &mut std::fmt::Formatter<'_>, opts: I) -> std::fmt::Result
where
    I: Iterator<Item = &'a str>,
{
    let mut first = true;
    for opt in opts {
        if !first {
            f.write_str(", ")?;
        }
        first = false;
        f.write_str(opt)?;
    }
    Ok(())
}

impl std::fmt::Debug for SettingsDebug<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first_config = true;
        for (config_name, sections) in self.0 {
            if !first_config {
                f.write_str(", ")?;
            }
            first_config = false;
            f.write_str(config_name)?;
            f.write_str(" { ")?;
            let mut first_section = true;
            for (section_name, section) in sections {
                if !first_section {
                    f.write_str(", ")?;
                }
                first_section = false;
                f.write_str(section_name)?;
                f.write_str(" { ")?;
                match section {
                    Section::Named(data) => {
                        write_option_names(f, data.options.keys().map(|k| k.as_str()))?;
                    }
                    Section::List(items) => {
                        write_option_names(
                            f,
                            items
                                .iter()
                                .flat_map(|item| item.options.keys().map(|k| k.as_str())),
                        )?;
                    }
                }
                f.write_str(" }")?;
            }
            f.write_str(" }")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Root {
    /// Redacting Debug: `Root` holds secret-bearing data — SSH public keys,
    /// rawUci commands, package-source URLs (which may embed credentials) and
    /// arbitrary file contents. After secret interpolation the `settings`
    /// values also contain plaintext credentials. Deriving `Debug` would leak
    /// all of this through `dbg!(root)` / `{:?}` in logs, so we surface
    /// structure only and never the sensitive payloads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Root")
            .field("package_manager", &self.package_manager)
            .field("settings", &SettingsDebug(&self.settings))
            .field("packages", &self.packages)
            .field(
                "package_sources",
                &self.package_sources.as_ref().map(|s| {
                    format!(
                        "<{} opkg feeds, {} apk repos, {} local packages>",
                        s.opkg_feeds.as_ref().map_or(0, |v| v.len()),
                        s.apk_repositories.as_ref().map_or(0, |v| v.len()),
                        s.local_packages.as_ref().map_or(0, |v| v.len()),
                    )
                }),
            )
            .field(
                "ssh_keys",
                &format_args!("<{} redacted>", self.ssh_keys.len()),
            )
            .field("secrets", &self.secrets.is_some())
            .field(
                "raw_uci",
                &self
                    .raw_uci
                    .as_ref()
                    .map(|r| format!("<{} redacted>", r.len())),
            )
            .field(
                "files",
                &self
                    .files
                    .as_ref()
                    .map(|fl| format!("<{} redacted>", fl.len())),
            )
            .finish()
    }
}

#[derive(Deserialize, Debug, Default)]
pub struct SopsConfig {
    #[serde(default)]
    pub sops: Option<SopsFiles>,
}

#[derive(Deserialize, Debug, Default)]
pub struct SopsFiles {
    #[serde(default)]
    pub files: Vec<String>,
}

fn default_package_manager() -> String {
    "opkg".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgBackend {
    Opkg,
    Apk,
}

impl TryFrom<&str> for PkgBackend {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "apk" => Ok(Self::Apk),
            "opkg" => Ok(Self::Opkg),
            _ => Err(ConfigError::Validation(format!(
                "Unsupported package manager: {value}"
            ))),
        }
    }
}

#[derive(Deserialize, Debug, Default)]
pub struct PackageSources {
    /// Opkg-style feeds (`src/gz <name> <url>`). Written to
    /// `/etc/opkg/customfeeds.conf` only when the target uses opkg.
    #[serde(rename = "opkgFeeds", default)]
    pub opkg_feeds: Option<Vec<String>>,
    /// Apk repository URLs. Written to
    /// `/etc/apk/repositories.d/customfeeds.list` only when the target uses apk.
    #[serde(rename = "apkRepositories", default)]
    pub apk_repositories: Option<Vec<String>>,
    #[serde(rename = "localPackages", default)]
    pub local_packages: Option<Vec<String>>,
}

impl PackageSources {
    /// True only when every source list is absent or empty. Used by
    /// `read_package_manager_hint` so an empty `packageSources = { … = []; }`
    /// (as emitted by Nix modules) does not trigger an unnecessary runtime
    /// capability probe.
    pub fn is_empty(&self) -> bool {
        self.opkg_feeds.as_ref().is_none_or(|v| v.is_empty())
            && self.apk_repositories.as_ref().is_none_or(|v| v.is_empty())
            && self.local_packages.as_ref().is_none_or(|v| v.is_empty())
    }
}
#[derive(Deserialize, Debug, Clone)]
pub struct SectionData {
    #[serde(rename = "_type")]
    pub section_type: String,
    #[serde(flatten)]
    pub options: IndexMap<String, Value>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum Section {
    List(Vec<SectionData>),
    Named(SectionData),
}

#[derive(Deserialize, Debug, Default)]
pub struct File {
    pub path: String,
    #[serde(default, deserialize_with = "deserialize_file_content")]
    pub content: FileContent,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(String),
    Base64(String),
}

impl Default for FileContent {
    fn default() -> Self {
        FileContent::Text(String::new())
    }
}

impl FileContent {
    pub fn is_empty(&self) -> bool {
        match self {
            FileContent::Text(s) => s.is_empty(),
            FileContent::Base64(s) => s.is_empty(),
        }
    }
}

fn deserialize_file_content<'de, D>(d: D) -> Result<FileContent, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Binary { base64: String },
    }
    match Raw::deserialize(d)? {
        Raw::Text(s) => Ok(FileContent::Text(s)),
        Raw::Binary { base64 } => Ok(FileContent::Base64(base64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_backend_try_from_known() {
        assert!(matches!(PkgBackend::try_from("apk"), Ok(PkgBackend::Apk)));
        assert!(matches!(PkgBackend::try_from("opkg"), Ok(PkgBackend::Opkg)));
    }

    #[test]
    fn pkg_backend_try_from_rejects_unknown() {
        let err = PkgBackend::try_from("apt").unwrap_err();
        assert!(format!("{err}").contains("Unsupported package manager: apt"));
    }

    #[test]
    fn pkg_backend_try_from_rejects_empty() {
        let err = PkgBackend::try_from("").unwrap_err();
        assert!(format!("{err}").contains("Unsupported package manager"));
    }

    #[test]
    fn root_debug_redacts_secrets() {
        // Audit E.5/4.6: Root Debug must never leak secret-bearing payloads
        // (ssh keys, rawUci commands, file contents, or interpolated values).
        let mut settings = IndexMap::new();
        let mut sec = IndexMap::new();
        sec.insert(
            "lan".to_string(),
            Section::Named(SectionData {
                section_type: "interface".into(),
                options: IndexMap::from([(
                    "password".into(),
                    Value::String("SUPER_SECRET_PASSWORD".into()),
                )]),
            }),
        );
        settings.insert("network".into(), sec);
        let root = Root {
            package_manager: "opkg".into(),
            settings,
            packages: None,
            package_sources: None,
            ssh_keys: vec!["ssh-ed25519 AAAASECRETPUBLICKEY user@host".into()],
            secrets: None,
            raw_uci: Some(vec!["uci set network.lan.key='ANOTHER_SECRET'".into()]),
            files: Some(vec![File {
                path: "/etc/secret.conf".into(),
                content: FileContent::Text("LEAKED_FILE_BODY".into()),
                executable: false,
                checksum: None,
            }]),
        };

        let dbg = format!("{root:?}");
        // Structure is visible: config, section and option names.
        assert!(dbg.contains("package_manager: \"opkg\""), "got: {dbg}");
        assert!(dbg.contains("network { lan { password } }"), "got: {dbg}");
        // Payload carriers are redacted.
        assert!(dbg.contains("ssh_keys: <1 redacted>"), "got: {dbg}");
        assert!(
            dbg.contains("raw_uci: Some(\"<1 redacted>\")"),
            "got: {dbg}"
        );
        assert!(dbg.contains("files: Some(\"<1 redacted>\")"), "got: {dbg}");
        // Payloads are NOT.
        assert!(
            !dbg.contains("SUPER_SECRET_PASSWORD"),
            "settings value leaked: {dbg}"
        );
        assert!(
            !dbg.contains("AAAASECRETPUBLICKEY"),
            "ssh key leaked: {dbg}"
        );
        assert!(!dbg.contains("ANOTHER_SECRET"), "rawUci leaked: {dbg}");
        assert!(
            !dbg.contains("LEAKED_FILE_BODY"),
            "file content leaked: {dbg}"
        );
    }
}
