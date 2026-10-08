use super::*;
use crabber::extension::CleanupOwner;
use rustix::process::{Pid, Signal};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(3);

fn shell_available() -> bool {
    use std::os::unix::fs::PermissionsExt;
    let available = std::fs::metadata("/bin/sh").is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        && std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,pgid=,stat="])
            .output()
            .is_ok_and(|o| o.status.success());
    if !available {
        assert_ne!(
            std::env::var("BACKGROUND_JOBS_REQUIRE_SHELL").as_deref(),
            Ok("1"),
            "required /bin/sh or ps absent"
        );
        eprintln!("background_jobs: /bin/sh or ps absent, skipping");
    }
    available
}

fn group_members(pgid: Pid) -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let group: i32 = fields.next()?.parse().ok()?;
            let state = fields.next()?;
            (group == pgid.as_raw_nonzero().get() && !state.starts_with('Z')).then_some(pid)
        })
        .collect()
}

async fn group_gone(pgid: Pid) {
    timeout(WAIT, async {
        while !group_members(pgid).is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("group still has live members");
}

struct Fixture {
    owner: CleanupOwner,
    directory: tempfile::TempDir,
    tails: Tails,
}
impl Fixture {
    fn new() -> Self {
        Self {
            owner: CleanupOwner::new(),
            directory: tempfile::tempdir().unwrap(),
            tails: Tails {
                stdout: Arc::new(Mutex::new(Tail::new(4096))),
                stderr: Arc::new(Mutex::new(Tail::new(4096))),
            },
        }
    }
    fn spawn(&self, command: &str, environment: &[(String, String)]) -> Spawned {
        spawn(
            Launch {
                shell: Path::new("/bin/sh"),
                command,
                directory: self.directory.path(),
                environment,
            },
            self.tails.clone(),
            &self.owner.tracker(),
        )
        .unwrap()
    }
    async fn finish(&self, pgid: Pid) {
        self.owner.join(WAIT).await.unwrap();
        group_gone(pgid).await;
    }
    async fn released(&self, command: &str) -> Group {
        self.spawn(command, &[]).release_gate().await.unwrap()
    }
}

#[test]
fn supervisor_protocol_and_digest_are_frozen() {
    assert_eq!(SUPERVISOR_PROTOCOL, "posix-anchor-supervisor-v1");
    assert_eq!(
        supervisor_digest(),
        "09ab3cf71c48b1be62f3e08759218b271bf15c1b31b5a8bb21307faec9e75f5e"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn natural_exit_code_and_output_are_exact() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("printf out; printf err >&2; exit 3").await;
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(3));
    let reap = group.sweep_and_reap(status, WAIT).await;
    assert!(reap.reaped);
    assert_eq!(reap.status.unwrap().code(), Some(3));
    assert!(!reap.output_forced);
    assert_eq!(
        f.tails.stdout.lock().unwrap().snapshot(),
        ("out".into(), false)
    );
    assert_eq!(
        f.tails.stderr.lock().unwrap().snapshot(),
        ("err".into(), false)
    );
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn withheld_gate_never_runs_the_command() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let command = ": > canary";
    let spawned = f.spawn(command, &[]);
    let pgid = spawned.pgid();
    let mut group = spawned.withhold_gate();
    sleep(Duration::from_millis(300)).await;
    assert!(!f.directory.path().join("canary").exists());
    assert!(
        group
            .terminate(GroupSignal::Kill, Duration::ZERO, WAIT)
            .await
            .reaped
    );
    assert!(!f.directory.path().join("canary").exists());
    f.finish(pgid).await;
    let mut group = f.released(command).await;
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    assert!(status.success());
    assert!(f.directory.path().join("canary").exists());
    group.sweep_and_reap(status, WAIT).await;
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_release_after_supervisor_death_is_a_gate_fault() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let spawned = f.spawn(": > canary", &[]);
    let pgid = spawned.pgid();
    signal_group(pgid, GroupSignal::Kill).unwrap();
    match spawned.release_gate().await {
        Err(mut error) => {
            assert!(
                error
                    .group
                    .terminate(GroupSignal::Kill, Duration::ZERO, WAIT)
                    .await
                    .reaped
            );
        }
        Ok(mut group) => {
            let reap = group
                .terminate(GroupSignal::Kill, Duration::ZERO, WAIT)
                .await;
            assert!(
                reap.reaped,
                "supervisor kill/reap failed; members={:?}",
                group_members(pgid)
            );
        }
    }
    f.finish(pgid).await;
    assert!(!f.directory.path().join("canary").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn anchor_keeps_the_group_alive_until_the_sweep() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("exit 0").await;
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    assert_eq!(group_members(pgid).len(), 1);
    group.sweep_and_reap(status, WAIT).await;
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anchor_ignores_term_and_the_command_receives_it() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released(": > ready; sleep 100").await;
    let pgid = group.pgid();
    timeout(WAIT, async {
        while !f.directory.path().join("ready").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    group.signal(GroupSignal::Terminate).unwrap();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(143));
    assert_eq!(group_members(pgid).len(), 1);
    group.sweep_and_reap(status, WAIT).await;
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn terminate_escalates_from_term_to_kill() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("trap '' TERM; : > ready; sleep 100").await;
    let pgid = group.pgid();
    timeout(WAIT, async {
        while !f.directory.path().join("ready").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let start = Instant::now();
    let reap = group
        .terminate(GroupSignal::Terminate, Duration::from_millis(300), WAIT)
        .await;
    assert!(reap.reaped);
    assert!(reap.status.unwrap().code().is_none());
    assert!(start.elapsed() >= Duration::from_millis(300));
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn terminate_retry_does_not_resend_term() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("sleep 100").await;
    let pgid = group.pgid();
    group
        .hooks
        .fail_kill_once
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        !group
            .terminate(GroupSignal::Terminate, Duration::from_millis(100), WAIT)
            .await
            .reaped
    );
    assert!(
        group
            .terminate(GroupSignal::Terminate, Duration::from_millis(100), WAIT)
            .await
            .reaped
    );
    assert_eq!(
        *group.hooks.signals.lock().unwrap(),
        vec![GroupSignal::Terminate, GroupSignal::Kill, GroupSignal::Kill]
    );
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anchor_starts_with_an_empty_path() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f
        .spawn("exit 0", &[("PATH".into(), "".into())])
        .release_gate()
        .await
        .unwrap();
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    assert!(status.success());
    assert_eq!(group_members(pgid).len(), 1);
    group.sweep_and_reap(status, WAIT).await;
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn held_output_is_forced_closed_at_kill_wait() {
    if !shell_available() {
        return;
    }
    if !std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        assert_ne!(
            std::env::var("BACKGROUND_JOBS_REQUIRE_SHELL").as_deref(),
            Ok("1"),
            "required python3 absent"
        );
        eprintln!("background_jobs: python3 absent, skipping");
        return;
    }
    let f = Fixture::new();
    // The child publishes readiness after setsid, removing a sweep race.
    let mut group = f.spawn("python3 -c 'import os,time; os.setsid(); open(\"holder\",\"w\").write(str(os.getpid())); time.sleep(60)' & while [ ! -s holder ]; do :; done; exit 0", &[("PATH".into(), std::env::var("PATH").unwrap())]).release_gate().await.unwrap();
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    let holder: i32 = std::fs::read_to_string(f.directory.path().join("holder"))
        .unwrap()
        .parse()
        .unwrap();
    struct Holder(Pid);
    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = rustix::process::kill_process(self.0, Signal::KILL);
        }
    }
    let holder = Holder(Pid::from_raw(holder).unwrap());
    let start = Instant::now();
    let reap = group
        .sweep_and_reap(status, Duration::from_millis(500))
        .await;
    assert!(reap.reaped && reap.output_forced);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(f.tails.stdout.lock().unwrap().snapshot().1);
    assert!(f.tails.stderr.lock().unwrap().snapshot().1);
    drop(holder);
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pump_abandon_marks_truncated() {
    let (_writer, reader) = tokio::io::duplex(32);
    let tail = Arc::new(Mutex::new(Tail::new(32)));
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(unix::pump(reader, tail.clone(), cancel.clone()));
    cancel.cancel();
    timeout(Duration::from_millis(100), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(tail.lock().unwrap().snapshot().1);
}

#[test]
fn signal_on_a_missing_group_is_gone() {
    assert_eq!(
        signal_group(Pid::from_raw(i32::MAX).unwrap(), GroupSignal::Kill),
        Err(SignalFault::Gone)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn environment_and_directory_are_applied_and_frozen() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f
        .spawn(
            "printf '%s|%s|%s' \"$FROZEN\" \"$(pwd -P)\" \"$HOME\"",
            &[("FROZEN".into(), "synthetic".into())],
        )
        .release_gate()
        .await
        .unwrap();
    let pgid = group.pgid();
    let status = timeout(WAIT, group.exited()).await.unwrap().unwrap();
    group.sweep_and_reap(status, WAIT).await;
    assert_eq!(
        f.tails.stdout.lock().unwrap().snapshot().0,
        format!(
            "synthetic|{}|",
            f.directory.path().canonicalize().unwrap().display()
        )
    );
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_of_a_live_group_kills_the_group() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let group = f.released("sleep 100").await;
    let pgid = group.pgid();
    drop(group);
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_after_exit_before_sweep_kills_the_anchor() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("exit 0").await;
    let pgid = group.pgid();
    timeout(WAIT, group.exited()).await.unwrap().unwrap();
    drop(group);
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_termination_keeps_the_original_grace_deadline() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("trap '' TERM; : > ready; sleep 100").await;
    let pgid = group.pgid();
    timeout(WAIT, async {
        while !f.directory.path().join("ready").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        timeout(
            Duration::from_millis(100),
            group.terminate(GroupSignal::Terminate, Duration::from_millis(300), WAIT)
        )
        .await
        .is_err()
    );
    sleep(Duration::from_millis(250)).await;
    let reap = timeout(
        Duration::from_secs(1),
        group.terminate(GroupSignal::Terminate, Duration::from_secs(10), WAIT),
    )
    .await
    .unwrap();
    assert!(reap.reaped);
    assert_eq!(
        *group.hooks.signals.lock().unwrap(),
        vec![GroupSignal::Terminate, GroupSignal::Kill]
    );
    f.finish(pgid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reap_timeout_retry_does_not_resend_signals() {
    if !shell_available() {
        return;
    }
    let f = Fixture::new();
    let mut group = f.released("sleep 100").await;
    let pgid = group.pgid();
    group
        .hooks
        .reap_timeout_once
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let first = group
        .terminate(GroupSignal::Kill, Duration::ZERO, WAIT)
        .await;
    assert!(!first.reaped);
    assert!(!first.output_forced);
    let second = group
        .terminate(GroupSignal::Terminate, Duration::from_secs(10), WAIT)
        .await;
    assert!(second.reaped);
    assert!(second.status.is_some());
    assert_eq!(
        *group.hooks.signals.lock().unwrap(),
        vec![GroupSignal::Kill]
    );
    f.finish(pgid).await;
}
