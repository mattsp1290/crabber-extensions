use super::Tail;
use crabber::extension::CleanupTracker;
use std::{
    path::Path,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::Duration,
};

pub(crate) struct Launch<'a> {
    pub(crate) shell: &'a Path,
    pub(crate) command: &'a str,
    pub(crate) directory: &'a Path,
    pub(crate) environment: &'a [(String, String)],
}
#[derive(Clone)]
pub(crate) struct Tails {
    pub(crate) stdout: Arc<Mutex<Tail>>,
    pub(crate) stderr: Arc<Mutex<Tail>>,
}
#[derive(Debug)]
pub(crate) enum Fault {
    Unsupported,
}
pub(crate) enum GroupSignal {
    Terminate,
    Kill,
}
pub(crate) enum SignalFault {
    Gone,
    Failed,
}
pub(crate) enum Phase {
    Initial,
    Terminated,
    Killed,
}
pub(crate) struct Spawned;
pub(crate) struct Group;
pub(crate) struct Reap {
    pub(crate) reaped: bool,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) output_forced: bool,
}
pub(crate) fn spawn(_: Launch<'_>, _: Tails, _: &CleanupTracker) -> Result<Spawned, Fault> {
    Err(Fault::Unsupported)
}
impl Spawned {
    pub(crate) fn pgid(&self) -> u32 {
        0
    }
    pub(crate) async fn release_gate(self) -> Result<Group, Fault> {
        Err(Fault::Unsupported)
    }
    pub(crate) fn withhold_gate(self) -> Group {
        Group
    }
}
impl Group {
    pub(crate) fn pgid(&self) -> u32 {
        0
    }
    pub(crate) async fn exited(&mut self) -> std::io::Result<ExitStatus> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn signal(&self, _: GroupSignal) -> Result<(), SignalFault> {
        Err(SignalFault::Failed)
    }
    pub(crate) async fn terminate(&mut self, _: GroupSignal, _: Duration, _: Duration) -> Reap {
        Reap::pending()
    }
    pub(crate) async fn sweep_and_reap(&mut self, _: ExitStatus, _: Duration) -> Reap {
        Reap::pending()
    }
}
impl Reap {
    fn pending() -> Self {
        Self {
            reaped: false,
            status: None,
            output_forced: false,
        }
    }
}
