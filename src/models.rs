use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Deserialize, Debug)]
pub(crate) struct Root {
    #[serde(default = "default_package_manager")]
    #[serde(rename = "packageManager")]
    pub(crate) package_manager: String,
    pub(crate) settings: IndexMap<String, IndexMap<String, Section>>,
    pub(crate) packages: Option<Vec<String>>,
    #[serde(rename = "packageSources")]
    pub(crate) package_sources: Option<PackageSources>,
    #[serde(rename = "sshKeys", default)]
    pub(crate) ssh_keys: Vec<String>,
    #[serde(default)]
    pub(crate) secrets: Option<SopsConfig>,
    /// Escape hatch: verbatim `uci` command lines emitted as-is, for UCI
    /// directives the typed `Section` model cannot express (rename, reorder,
    /// deleting a single option, exotic types). Each entry must be a complete
    /// `uci ...` command; this is the one place raw shell reaches the target.
    #[serde(default, rename = "rawUci")]
    pub(crate) raw_uci: Option<Vec<String>>,
    /// Arbitrary files to write on the target. Each entry specifies a
    /// destination path and content; an optional `executable` flag makes
    /// the file mode 0755 instead of 0644. A `checksum` (sha256 hex) guards
    /// the write so an unchanged file is skipped on redeploy.
    #[serde(default, rename = "files")]
    pub(crate) files: Option<Vec<File>>,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) struct SopsConfig {
    #[serde(default)]
    pub(crate) sops: Option<SopsFiles>,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) struct SopsFiles {
    #[serde(default)]
    pub(crate) files: Vec<String>,
}

fn default_package_manager() -> String {
    "opkg".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PkgBackend {
    Opkg,
    Apk,
}

impl PkgBackend {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "apk" => PkgBackend::Apk,
            _ => PkgBackend::Opkg,
        }
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct PackageSources {
    pub(crate) feeds: Option<Vec<String>>,
    #[serde(rename = "localPackages")]
    pub(crate) local_packages: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub(crate) enum Section {
    List(Vec<Map<String, Value>>),
    Named(Map<String, Value>),
}

/// A file to write on the target device.
#[derive(Deserialize, Debug, Default)]
pub(crate) struct File {
    /// Absolute path on the target, e.g. `/etc/rc.local`.
    pub(crate) path: String,
    /// File content. Plain text by default; pass `{"base64": "..."}` for
    /// binary content (decoded on the target via `base64 -d`).
    #[serde(default, deserialize_with = "deserialize_file_content")]
    pub(crate) content: FileContent,
    /// Whether to make the file executable (default: false, mode 0644).
    #[serde(default)]
    pub(crate) executable: bool,
    /// Optional sha256 (hex) of the desired file. When set, the target
    /// skips the write if its current sha256 already matches — idempotent
    /// redeploys never touch an unchanged file.
    #[serde(default)]
    pub(crate) checksum: Option<String>,
}

/// File content: either inline text or base64-encoded binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileContent {
    Text(String),
    Base64(String),
}

impl Default for FileContent {
    fn default() -> Self {
        FileContent::Text(String::new())
    }
}

impl FileContent {
    pub(crate) fn is_empty(&self) -> bool {
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
