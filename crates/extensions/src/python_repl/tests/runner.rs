use super::support::{group_gone, group_members, python};
use crate::{process::RunnerLaunch, python_repl::runner::*};
use serde_json::json;
use std::time::Duration;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

const BOUNDS: Bounds = Bounds {
    request: 100_000,
    response: 100_000,
    output: 32,
    result: 1024,
    exception: 2048,
};
const GRACE: Duration = Duration::from_millis(100);
const WAIT: Duration = Duration::from_secs(3);

async fn start(
    source: &str,
    cancel: &CancellationToken,
    close: &CancellationToken,
    timeout: Duration,
) -> Option<Result<Runner, StartFailure>> {
    let path = python()?;
    let args: Vec<String> = INTERPRETER_FLAGS
        .into_iter()
        .map(str::to_owned)
        .chain([
            "-c".into(),
            source.into(),
            RUNNER_PROTOCOL.into(),
            BOUNDS.request.to_string(),
            BOUNDS.response.to_string(),
            BOUNDS.output.to_string(),
            BOUNDS.output.to_string(),
            BOUNDS.result.to_string(),
            BOUNDS.exception.to_string(),
        ])
        .collect();
    Some(
        Runner::start(
            RunnerLaunch {
                program: &path,
                args: &args,
                directory: std::path::Path::new("/tmp"),
                environment: &[],
            },
            BOUNDS,
            cancel,
            close,
            Instant::now() + timeout,
            GRACE,
            WAIT,
        )
        .await,
    )
}

async fn real() -> Option<Runner> {
    start(
        RUNNER_SOURCE,
        &CancellationToken::new(),
        &CancellationToken::new(),
        WAIT,
    )
    .await
    .map(|r| r.unwrap_or_else(|e| panic!("runner startup: {:?}", e.fault)))
}
async fn response(runner: &mut Runner, code: &str) -> Response {
    match runner
        .execute(
            code,
            &CancellationToken::new(),
            &CancellationToken::new(),
            Instant::now() + WAIT,
        )
        .await
    {
        ExecuteOutcome::Completed(response) => response,
        other => panic!("unexpected outcome: {other:?}"),
    }
}
async fn stop(runner: &mut Runner) {
    let id = runner.child.pgid().as_raw_nonzero().get();
    assert!(runner.terminate(GRACE, WAIT).await.reaped);
    group_gone(id).await;
}

#[test]
fn runner_source_fits_argument_limits() {
    assert!(RUNNER_SOURCE.len() < 64 * 1024);
    assert_eq!(runner_digest().len(), 64);
}

#[tokio::test]
async fn frames_are_length_prefixed_and_bounded() {
    let frame = encode_frame(&json!({"a": 1}), 7).unwrap();
    assert_eq!(&frame[..4], &7u32.to_be_bytes());
    assert_eq!(
        read_frame(&mut frame.as_slice(), 7).await.unwrap(),
        br#"{"a":1}"#
    );
    assert!(encode_frame(&json!({"a":1}), 6).is_err());
    for raw in [
        vec![],
        vec![0, 0, 0],
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 8],
        vec![0, 0, 0, 1],
    ] {
        assert!(read_frame(&mut raw.as_slice(), 7).await.is_err());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn state_expression_errors_fds_and_flags() {
    let Some(mut runner) = real().await else {
        return;
    };
    assert_eq!(response(&mut runner, "x = 1").await.result.text, "");
    assert_eq!(response(&mut runner, "x + 1").await.result.text, "2");
    let error = response(&mut runner, "x / 0").await;
    assert_eq!(error.status, Status::PythonError);
    assert!(error.exception.text.contains("<python_repl>"));
    assert!(!error.exception.text.contains("<string>"));
    assert_eq!(error.result.text, "");
    assert_eq!(
        response(&mut runner, "import os; os.write(1, b'junk'); input()")
            .await
            .status,
        Status::PythonError
    );
    let flags = response(&mut runner, "import sys; (sys.flags.isolated, sys.flags.dont_write_bytecode, sys.flags.no_site, sys.prefix == sys.base_prefix, sys.executable)").await.result.text;
    assert!(flags.starts_with("(1, 1, 0, True, "));
    assert!(
        flags.contains(python().unwrap().to_str().unwrap()),
        "flags: {flags:?}, python: {:?}",
        python()
    );
    assert_eq!(
        response(&mut runner, "'REQUEST_FD' in globals()")
            .await
            .result
            .text,
        "False"
    );
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn output_utf8_prefix_across_writes_and_unencodable_text() {
    let Some(mut runner) = real().await else {
        return;
    };
    let response = response(&mut runner, "import sys; sys.stdout.write('a'*31); sys.stdout.write('é'); sys.stdout.write('z'); sys.stderr.write('\\ud800')").await;
    assert_eq!(response.stdout.text, "a".repeat(31));
    assert!(response.stdout.truncated);
    assert_eq!(response.stderr.text, "?");
    assert!(!response.stderr.truncated);
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_malformed_and_slow_readiness() {
    for (source, fault) in [
        (
            "import os,struct,json; b=json.dumps({'version':'python-repl-runner-v1','phase':'ready','python':[3,10]}).encode(); os.write(1,struct.pack('>I',len(b))+b)",
            StartFault::Bootstrap,
        ),
        ("print('junk')", StartFault::Readiness),
        ("pass", StartFault::Readiness),
        ("import time; time.sleep(60)", StartFault::Timeout),
    ] {
        let Some(result) = start(
            source,
            &CancellationToken::new(),
            &CancellationToken::new(),
            if fault == StartFault::Timeout {
                Duration::from_millis(200)
            } else {
                WAIT
            },
        )
        .await
        else {
            return;
        };
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("unexpected ready"),
        };
        assert_eq!(error.fault, fault);
        assert!(error.runner.is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn start_is_interrupted_by_close_token() {
    let close = CancellationToken::new();
    let cloned = close.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(100)).await;
        cloned.cancel();
    });
    let Some(result) = start(
        "import time; time.sleep(60)",
        &CancellationToken::new(),
        &close,
        WAIT,
    )
    .await
    else {
        return;
    };
    assert!(matches!(
        result,
        Err(StartFailure {
            fault: StartFault::Closing,
            runner: None
        })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_before_write_is_not_executed() {
    let Some(mut runner) = real().await else {
        return;
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        runner
            .execute(
                "x = 1",
                &cancel,
                &CancellationToken::new(),
                Instant::now() + WAIT
            )
            .await,
        ExecuteOutcome::Interrupted {
            may_have_executed: false
        }
    ));
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closerange_fails_and_exited_leader_stays_zombie() {
    for code in [
        "import os; os.closerange(3, 1024)",
        "import os; os._exit(3)",
    ] {
        let Some(mut runner) = real().await else {
            return;
        };
        let id = runner.child.pgid().as_raw_nonzero().get();
        assert!(matches!(
            runner
                .execute(
                    code,
                    &CancellationToken::new(),
                    &CancellationToken::new(),
                    Instant::now() + WAIT
                )
                .await,
            ExecuteOutcome::Failed {
                may_have_executed: true
            }
        ));
        tokio::time::timeout(WAIT, async {
            while !runner.child.leader_exited() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            group_members(id)
                .iter()
                .any(|(pid, state)| *pid == id && state.starts_with('Z'))
        );
        stop(&mut runner).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn forked_child_does_not_hide_runner_death() {
    let Some(mut runner) = real().await else {
        return;
    };
    let now = Instant::now();
    assert!(matches!(
        runner
            .execute(
                "import os,time\nif os.fork() == 0: time.sleep(60); os._exit(0)\nos._exit(1)",
                &CancellationToken::new(),
                &CancellationToken::new(),
                now + Duration::from_secs(30)
            )
            .await,
        ExecuteOutcome::Failed {
            may_have_executed: true
        }
    ));
    assert!(now.elapsed() < WAIT);
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn term_honoring_ends_early_and_ignoring_requires_kill() {
    for ignore in [false, true] {
        let Some(mut runner) = real().await else {
            return;
        };
        if ignore {
            assert!(matches!(runner.execute("import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)", &CancellationToken::new(), &CancellationToken::new(), Instant::now() + Duration::from_millis(200)).await, ExecuteOutcome::TimedOut { may_have_executed: true }));
        }
        let now = Instant::now();
        let grace = if ignore {
            GRACE
        } else {
            Duration::from_secs(5)
        };
        assert!(runner.terminate(grace, WAIT).await.reaped);
        if ignore {
            assert!(now.elapsed() >= GRACE);
        } else {
            assert!(now.elapsed() < Duration::from_secs(1));
        }
        group_gone(runner.child.pgid().as_raw_nonzero().get()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_terminate_keeps_term_deadline() {
    let Some(mut runner) = real().await else {
        return;
    };
    assert!(matches!(
        runner
            .execute(
                "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)",
                &CancellationToken::new(),
                &CancellationToken::new(),
                Instant::now() + Duration::from_millis(200)
            )
            .await,
        ExecuteOutcome::TimedOut {
            may_have_executed: true
        }
    ));
    let now = Instant::now();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(80),
            runner.terminate(Duration::from_millis(200), WAIT)
        )
        .await
        .is_err()
    );
    assert!(
        runner
            .terminate(Duration::from_millis(200), WAIT)
            .await
            .reaped
    );
    assert!(now.elapsed() < Duration::from_millis(270));
    group_gone(runner.child.pgid().as_raw_nonzero().get()).await;
}

const READY_SLEEP: &str = "import os,struct,json,time; b=json.dumps({'version':'python-repl-runner-v1','phase':'ready','python':[3,14]}).encode(); os.write(1,struct.pack('>I',len(b))+b); time.sleep(60)";

#[tokio::test(flavor = "multi_thread")]
async fn blocked_write_is_cancellable_and_bounded() {
    for cancellation in [false, true] {
        let cancel = CancellationToken::new();
        let close = CancellationToken::new();
        let Some(Ok(mut runner)) = start(READY_SLEEP, &cancel, &close, WAIT).await else {
            return;
        };
        if cancellation {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(100)).await;
                cancel.cancel();
            });
        }
        let outcome = runner
            .execute(
                &"x".repeat(90_000),
                &cancel,
                &close,
                Instant::now() + Duration::from_millis(200),
            )
            .await;
        if cancellation {
            assert!(matches!(
                outcome,
                ExecuteOutcome::Interrupted {
                    may_have_executed: false
                }
            ));
        } else {
            assert!(matches!(
                outcome,
                ExecuteOutcome::TimedOut {
                    may_have_executed: false
                }
            ));
        }
        stop(&mut runner).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn close_before_and_after_write() {
    for before in [false, true] {
        let Some(mut runner) = real().await else {
            return;
        };
        let close = CancellationToken::new();
        if before {
            close.cancel();
        } else {
            let close = close.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(100)).await;
                close.cancel();
            });
        }
        let outcome = runner
            .execute(
                "import time; time.sleep(60)",
                &CancellationToken::new(),
                &close,
                Instant::now() + WAIT,
            )
            .await;
        match outcome {
            ExecuteOutcome::Closing { may_have_executed } => assert_eq!(may_have_executed, !before),
            other => panic!("{other:?}"),
        }
        stop(&mut runner).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_death_and_multiprocessing_fork_are_detected() {
    for code in [
        "import os,time\nif os.fork() == 0: time.sleep(60); os._exit(0)\ntime.sleep(.1); os._exit(1)",
        "import multiprocessing,os; pool = multiprocessing.get_context('fork').Pool(1); os._exit(1)",
    ] {
        let Some(mut runner) = real().await else {
            return;
        };
        assert!(matches!(
            runner
                .execute(
                    code,
                    &CancellationToken::new(),
                    &CancellationToken::new(),
                    Instant::now() + WAIT
                )
                .await,
            ExecuteOutcome::Failed {
                may_have_executed: true
            }
        ));
        stop(&mut runner).await;
    }
    let Some(mut runner) = real().await else {
        return;
    };
    response(&mut runner, "import threading,os,time; threading.Thread(target=lambda: (time.sleep(.1), os._exit(1))).start()").await;
    sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        runner
            .execute(
                "1",
                &CancellationToken::new(),
                &CancellationToken::new(),
                Instant::now() + WAIT
            )
            .await,
        ExecuteOutcome::Failed {
            may_have_executed: false
        }
    ));
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn leader_that_leaves_group_is_killed_directly() {
    let Some(mut runner) = real().await else {
        return;
    };
    // The parent's process group need not equal its PID (PTY and CI differ).
    let code = format!(
        "import os,time; os.setpgid(0, {}); time.sleep(60)",
        rustix::process::getpgrp().as_raw_nonzero().get()
    );
    assert!(matches!(
        runner
            .execute(
                &code,
                &CancellationToken::new(),
                &CancellationToken::new(),
                Instant::now() + Duration::from_millis(200)
            )
            .await,
        ExecuteOutcome::TimedOut {
            may_have_executed: true
        }
    ));
    stop(&mut runner).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn detached_descendant_is_outside_cleanup_contract() {
    let Some(mut runner) = real().await else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("holder");
    let code = format!(
        "import os,time,pathlib\npid = os.fork()\nif pid == 0:\n os.setsid()\n pathlib.Path({}).write_text(str(os.getpid()))\n time.sleep(60)\n os._exit(0)\nwhile not pathlib.Path({}).exists(): time.sleep(.01)\npid",
        serde_json::to_string(file.to_str().unwrap()).unwrap(),
        serde_json::to_string(file.to_str().unwrap()).unwrap()
    );
    // Always recover the holder even if a later assertion panics.
    struct Holder(std::path::PathBuf);
    impl Drop for Holder {
        fn drop(&mut self) {
            if let Ok(raw) = std::fs::read_to_string(&self.0)
                && let Some(pid) = raw
                    .parse::<i32>()
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
    let guard = Holder(file.clone());
    let result = response(&mut runner, &code).await;
    let pid: i32 = result.result.text.parse().unwrap();
    stop(&mut runner).await;
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .starts_with('Z')
    );
    assert!(output.status.success());
    drop(guard);
}

#[tokio::test(flavor = "multi_thread")]
async fn responses_require_matching_identity_bounds_and_status_zeroes() {
    let base = json!({"version":RUNNER_PROTOCOL, "id":1, "status":"completed",
        "stdout":{"text":"","truncated":false}, "stderr":{"text":"","truncated":false},
        "result":{"text":"","truncated":false}, "exception":{"text":"","truncated":false}});
    let mut invalid = Vec::new();
    for (field, value) in [
        ("version", json!("wrong")),
        ("id", json!(2)),
        ("status", json!("wrong")),
    ] {
        let mut item = base.clone();
        item[field] = value;
        invalid.push(item);
    }
    for (field, length) in [
        ("stdout", BOUNDS.output),
        ("stderr", BOUNDS.output),
        ("result", BOUNDS.result),
        ("exception", BOUNDS.exception),
    ] {
        let mut item = base.clone();
        item[field]["text"] = json!("a".repeat(length + 1));
        invalid.push(item);
    }
    let mut item = base.clone();
    item["exception"]["truncated"] = json!(true);
    invalid.push(item);
    let mut item = base.clone();
    item["status"] = json!("python_error");
    item["result"]["text"] = json!("forbidden");
    invalid.push(item);
    let mut item = base.clone();
    item["unknown"] = json!(1);
    invalid.push(item);
    for item in invalid {
        let source = format!(
            "import os,struct,json,time\ndef send(b): os.write(1,struct.pack('>I',len(b))+b)\nsend(b'{{\"version\":\"python-repl-runner-v1\",\"phase\":\"ready\",\"python\":[3,14]}}')\nos.read(0,65536)\nsend({}.encode())\ntime.sleep(60)",
            serde_json::to_string(&item.to_string()).unwrap()
        );
        let Some(Ok(mut runner)) = start(
            &source,
            &CancellationToken::new(),
            &CancellationToken::new(),
            WAIT,
        )
        .await
        else {
            return;
        };
        assert!(matches!(
            runner
                .execute(
                    "1",
                    &CancellationToken::new(),
                    &CancellationToken::new(),
                    Instant::now() + WAIT
                )
                .await,
            ExecuteOutcome::Failed {
                may_have_executed: true
            }
        ));
        stop(&mut runner).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn descendant_environment_and_null_fds() {
    let Some(path) = python() else { return };
    let directory = tempfile::tempdir().unwrap();
    let private = directory.path().join("home");
    std::fs::create_dir(&private).unwrap();
    let environment = vec![("HOME".into(), private.to_str().unwrap().into())];
    let args: Vec<String> = INTERPRETER_FLAGS
        .into_iter()
        .map(str::to_owned)
        .chain([
            "-c".into(),
            RUNNER_SOURCE.into(),
            RUNNER_PROTOCOL.into(),
            BOUNDS.request.to_string(),
            BOUNDS.response.to_string(),
            BOUNDS.output.to_string(),
            BOUNDS.output.to_string(),
            BOUNDS.result.to_string(),
            BOUNDS.exception.to_string(),
        ])
        .collect();
    let mut runner = Runner::start(
        RunnerLaunch {
            program: &path,
            args: &args,
            directory: directory.path(),
            environment: &environment,
        },
        BOUNDS,
        &CancellationToken::new(),
        &CancellationToken::new(),
        Instant::now() + WAIT,
        GRACE,
        WAIT,
    )
    .await
    .unwrap_or_else(|e| panic!("{:?}", e.fault));
    let code = "import subprocess,sys; subprocess.check_output([sys.executable, '-c', 'import os; print(os.environ[\"HOME\"]); print(os.read(0, 1))']).decode()";
    let result = response(&mut runner, code).await.result.text;
    assert!(result.contains(private.to_str().unwrap()));
    assert!(result.contains("b''"));
    stop(&mut runner).await;
}
