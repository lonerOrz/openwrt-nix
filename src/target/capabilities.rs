//! Runtime discovery of target capabilities.
//!
//! The package manager actually installed on the target (`apk` on OpenWrt 25.12+,
//! `opkg` on older releases) is authoritative: a box cannot run the other's
//! commands. The JSON `packageManager` field is only a hint. We probe the target
//! over SSH and use the detected backend for execution; a mismatch with the declared
//! field is reported but does not block a valid deployment (the probe wins).

use crate::config::models::PkgBackend;
use crate::target::deploy::{DeployConfig, SshExec};
use crate::utils::error::ConfigError;

pub(crate) const PKG_MGR_PROBE: &str = "\
if command -v apk >/dev/null 2>&1; then echo apk; \
elif command -v opkg >/dev/null 2>&1; then echo opkg; \
else echo none; fi";

pub fn discover_package_manager(
    ssh: &dyn SshExec,
    target: &str,
    config: &DeployConfig,
) -> Result<PkgBackend, ConfigError> {
    let out = ssh.exec(target, PKG_MGR_PROBE, None, config)?;
    match out.trim() {
        "apk" => Ok(PkgBackend::Apk),
        "opkg" => Ok(PkgBackend::Opkg),
        other => Err(ConfigError::Validation(format!(
            "Could not detect a supported package manager on target {target} (probe returned: {other})"
        ))),
    }
}

/// Reconcile the declared backend with the probed one: the probed backend is
/// always returned; a mismatch is reported as an error naming both values.
pub fn resolve_backend(declared: &str, discovered: PkgBackend) -> Result<PkgBackend, ConfigError> {
    // Validate the declared value first: a typo like "apt" must still be rejected.
    let declared = PkgBackend::try_from(declared)?;
    if declared != discovered {
        return Err(ConfigError::Validation(format!(
            "declared package manager '{declared:?}' differs from target's detected '{discovered:?}'; using detected backend"
        )));
    }
    Ok(discovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProbeSsh {
        reply: String,
    }

    impl SshExec for ProbeSsh {
        fn exec(
            &self,
            _target: &str,
            cmd: &str,
            _stdin_data: Option<&[u8]>,
            _config: &DeployConfig,
        ) -> Result<String, ConfigError> {
            assert!(
                cmd.contains("command -v apk"),
                "capabilities must run the probe"
            );
            Ok(self.reply.clone())
        }
    }

    fn cfg() -> DeployConfig {
        DeployConfig {
            port: 22,
            identity_file: None,
            force: false,
            no_sops: false,
            watchdog_timeout: 60,
            verbose: false,
        }
    }

    #[test]
    fn probe_detects_apk() {
        let ssh = ProbeSsh {
            reply: "apk\n".into(),
        };
        assert_eq!(
            discover_package_manager(&ssh, "t", &cfg()).unwrap(),
            PkgBackend::Apk
        );
    }

    #[test]
    fn probe_detects_opkg() {
        let ssh = ProbeSsh {
            reply: "opkg\n".into(),
        };
        assert_eq!(
            discover_package_manager(&ssh, "t", &cfg()).unwrap(),
            PkgBackend::Opkg
        );
    }

    #[test]
    fn probe_errors_when_none() {
        let ssh = ProbeSsh {
            reply: "none\n".into(),
        };
        let err = discover_package_manager(&ssh, "box", &cfg()).unwrap_err();
        assert!(format!("{err}").contains("Could not detect a supported package manager"));
    }

    #[test]
    fn resolve_uses_probe_on_match() {
        assert_eq!(
            resolve_backend("opkg", PkgBackend::Opkg).unwrap(),
            PkgBackend::Opkg
        );
        assert_eq!(
            resolve_backend("apk", PkgBackend::Apk).unwrap(),
            PkgBackend::Apk
        );
    }

    #[test]
    fn resolve_rejects_typo_declared_value() {
        // declared is invalid -> still rejected even though target is fine.
        let err = resolve_backend("apt", PkgBackend::Apk).unwrap_err();
        assert!(format!("{err}").contains("Unsupported package manager: apt"));
    }

    #[test]
    fn resolve_mismatch_is_reported_but_probe_wins() {
        // declared opkg but target is apk: error carries the mismatch, but the
        // detected backend is what callers should use for execution.
        let err = resolve_backend("opkg", PkgBackend::Apk).unwrap_err();
        assert!(format!("{err}").contains("differs from target"));
    }
}
