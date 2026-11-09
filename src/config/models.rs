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

/// Names only: option values hold interpolated secret plaintext.
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
    /// Redacting Debug: the struct holds secret-bearing data (keys, rawUci, URLs, values).
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

/// Redacting Debug: feed URLs can embed HTTP basic credentials.
#[derive(Deserialize)]
pub struct PackageSources {
    /// Opkg feeds, written to `/etc/opkg/customfeeds.conf` on opkg targets.
    #[serde(rename = "opkgFeeds", default)]
    pub opkg_feeds: Option<Vec<String>>,
    /// Apk repositories, written to `/etc/apk/repositories.d/customfeeds.list` on apk targets.
    #[serde(rename = "apkRepositories", default)]
    pub apk_repositories: Option<Vec<String>>,
    #[serde(rename = "localPackages", default)]
    pub local_packages: Option<Vec<String>>,
}

impl std::fmt::Debug for PackageSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageSources")
            .field(
                "opkg_feeds",
                &self.opkg_feeds.as_ref().map_or(0, |v| v.len()),
            )
            .field(
                "apk_repositories",
                &self.apk_repositories.as_ref().map_or(0, |v| v.len()),
            )
            .field(
                "local_packages",
                &self.local_packages.as_ref().map_or(0, |v| v.len()),
            )
            .finish()
    }
}

impl PackageSources {
    /// True when no source list holds an entry (Nix modules emit empty lists).
    pub fn is_empty(&self) -> bool {
        self.opkg_feeds.as_ref().is_none_or(|v| v.is_empty())
            && self.apk_repositories.as_ref().is_none_or(|v| v.is_empty())
            && self.local_packages.as_ref().is_none_or(|v| v.is_empty())
    }
}
/// Holds option values, which carry plaintext credentials after interpolation.
#[derive(Deserialize)]
pub struct SectionData {
    #[serde(rename = "_type")]
    pub section_type: String,
    #[serde(flatten)]
    pub options: IndexMap<String, Value>,
}

impl std::fmt::Debug for SectionData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionData")
            .field("_type", &self.section_type)
            .field("options", &self.options.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Redacting Debug: the inner options hold interpolated secret plaintext.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Section {
    List(Vec<SectionData>),
    Named(SectionData),
}

impl std::fmt::Debug for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Section::List(items) => {
                let types: Vec<&str> = items.iter().map(|i| i.section_type.as_str()).collect();
                f.debug_tuple("List").field(&types).finish()
            }
            Section::Named(data) => f.debug_tuple("Named").field(data).finish(),
        }
    }
}

/// Redacting Debug: `content` is the full file body and may be a secret.
#[derive(Deserialize)]
pub struct File {
    pub path: String,
    #[serde(default, deserialize_with = "deserialize_file_content")]
    pub content: FileContent,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub checksum: Option<String>,
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("File")
            .field("path", &self.path)
            .field("content", &self.content)
            .field("executable", &self.executable)
            .field("checksum", &self.checksum)
            .finish()
    }
}

/// Redacting Debug: the payload can be secret plaintext or a key file.
#[derive(Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(String),
    Base64(String),
}
impl std::fmt::Debug for FileContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileContent::Text(s) => f
                .debug_tuple("Text")
                .field(&format_args!("<{} bytes redacted>", s.len()))
                .finish(),
            FileContent::Base64(s) => f
                .debug_tuple("Base64")
                .field(&format_args!("<{} bytes redacted>", s.len()))
                .finish(),
        }
    }
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
        // Root Debug must never leak secret-bearing payloads (ssh keys, rawUci
        // commands, file contents, or interpolated values).
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

    #[test]
    fn inner_type_debug_redacts_secret_payloads() {
        // The secret-bearing inner types must redact their own Debug, because
        // `Root`/`CompiledConfig` redaction only protects the top level — a stray
        // `dbg!(section)` / `dbg!(file)` must not dump plaintext either.
        let section = Section::Named(SectionData {
            section_type: "interface".into(),
            options: IndexMap::from([
                ("password".into(), Value::String("PLAINTEXT_PW".into())),
                ("_type".into(), Value::String("interface".into())),
            ]),
        });
        let section_dbg = format!("{section:?}");
        assert!(
            section_dbg.contains("interface"),
            "type must stay visible: {section_dbg}"
        );
        assert!(
            !section_dbg.contains("PLAINTEXT_PW"),
            "section option value leaked: {section_dbg}"
        );

        let file = File {
            path: "/etc/secret.conf".into(),
            content: FileContent::Text("LEAKED_BODY".into()),
            executable: true,
            checksum: Some("abc123".into()),
        };
        let file_dbg = format!("{file:?}");
        assert!(
            file_dbg.contains("/etc/secret.conf"),
            "path must stay visible: {file_dbg}"
        );
        assert!(file_dbg.contains("<11 bytes redacted>"), "got: {file_dbg}");
        assert!(
            !file_dbg.contains("LEAKED_BODY"),
            "file content leaked: {file_dbg}"
        );

        // Base64 variant redacts the encoded bytes too.
        let b64_payload = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=";
        let b64_dbg = format!("{:?}", FileContent::Base64(b64_payload.into()));
        assert!(
            b64_dbg.contains(&format!("<{} bytes redacted>", b64_payload.len())),
            "got: {b64_dbg}"
        );
        assert!(
            !b64_dbg.contains("QUJD"),
            "base64 payload leaked: {b64_dbg}"
        );

        // PackageSources redacts feed/repo URLs (which may embed credentials).
        let sources = PackageSources {
            opkg_feeds: Some(vec!["src/gz custom https://user:pass@host/feed".into()]),
            apk_repositories: Some(vec!["https://user:pw@repo.example/".into()]),
            local_packages: None,
        };
        let sources_dbg = format!("{sources:?}");
        assert!(sources_dbg.contains("opkg_feeds: 1"), "got: {sources_dbg}");
        assert!(
            sources_dbg.contains("apk_repositories: 1"),
            "got: {sources_dbg}"
        );
        assert!(
            !sources_dbg.contains("user:pass"),
            "feed URL credentials leaked: {sources_dbg}"
        );
    }
}
