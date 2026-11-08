use crate::utils::error::ConfigError;

/// A package name validated against the apk/opkg charset
/// (`[A-Za-z0-9][A-Za-z0-9.+_-]*`).
///
/// Parsing at the config boundary makes shell injection via package names
/// unrepresentable: the charset excludes whitespace, quotes, `$`, backticks,
/// `;`, `&`, `|`, `/`, and globs, so a downstream call site cannot smuggle
/// shell syntax even if it forgets to quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageName(String);

impl PackageName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for PackageName {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if is_valid_name(value) {
            Ok(Self(value.to_string()))
        } else {
            Err(ConfigError::Validation(format!(
                "Invalid package name '{value}': must match [A-Za-z0-9][A-Za-z0-9.+_-]*"
            )))
        }
    }
}

fn is_valid_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
}

/// One entry of the `packages` list: `"name"` installs, `"-name"` removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageAction {
    Install(PackageName),
    Remove(PackageName),
}

impl PackageAction {
    /// Parse a package spec. A lone `-` or an invalid name part is a config
    /// error, never a silent skip.
    pub fn parse(spec: &str) -> Result<Self, ConfigError> {
        match spec.strip_prefix('-') {
            Some(name) => Ok(Self::Remove(PackageName::try_from(name)?)),
            None => Ok(Self::Install(PackageName::try_from(spec)?)),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Install(name) | Self::Remove(name) => name.as_str(),
        }
    }

    pub fn quoted_name(&self) -> String {
        crate::utils::helpers::shell_quote(self.name())
    }

    pub fn is_remove(&self) -> bool {
        matches!(self, Self::Remove(_))
    }
}

/// An absolute path on the target device, safe to interpolate into a remote
/// command once shell-quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePath(String);

impl RemotePath {
    /// Destination for a streamed local package: `/tmp/{file_name}`.
    ///
    /// `file_name` must be a plain filename component (same charset as
    /// [`PackageName`]), so the value can neither escape `/tmp` nor carry
    /// shell syntax.
    pub fn tmp(file_name: &str) -> Result<Self, ConfigError> {
        if is_valid_name(file_name) {
            Ok(Self(format!("/tmp/{file_name}")))
        } else {
            Err(ConfigError::Validation(format!(
                "Invalid local package file name '{file_name}': must be a plain filename \
                 (no '/', whitespace, or shell metacharacters)"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_accepts_real_world_names() {
        for ok in [
            "luci",
            "tcpdump",
            "prometheus-node-exporter-lua",
            "luci-i18n-nlbwmon-zh-cn",
            "libxml2",
            "gtk+",
            "python3-light",
            "kmod-nf-conntrack",
            "wpad-basic-mbedtls",
            "0ad",
        ] {
            assert!(PackageName::try_from(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn package_name_rejects_shell_and_path_payloads() {
        for bad in [
            "",
            "-",
            "-abc", // raw name with leading dash (specs peel the dash before this)
            "a b",
            "a;b",
            "a;b|c",
            "$(touch /tmp/x)",
            "`id`",
            "pkg$HOME",
            "pkg\nrm",
            "../etc/passwd",
            "a/b",
            "a'b",
            "pkg*x",
            " pkg",
            "pkg\t",
        ] {
            assert!(
                PackageName::try_from(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn package_action_peels_removal_dash() {
        let install = PackageAction::parse("luci").unwrap();
        assert_eq!(
            install,
            PackageAction::Install(PackageName::try_from("luci").unwrap())
        );
        let remove = PackageAction::parse("-tcpdump").unwrap();
        assert_eq!(remove.name(), "tcpdump");
        assert!(remove.is_remove());
    }

    #[test]
    fn package_action_rejects_lone_dash_and_invalid_names() {
        assert!(PackageAction::parse("-").is_err());
        assert!(PackageAction::parse("").is_err());
        assert!(PackageAction::parse("-a b").is_err());
        assert!(PackageAction::parse("$(x)").is_err());
    }

    #[test]
    fn remote_path_tmp_accepts_package_filenames() {
        for ok in [
            "test_1.0_all.ipk",
            "luci-app-nlbwmon_0.3-1_all.apk",
            "luci-i18n-nlbwmon-zh-cn_0.3-1_all.ipk",
        ] {
            let p = RemotePath::tmp(ok).unwrap();
            assert_eq!(p.as_str(), format!("/tmp/{ok}"));
        }
    }

    #[test]
    fn remote_path_tmp_rejects_traversal_and_shell_payloads() {
        for bad in [
            "",
            "a b.ipk",
            "../etc/passwd",
            "a/b.ipk",
            "$(x).ipk",
            "pkg;'id'.ipk",
            ".hidden",
        ] {
            assert!(RemotePath::tmp(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
