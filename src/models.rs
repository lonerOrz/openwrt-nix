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
