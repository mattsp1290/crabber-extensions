use std::{
    path::Path,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use crabber::extension::CleanupTracker;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::watch,
    time::{Instant, timeout_at},
};
use tokio_util::sync::CancellationToken;

use super::{Tail, script::SUPERVISOR_SCRIPT};

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Spawn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupSignal {
    Terminate,
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalFault {
    Gone,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Initial,
    Terminated,
    Killed,
}

pub(crate) struct Reap {
    pub(crate) reaped: bool,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) output_forced: bool,
}

pub(crate) struct Spawned {
    group: Group,
    stdin: ChildStdin,
}

pub(crate) struct GateFailure {
    pub(crate) group: Box<Group>,
}
impl std::fmt::Debug for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

impl Spawned {
    #[cfg(test)]
    pub(crate) fn pgid(&self) -> Pid {
        self.group.pgid()
    }

    pub(crate) async fn release_gate(self) -> Result<Group, GateFailure> {
        let Self { group, mut stdin } = self;
        let result = stdin.write_all(b"G\n").await;
        drop(stdin);
        match result {
            Ok(()) => Ok(group),
            Err(_) => Err(GateFailure {
                group: Box::new(group),
            }),
        }
    }

    pub(crate) fn withhold_gate(self) -> Group {
        drop(self.stdin);
        self.group
    }
}

pub(crate) fn spawn(
    launch: Launch<'_>,
    tails: Tails,
    tracker: &CleanupTracker,
) -> Result<Spawned, Fault> {
    let mut child = Command::new(launch.shell)
        .args(["-c", SUPERVISOR_SCRIPT, "background-job-supervisor"])
        .arg(launch.shell)
        .arg(launch.command)
        .current_dir(launch.directory)
        .env_clear()
        .envs(launch.environment.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Fault::Spawn)?;
    let pgid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .ok_or(Fault::Spawn)?;
    let stdin = child.stdin.take().ok_or(Fault::Spawn)?;
    let stdout = child.stdout.take().ok_or(Fault::Spawn)?;
    let stderr = child.stderr.take().ok_or(Fault::Spawn)?;
    let pumps = Pumps {
        stdout: start_pump(stdout, tails.stdout, tracker),
        stderr: start_pump(stderr, tails.stderr, tracker),
    };
    Ok(Spawned {
        group: Group {
            child,
            pgid,
            pumps,
            phase: Phase::Initial,
            term_deadline: None,
            swept: false,
            #[cfg(test)]
            hooks: TestHooks::default(),
        },
        stdin,
    })
}

pub(crate) struct Group {
    child: Child,
    pgid: Pid,
    pumps: Pumps,
    phase: Phase,
    term_deadline: Option<Instant>,
    swept: bool,
    #[cfg(test)]
    pub(super) hooks: TestHooks,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestHooks {
    pub(super) signals: Mutex<Vec<GroupSignal>>,
    pub(super) fail_kill_once: std::sync::atomic::AtomicBool,
    pub(super) reap_timeout_once: std::sync::atomic::AtomicBool,
}

pub(crate) fn signal_group(pgid: Pid, signal: GroupSignal) -> Result<(), SignalFault> {
    let signal = match signal {
        GroupSignal::Terminate => Signal::TERM,
        GroupSignal::Kill => Signal::KILL,
    };
    kill_process_group(pgid, signal).map_err(|error| {
        if error == rustix::io::Errno::SRCH {
            SignalFault::Gone
        } else {
            SignalFault::Failed
        }
    })
}

impl Group {
    #[cfg(test)]
    pub(crate) fn pgid(&self) -> Pid {
        self.pgid
    }

    pub(crate) async fn exited(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait().await
    }

    pub(crate) fn signal(&mut self, signal: GroupSignal) -> Result<(), SignalFault> {
        #[cfg(test)]
        {
            self.hooks.signals.lock().unwrap().push(signal);
            if signal == GroupSignal::Kill
                && self
                    .hooks
                    .fail_kill_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(SignalFault::Failed);
            }
        }
        let result = signal_group(self.pgid, signal);
        if result == Err(SignalFault::Failed)
            && self.child.try_wait().is_ok_and(|status| status.is_some())
        {
            // Darwin can return EPERM for a group containing only a zombie.
            // Reap our owned leader, then retry: ESRCH is authoritative absence;
            // a real permission failure remains Failed. Tokio caches the status.
            return signal_group(self.pgid, signal);
        }
        result
    }

    pub(crate) async fn terminate(
        &mut self,
        first: GroupSignal,
        grace: Duration,
        kill_wait: Duration,
    ) -> Reap {
        let mut status = None;
        if self.phase == Phase::Initial && first == GroupSignal::Terminate {
            match self.signal(GroupSignal::Terminate) {
                Ok(()) => {
                    self.phase = Phase::Terminated;
                    self.term_deadline = Some(Instant::now() + grace);
                }
                Err(SignalFault::Gone) => {
                    self.phase = Phase::Killed;
                    self.swept = true;
                }
                Err(SignalFault::Failed) => return Reap::pending(),
            }
        }
        if self.phase == Phase::Terminated {
            // Persist the deadline across dropped futures and failed signals.
            let deadline = self.term_deadline.expect("TERM phase has a deadline");
            if let Ok(Ok(exit)) = timeout_at(deadline, self.child.wait()).await {
                status = Some(exit);
            }
        }
        if self.phase != Phase::Killed {
            if self
                .signal(GroupSignal::Kill)
                .is_err_and(|fault| fault != SignalFault::Gone)
            {
                return Reap::pending();
            }
            self.phase = Phase::Killed;
            self.swept = true;
        }
        #[cfg(test)]
        if self
            .hooks
            .reap_timeout_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Reap {
                reaped: false,
                status,
                output_forced: false,
            };
        }
        let deadline = Instant::now() + kill_wait;
        match timeout_at(deadline, self.child.wait()).await {
            Ok(Ok(exit)) => {
                status = Some(exit);
                let output_forced = self.pumps.settle(deadline).await;
                Reap {
                    reaped: true,
                    status,
                    output_forced,
                }
            }
            _ => Reap {
                reaped: false,
                status,
                output_forced: false,
            },
        }
    }

    pub(crate) async fn sweep_and_reap(&mut self, status: ExitStatus, kill_wait: Duration) -> Reap {
        // A failed sweep leaves Drop armed to retry the group signal.
        self.swept = !self
            .signal(GroupSignal::Kill)
            .is_err_and(|fault| fault != SignalFault::Gone);
        self.phase = Phase::Killed;
        let output_forced = self.pumps.settle(Instant::now() + kill_wait).await;
        Reap {
            reaped: true,
            status: Some(status),
            output_forced,
        }
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

impl Drop for Group {
    fn drop(&mut self) {
        if !self.swept {
            let _ = signal_group(self.pgid, GroupSignal::Kill);
        }
        self.pumps.stdout.abandon.cancel();
        self.pumps.stderr.abandon.cancel();
    }
}

struct Pumps {
    stdout: PumpHandle,
    stderr: PumpHandle,
}
struct PumpHandle {
    abandon: CancellationToken,
    done: watch::Receiver<bool>,
    tail: Arc<Mutex<Tail>>,
}

impl Pumps {
    async fn settle(&mut self, deadline: Instant) -> bool {
        let completed = timeout_at(deadline, async {
            let _ = self.stdout.done.wait_for(|done| *done).await;
            let _ = self.stderr.done.wait_for(|done| *done).await;
        })
        .await
        .is_ok();
        if !completed {
            // Publish truncation before returning, without waiting for the
            // abandoned readers to be scheduled.
            self.stdout.tail.lock().unwrap().mark_truncated();
            self.stderr.tail.lock().unwrap().mark_truncated();
            self.stdout.abandon.cancel();
            self.stderr.abandon.cancel();
        }
        !completed
    }
}

fn start_pump(
    reader: impl AsyncRead + Unpin + Send + 'static,
    tail: Arc<Mutex<Tail>>,
    tracker: &CleanupTracker,
) -> PumpHandle {
    let abandon = CancellationToken::new();
    let (done_tx, done) = watch::channel(false);
    let handle = PumpHandle {
        abandon: abandon.clone(),
        done,
        tail: tail.clone(),
    };
    tracker.spawn(async move {
        pump(reader, tail, abandon).await;
        let _ = done_tx.send(true);
    });
    handle
}

pub(super) async fn pump(
    mut reader: impl AsyncRead + Unpin,
    tail: Arc<Mutex<Tail>>,
    abandon: CancellationToken,
) {
    let mut bytes = [0; 8192];
    loop {
        tokio::select! {
            biased;
            () = abandon.cancelled() => { tail.lock().unwrap().mark_truncated(); return; }
            result = reader.read(&mut bytes) => match result {
                Ok(0) => return,
                Ok(count) => tail.lock().unwrap().write(&bytes[..count]),
                Err(_) => { tail.lock().unwrap().mark_truncated(); return; }
            }
        }
    }
}
