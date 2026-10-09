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
pub(crate) struct Spawned;
pub(crate) struct GateFailure {
    pub(crate) group: Box<Group>,
}
impl std::fmt::Debug for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unsupported")
    }
}
pub(crate) struct Group;
pub(crate) struct Reap {
    pub(crate) reaped: bool,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) output_forced: bool,
}
pub(crate) fn spawn(launch: Launch<'_>, _: Tails, _: &CleanupTracker) -> Result<Spawned, Fault> {
    let _ = (
        launch.shell,
        launch.command,
        launch.directory,
        launch.environment,
    );
    Err(Fault::Unsupported)
}
impl Spawned {
    pub(crate) async fn release_gate(self) -> Result<Group, GateFailure> {
        Err(GateFailure {
            group: Box::new(Group),
        })
    }
    pub(crate) fn withhold_gate(self) -> Group {
        Group
    }
}
impl Group {
    pub(crate) async fn exited(&mut self) -> std::io::Result<ExitStatus> {
        Err(std::io::ErrorKind::Unsupported.into())
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
