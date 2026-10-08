//! Bounded, non-interactive jobs owned by a durable session and workspace.
//!
//! Hosts provision the POSIX shell and own process policy, credentials and trust.
//! Working-directory checks constrain launch only; this is not a sandbox.
//! Jobs and output live in memory and do not survive a host restart. Crabber may
//! execute a pending start once when resuming a paused run; Running calls are
//! interrupted on recovery. Hosts settle runs before changing fingerprinted policy.
#![cfg_attr(not(test), allow(dead_code))]

mod config;
mod input;
mod job;
mod manager;
#[cfg(all(test, unix))]
mod tests;
mod time;

pub use config::{Environment, EnvironmentMode, Limits, Options};
use crabber::extension::ExtensionError;
use manager::Policy;
use std::sync::Arc;

/// Tool for starting a bounded command.
pub const START_TOOL: &str = "background_job_start";
/// Tool for reading state and bounded output tails.
pub const STATUS_TOOL: &str = "background_job_status";
/// Tool for listing the caller's tracked jobs.
pub const LIST_TOOL: &str = "background_job_list";
/// Tool for group-wide termination.
pub const KILL_TOOL: &str = "background_job_kill";
/// Start permission metadata; the host decides access.
pub const PERMISSION_START: &str = "background.process.start";
/// Status/list permission metadata; the host decides access.
pub const PERMISSION_READ: &str = "background.process.read";
/// Kill permission metadata; the host decides access.
pub const PERMISSION_KILL: &str = "background.process.kill";

/// Trusted process manager with shared capacity across mounts.
///
/// Use a Tokio runtime with `enable_all()`: process support needs the I/O driver
/// and installs a process-wide SIGCHLD handler on first use. Closing any mount
/// closes all jobs of this instance, so use separate instances for independent
/// registries/tenants. Interrupt active runs before closing mounts. A close is
/// bounded; inspect [`Self::live_jobs`] for survivors. No shell discovery,
/// provisioning, sandbox or durable process recovery is provided.
pub struct BackgroundJobs {
    policy: Arc<Policy>,
    hash: String,
}
impl BackgroundJobs {
    /// Validate bounds and identities, canonicalize the executable, and freeze
    /// the effective environment. Non-Unix targets reject construction.
    ///
    /// Environment values do not enter the fingerprint: rotate the environment
    /// identity whenever effective values or host routing policy change.
    pub fn new(options: Options) -> Result<Self, ExtensionError> {
        let (configuration, hash) = config::validate(options)?;
        Ok(Self {
            policy: Arc::new(Policy::new(configuration)),
            hash,
        })
    }

    /// Diagnostic snapshot of running, starting and hidden jobs. Finished
    /// tracked jobs do not count. A bounded close may leave live survivors.
    pub fn live_jobs(&self) -> usize {
        self.policy.live_jobs()
    }
}

fn input_error(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("background jobs input invalid: {code}"))
}
fn runtime_error(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("background jobs runtime invalid: {code}"))
}
fn failure(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("background jobs operation failed: {code}"))
}
