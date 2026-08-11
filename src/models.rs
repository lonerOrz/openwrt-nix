use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, Debug)]
pub struct Root {
    #[serde(default = "default_package_manager")]
    #[serde(rename = "packageManager")]
    pub package_manager: String,
    pub settings: IndexMap<String, IndexMap<String, Section>>,
    pub packages: Option<Vec<String>>,
    #[serde(rename = "packageSources")]
    pub package_sources: Option<PackageSources>,
    #[serde(rename = "sshKeys", default)]
    pub ssh_keys: Vec<String>,
    #[serde(default)]
    pub secrets: Option<SopsConfig>,
    /// Escape hatch: verbatim `uci` command lines emitted as-is, for UCI
    /// directives the typed `Section` model cannot express (rename, reorder,
    /// deleting a single option, exotic types). Each entry must be a complete
    /// `uci ...` command; this is the one place raw shell reaches the target.
    #[serde(default, rename = "rawUci")]
    pub raw_uci: Option<Vec<String>>,
    /// Arbitrary files to write on the target. Each entry specifies a
    /// destination path and content; an optional `executable` flag makes
    /// the file mode 0755 instead of 0644. A `checksum` (sha256 hex) guards
    /// the write so an unchanged file is skipped on redeploy.
    #[serde(default, rename = "files")]
    pub files: Option<Vec<File>>,
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

impl PkgBackend {
    pub fn from_name(s: &str) -> Self {
        match s {
            "apk" => PkgBackend::Apk,
            _ => PkgBackend::Opkg,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct PackageSources {
    pub feeds: Option<Vec<String>>,
    #[serde(rename = "localPackages")]
    pub local_packages: Option<Vec<String>>,
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

/// A file to write on the target device.
#[derive(Deserialize, Debug, Default)]
pub struct File {
    /// Absolute path on the target, e.g. `/etc/rc.local`.
    pub path: String,
    /// File content. Plain text by default; pass `{"base64": "..."}` for
    /// binary content (decoded on the target via `base64 -d`).
    #[serde(default, deserialize_with = "deserialize_file_content")]
    pub content: FileContent,
    /// Whether to make the file executable (default: false, mode 0644).
    #[serde(default)]
    pub executable: bool,
    /// Optional sha256 (hex) of the desired file. When set, the target
    /// skips the write if its current sha256 already matches — idempotent
    /// redeploys never touch an unchanged file.
    #[serde(default)]
    pub checksum: Option<String>,
}

/// File content: either inline text or base64-encoded binary.
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
