use std::collections::BTreeMap;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Deserialize, Debug)]
pub(crate) struct Root {
    #[serde(default = "default_package_manager")]
    #[serde(rename = "packageManager")]
    pub(crate) package_manager: String,
    pub(crate) settings: BTreeMap<String, BTreeMap<String, Section>>,
    pub(crate) packages: Option<Vec<String>>,
    pub(crate) opkg: Option<Opkg>,
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
pub(crate) struct Opkg {
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
