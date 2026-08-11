use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, Debug)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageAction {
    Install(String),
    Remove(String),
}

impl PackageAction {
    pub fn parse(spec: &str) -> Self {
        match spec.strip_prefix('-') {
            Some(name) => PackageAction::Remove(name.to_string()),
            None => PackageAction::Install(spec.to_string()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            PackageAction::Install(name) | PackageAction::Remove(name) => name,
        }
    }

    pub fn quoted_name(&self) -> String {
        crate::utils::helpers::shell_quote(self.name())
    }

    pub fn is_remove(&self) -> bool {
        matches!(self, PackageAction::Remove(_))
    }
}
