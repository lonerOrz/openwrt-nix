//! `DeployPlan`: the structured, render-agnostic description of one deployment.
//!
//! `run()` builds a plan from the compiled config plus runtime inputs (deployer
//! key, watchdog timeout), then renders it to a remote script string. Splitting
//! the *what* (plan) from the *how* (renderer) is the mount point for the
//! transaction/rollback layer: once every operation type is a typed field here,
//! the executor can apply, verify, and roll back each independently instead of
//! shipping one opaque shell blob.

use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::config::models::Root;
use crate::target::remote_script::build_remote_script;

/// A resolved, render-ready deployment plan.
///
/// `root` and `secrets` are borrowed from the compiled config; the rest are
/// owned inputs assembled by `run()`. The plan performs no IO and emits no
/// shell — it is a typed seam between orchestration and rendering.
pub struct DeployPlan<'a> {
    /// Resolved config after secret interpolation.
    pub root: &'a Root,
    /// Interpolated secrets, e.g. `root_password`.
    pub secrets: &'a HashMap<String, String>,
    /// UCI batch: orphan deletions followed by the compiled `uci_batch`.
    pub uci_commands: String,
    /// Active deployer public key, if a local agent key is available.
    pub deployer_key: Option<String>,
    /// Config names that changed (drives the reload map).
    pub managed_configs: Vec<String>,
    /// service-name → reload command overrides.
    pub service_map: BTreeMap<String, String>,
    /// Seconds before the rollback watchdog fires.
    pub watchdog_timeout: u64,
}

impl<'a> DeployPlan<'a> {
    /// Construct a plan from its inputs. Rendering is intentionally deferred.
    pub fn new(
        root: &'a Root,
        secrets: &'a HashMap<String, String>,
        uci_commands: String,
        deployer_key: Option<String>,
        managed_configs: Vec<String>,
        service_map: BTreeMap<String, String>,
        watchdog_timeout: u64,
    ) -> Self {
        Self {
            root,
            secrets,
            uci_commands,
            deployer_key,
            managed_configs,
            service_map,
            watchdog_timeout,
        }
    }

    /// Render the plan into the remote deploy script. Pure: delegates to the
    /// existing segment renderers and returns a single shell string, identical
    /// to the one `run()` produced before this seam existed.
    pub fn render(&self) -> String {
        build_remote_script(
            self.root,
            self.secrets,
            &self.uci_commands,
            self.deployer_key.as_deref(),
            &self.managed_configs,
            &self.service_map,
            self.watchdog_timeout,
        )
    }
}
