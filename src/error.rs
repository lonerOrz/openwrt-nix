use std::fmt;

#[derive(Debug)]
pub(crate) enum ConfigError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Validation(String),
    Sops(String),
    Deploy(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "{e}"),
            ConfigError::Json(e) => write!(f, "Failed to parse JSON: {e}"),
            ConfigError::Validation(msg) => write!(f, "{msg}"),
            ConfigError::Sops(msg) => write!(f, "{msg}"),
            ConfigError::Deploy(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(e: serde_json::Error) -> Self {
        ConfigError::Json(e)
    }
}
