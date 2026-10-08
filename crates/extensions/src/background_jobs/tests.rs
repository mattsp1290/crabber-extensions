use super::*;
use super::{
    job::{Cause, Job, State},
    manager::{Owner, Policy},
};
use crate::process::{Reap, Tail, Tails};
use crabber::{
    core::{RunId, SessionId, ToolCallId},
    extension::{HostServices, ToolContext, WorkspaceContext},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::process::ExitStatusExt,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

fn options() -> Options {
    Options {
        shell_path: "/bin/sh".into(),
        shell_identity: "test-sh-v1".into(),
        environment: Environment {
            mode: EnvironmentMode::ExplicitOnly,
            overrides: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            identity: "test-env-v1".into(),
        },
        limits: Limits {
            max_running: 2,
            max_tracked: 4,
            max_command_bytes: 4096,
            max_working_directory_bytes: 1024,
            max_output_bytes_per_stream: 4096,
            max_environment_entries: 1024,
            max_environment_bytes: 256 * 1024,
            default_timeout: Duration::ZERO,
            max_timeout: Duration::from_secs(30),
            terminate_grace: Duration::from_millis(100),
            kill_wait: Duration::from_secs(2),
            shutdown_grace: Duration::from_secs(3),
        },
    }
}
fn policy(options: Options) -> Arc<Policy> {
    BackgroundJobs::new(options).unwrap().policy
}
fn owner() -> Owner {
    Owner {
        session: SessionId::from("session"),
        workspace: "workspace".into(),
    }
}
fn tails() -> Tails {
    Tails {
        stdout: Arc::new(Mutex::new(Tail::new(4096))),
        stderr: Arc::new(Mutex::new(Tail::new(4096))),
    }
}
fn context(dir: &std::path::Path, cancel: CancellationToken) -> ToolContext {
    ToolContext::new(
        SessionId::from("session"),
        RunId::from("run"),
        ToolCallId::from("call"),
        cancel,
        HostServices::default(),
        WorkspaceContext::from_persisted("workspace", dir.to_str().unwrap()),
        Arc::new(|_| {}),
        None,
    )
}
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
            "required shell/ps unavailable"
        );
        eprintln!("background_jobs: /bin/sh or ps absent, skipping");
    }
    available
}
fn group_members(pgid: i32) -> Vec<u32> {
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
            (group == pgid && !state.starts_with('Z')).then_some(pid)
        })
        .collect()
}
async fn finish(policy: &Policy) {
    policy.close(Duration::from_secs(5)).await.unwrap();
    policy.cleanup.join(Duration::from_secs(3)).await.unwrap();
    let pgids = policy.test_hooks.lock().unwrap().pgids.clone();
    for pgid in pgids {
        timeout(Duration::from_secs(3), async {
            while !group_members(pgid).is_empty() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(policy.live_jobs(), 0);
}
async fn terminal(policy: &Policy, dir: &std::path::Path, id: &str) -> Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let status = policy
                .status(context(dir, CancellationToken::new()), json!({"id":id}))
                .unwrap();
            if status["state"] != "running" {
                return status;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[test]
fn ids_are_53_bytes_and_monotonic_within_a_manager() {
    let p = policy(options());
    let first = p.reserve_start().unwrap();
    let id = first.id.clone();
    drop(first);
    let second = p.reserve_start().unwrap();
    assert_eq!(id.len(), 53);
    assert!(id < second.id);
    assert_eq!(input::id(&json!({"id":id})).unwrap(), id);
}
#[test]
fn two_managers_have_different_epochs() {
    let a = policy(options());
    let b = policy(options());
    assert_ne!(a.reserve_start().unwrap().id, b.reserve_start().unwrap().id);
}
#[test]
fn reservation_drop_releases_starting() {
    let p = policy(options());
    let r = p.reserve_start().unwrap();
    assert_eq!(p.live_jobs(), 1);
    drop(r);
    assert_eq!(p.live_jobs(), 0);
    assert_eq!(p.registry.lock().unwrap().starting, 0);
}
#[test]
fn cause_is_set_once_and_natural_loses_to_an_earlier_kill() {
    let job = Job::new("id".into(), owner(), 0, tails());
    assert!(job.set_cause_once(Cause::Kill));
    assert!(!job.set_cause_once(Cause::Natural));
    assert_eq!(*job.cause.borrow(), Some(Cause::Kill));
}
#[test]
fn finish_maps_every_cause_and_status_shape() {
    for cause in [
        Cause::Natural,
        Cause::Kill,
        Cause::Timeout,
        Cause::Close,
        Cause::Cancelled,
    ] {
        for status in [
            ExitStatus::from_raw(0),
            ExitStatus::from_raw(7 << 8),
            ExitStatus::from_raw(9),
        ] {
            for forced in [false, true] {
                let job = Job::new("id".into(), owner(), 0, tails());
                let reap = Reap {
                    reaped: true,
                    status: Some(status),
                    output_forced: forced,
                };
                job.finish(cause, &reap);
                let t = job.terminal.lock().unwrap().clone().unwrap();
                let expected = match cause {
                    Cause::Natural if !forced && status.code() == Some(0) => State::Succeeded,
                    Cause::Natural => State::Failed,
                    Cause::Timeout => State::TimedOut,
                    _ => State::Killed,
                };
                assert_eq!(t.state, expected);
                assert_eq!(
                    t.exit_code,
                    if cause == Cause::Natural && !forced {
                        status.code()
                    } else {
                        None
                    }
                );
            }
        }
    }
}
#[test]
fn prune_evicts_oldest_finished_only_when_tracked_is_full() {
    let mut config = options();
    config.limits.max_tracked = 2;
    let p = policy(config);
    let old = Arc::new(Job::new("old".into(), owner(), 0, tails()));
    let new = Arc::new(Job::new("new".into(), owner(), 0, tails()));
    for (job, time) in [
        (&old, UNIX_EPOCH),
        (&new, UNIX_EPOCH + Duration::from_secs(1)),
    ] {
        job.finish(
            Cause::Natural,
            &Reap {
                reaped: true,
                status: Some(ExitStatus::from_raw(0)),
                output_forced: false,
            },
        );
        job.terminal.lock().unwrap().as_mut().unwrap().completed_at = time;
    }
    p.registry.lock().unwrap().jobs.insert("old".into(), old);
    let r = p.reserve_start().unwrap();
    assert!(p.registry.lock().unwrap().jobs.contains_key("old"));
    drop(r);
    p.registry.lock().unwrap().jobs.insert("new".into(), new);
    let r = p.reserve_start().unwrap();
    assert!(!p.registry.lock().unwrap().jobs.contains_key("old"));
    assert!(p.registry.lock().unwrap().jobs.contains_key("new"));
    drop(r);
    let running = Arc::new(Job::new("running".into(), owner(), 0, tails()));
    p.registry
        .lock()
        .unwrap()
        .jobs
        .insert("running".into(), running);
    let _r = p.reserve_start().unwrap();
    assert!(p.registry.lock().unwrap().jobs.contains_key("running"));
}

fn assert_config(options: Options, code: &str) {
    let error = match BackgroundJobs::new(options) {
        Err(error) => error,
        Ok(_) => panic!("configuration accepted: {code}"),
    };
    assert_eq!(
        error.to_string(),
        format!("extension plan failed: extension configuration invalid: {code}")
    );
}
#[test]
fn config_validation_table() {
    type Edit = fn(&mut Options);
    let edits: Vec<(&str, Edit)> = vec![
        ("shell-path", |o| o.shell_path = "relative".into()),
        ("shell-path", |o| o.shell_path = "/bin/../bin/sh".into()),
        ("shell-path", |o| o.shell_path = "/bin/sh/".into()),
        ("shell-path", |o| {
            o.shell_path = "/definitely-missing-synthetic-shell".into()
        }),
        ("shell-executable", |o| o.shell_path = "/tmp".into()),
        ("shell-identity", |o| o.shell_identity.clear()),
        ("environment-identity", |o| {
            o.environment.identity = "bad\nidentity".into()
        }),
        ("environment-entries", |o| {
            o.limits.max_environment_entries = 1;
            o.environment.overrides.insert("EXTRA".into(), "x".into());
        }),
        ("environment-entry", |o| {
            o.environment.overrides.insert("BAD=KEY".into(), "x".into());
        }),
        ("environment-entry", |o| {
            o.environment
                .overrides
                .insert("KEY".into(), "bad\0value".into());
        }),
        ("environment-bytes", |o| o.limits.max_environment_bytes = 1),
        ("tracked-below-running", |o| o.limits.max_tracked = 1),
        ("max-timeout", |o| o.limits.max_timeout = Duration::ZERO),
        ("max-timeout", |o| {
            o.limits.max_timeout = Duration::from_millis(1001)
        }),
        ("max-timeout", |o| {
            o.limits.max_timeout = Duration::from_secs(86401)
        }),
        ("default-timeout", |o| {
            o.limits.default_timeout = Duration::from_secs(31)
        }),
        ("default-timeout", |o| {
            o.limits.default_timeout = Duration::from_millis(1)
        }),
        ("termination-bounds", |o| {
            o.limits.terminate_grace = Duration::ZERO
        }),
        ("termination-bounds", |o| {
            o.limits.kill_wait = Duration::from_secs(61)
        }),
        ("termination-bounds", |o| {
            o.limits.shutdown_grace = Duration::from_secs(121)
        }),
    ];
    for (code, edit) in edits {
        let mut o = options();
        edit(&mut o);
        assert_config(o, code);
    }
    let fields: [fn(&mut Limits) -> &mut usize; 7] = [
        |l| &mut l.max_running,
        |l| &mut l.max_tracked,
        |l| &mut l.max_command_bytes,
        |l| &mut l.max_working_directory_bytes,
        |l| &mut l.max_output_bytes_per_stream,
        |l| &mut l.max_environment_entries,
        |l| &mut l.max_environment_bytes,
    ];
    for (field, cap) in fields
        .into_iter()
        .zip([64, 256, 65536, 4096, 1048576, 1024, 262144])
    {
        for value in [0, cap + 1] {
            let mut o = options();
            *field(&mut o.limits) = value;
            assert_config(o, "limits");
        }
    }
}
#[test]
fn environment_debug_hides_values() {
    let mut o = options();
    o.shell_path = "/synthetic-private-shell".into();
    o.environment
        .overrides
        .insert("SECRET".into(), "synthetic-private-value".into());
    let debug = format!("{o:?} {:?}", o.environment);
    assert!(!debug.contains("synthetic-private"));
    assert!(!debug.contains("SECRET"));
}
#[test]
fn hash_ignores_environment_values_and_changes_with_each_field() {
    let original = BackgroundJobs::new(options()).unwrap().hash;
    let mut changed = options();
    changed
        .environment
        .overrides
        .insert("VALUE".into(), "synthetic".into());
    assert_eq!(BackgroundJobs::new(changed).unwrap().hash, original);
    let edits: [fn(&mut Options); 16] = [
        |o| o.limits.max_running += 1,
        |o| o.limits.max_tracked += 1,
        |o| o.limits.max_command_bytes += 1,
        |o| o.limits.max_working_directory_bytes += 1,
        |o| o.limits.max_output_bytes_per_stream += 1,
        |o| o.limits.max_environment_entries -= 1,
        |o| o.limits.max_environment_bytes -= 1,
        |o| o.limits.default_timeout = Duration::from_secs(1),
        |o| o.limits.max_timeout += Duration::from_secs(1),
        |o| o.limits.terminate_grace += Duration::from_millis(1),
        |o| o.limits.kill_wait += Duration::from_millis(1),
        |o| o.limits.shutdown_grace += Duration::from_millis(1),
        |o| o.shell_identity.push('x'),
        |o| o.environment.identity.push('x'),
        |o| o.environment.mode = EnvironmentMode::InheritAndOverride,
        |o| o.shell_path = "/bin/cat".into(),
    ];
    for edit in edits {
        let mut o = options();
        edit(&mut o);
        assert_ne!(BackgroundJobs::new(o).unwrap().hash, original);
    }
}
#[test]
fn shell_symlink_is_resolved_and_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("shell");
    std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
    let original = BackgroundJobs::new(options()).unwrap().hash;
    let mut o = options();
    o.shell_path = link;
    assert_eq!(BackgroundJobs::new(o).unwrap().hash, original);
}
#[test]
fn inputs_are_strict_bounded_and_normalized() {
    let limits = options().limits;
    let start = input::start(
        &json!({"command":" echo ok ","working_directory":"a/../b","timeout_seconds":0}),
        &limits,
    )
    .unwrap();
    assert_eq!(start.command, " echo ok ");
    assert_eq!(start.directory, std::path::Path::new("b"));
    assert_eq!(start.timeout, 0);
    for value in [
        json!(null),
        json!({}),
        json!({"command":"x","unknown":true}),
        json!({"command":1}),
    ] {
        assert!(input::start(&value, &limits).is_err());
    }
    for value in [
        json!({"command":""}),
        json!({"command":"\0"}),
        json!({"command":"x","working_directory":"../x"}),
        json!({"command":"x","working_directory":"/tmp"}),
        json!({"command":"x","timeout_seconds":-1}),
        json!({"command":"x","timeout_seconds":null}),
    ] {
        assert!(input::start(&value, &limits).is_err());
    }
    assert!(input::list(&json!({})).is_ok());
    assert!(input::list(&json!({"x":1})).is_err());
    assert!(input::id(&json!({"id":"job_bad"})).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_after_spawn_withholds_the_gate_and_recovers_the_slot() {
    if !shell_available() {
        return;
    }
    let p = policy(options());
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    p.test_hooks.lock().unwrap().after_spawn = Some(Arc::new(move |_| {
        let token = token.clone();
        Box::pin(async move {
            token.cancel();
        })
    }));
    assert!(
        p.start(context(dir.path(), cancel), json!({"command":": > canary"}))
            .await
            .unwrap_err()
            .to_string()
            .ends_with("cancelled")
    );
    timeout(Duration::from_secs(3), async {
        while p.live_jobs() != 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    sleep(Duration::from_millis(500)).await;
    assert!(!dir.path().join("canary").exists());
    p.test_hooks.lock().unwrap().after_spawn = None;
    let result = p
        .start(
            context(dir.path(), CancellationToken::new()),
            json!({"command":": > positive"}),
        )
        .await
        .unwrap();
    terminal(&p, dir.path(), result["id"].as_str().unwrap()).await;
    assert!(dir.path().join("positive").exists());
    finish(&p).await;
}

async fn dropped_start(cancel_token: bool) {
    let p = policy(options());
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    {
        let entered = entered.clone();
        let release = release.clone();
        p.test_hooks.lock().unwrap().after_spawn = Some(Arc::new(move |_| {
            let entered = entered.clone();
            let release = release.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
            })
        }));
    }
    let task = {
        let p = p.clone();
        let ctx = context(dir.path(), cancel.clone());
        tokio::spawn(async move { p.start(ctx, json!({"command":": > canary"})).await })
    };
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    if cancel_token {
        cancel.cancel();
    }
    release.notify_one();
    if cancel_token {
        timeout(Duration::from_secs(3), async {
            while p.live_jobs() != 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!dir.path().join("canary").exists());
    } else {
        let id = timeout(Duration::from_secs(3), async {
            loop {
                let list = p
                    .list(context(dir.path(), CancellationToken::new()), json!({}))
                    .unwrap();
                if let Some(id) = list["jobs"][0]["id"].as_str() {
                    return id.to_owned();
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(terminal(&p, dir.path(), &id).await["state"], "succeeded");
        assert!(dir.path().join("canary").exists());
    }
    assert_eq!(p.registry.lock().unwrap().starting, 0);
    finish(&p).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn dropped_start_future_with_a_live_token_publishes_the_job() {
    if shell_available() {
        dropped_start(false).await;
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn dropped_start_future_with_a_cancelled_token_hides_the_job() {
    if shell_available() {
        dropped_start(true).await;
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn close_during_start_terminates_the_published_job() {
    if !shell_available() {
        return;
    }
    let p = policy(options());
    let dir = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    {
        let entered = entered.clone();
        let release = release.clone();
        p.test_hooks.lock().unwrap().after_spawn = Some(Arc::new(move |_| {
            let entered = entered.clone();
            let release = release.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
            })
        }));
    }
    let start = {
        let p = p.clone();
        let ctx = context(dir.path(), CancellationToken::new());
        tokio::spawn(async move { p.start(ctx, json!({"command":": > canary"})).await })
    };
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let close = {
        let p = p.clone();
        tokio::spawn(async move { p.close(Duration::from_secs(3)).await })
    };
    timeout(Duration::from_secs(3), async {
        while !p.registry.lock().unwrap().closing {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.notify_one();
    assert!(start.await.unwrap().is_err());
    close.await.unwrap().unwrap();
    assert!(!dir.path().join("canary").exists());
    assert!(p.registry.lock().unwrap().jobs.is_empty());
    finish(&p).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_kills_share_the_attempt_and_owner_isolation_holds() {
    if !shell_available() {
        return;
    }
    let p = policy(options());
    let dir = tempfile::tempdir().unwrap();
    let result = p
        .start(
            context(dir.path(), CancellationToken::new()),
            json!({"command":"sleep 30"}),
        )
        .await
        .unwrap();
    let id = result["id"].as_str().unwrap();
    let mut other = context(dir.path(), CancellationToken::new());
    other.session_id = SessionId::from("other");
    assert!(
        p.status(other, json!({"id":id}))
            .unwrap_err()
            .to_string()
            .ends_with("job-not-found")
    );
    let (a, b) = tokio::join!(
        p.kill(
            context(dir.path(), CancellationToken::new()),
            json!({"id":id})
        ),
        p.kill(
            context(dir.path(), CancellationToken::new()),
            json!({"id":id})
        )
    );
    assert_eq!(a.unwrap()["state"], "killed");
    assert_eq!(b.unwrap()["state"], "killed");
    assert_eq!(p.live_jobs(), 0);
    finish(&p).await;
}
