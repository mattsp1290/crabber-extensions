use super::background::Journey;
use super::*;
use crabber_extensions::delegate_task::{LIST_TOOL, START_TOOL, STATUS_TOOL};

#[tokio::test(start_paused = true)]
async fn deadline_marks_timed_out_cancels_token_and_ignores_late_reply() {
    for cooperative in [true, false] {
        let mut host = PendingHost::new(cooperative);
        let mut j = Journey::new(
            host.runner.clone(),
            Limits {
                max_background_duration: Duration::from_secs(1),
                ..limits()
            },
        );
        let id = j.start().await;
        host.entered(1).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let value = result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await);
        assert_eq!(value["status"], "timed_out");
        assert!(host.seen.lock().unwrap()[0].cancellation().is_cancelled());
        if !cooperative {
            assert_eq!(
                result_value(&j.call(START_TOOL, arguments()).await)["status"],
                "unavailable"
            );
            host.release.notify_one();
        }
        host.exited(1).await;
        assert_eq!(j.wait(&[id]).await["tasks"][0]["status"], "timed_out");
        assert_eq!(
            j.start().await.len(),
            crabber_extensions::delegate_task::TASK_ID_BYTES
        );
    }
}

#[tokio::test]
async fn background_and_inline_share_capacity_until_exit() {
    let mut host = PendingHost::new(false);
    let mut j = Journey::new(host.runner.clone(), limits());
    let id = j.start().await;
    host.entered(1).await;
    assert_eq!(
        result_value(&j.call(TOOL_NAME, arguments()).await)["status"],
        "unavailable"
    );
    assert_eq!(
        result_value(&j.call(START_TOOL, arguments()).await)["status"],
        "unavailable"
    );
    host.release.notify_one();
    j.wait(&[id]).await;
    let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
    let a = agent(
        j.ext.clone(),
        j.store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = a.prompt(None, "inline").await.unwrap();
    host.entered(2).await;
    assert_eq!(
        result_value(&j.call(START_TOOL, arguments()).await)["status"],
        "unavailable"
    );
    run.interrupt();
    run.done().await.unwrap();
    host.release.notify_one();
    host.exited(2).await;
    tokio::task::yield_now().await;
    assert_eq!(
        j.start().await.len(),
        crabber_extensions::delegate_task::TASK_ID_BYTES
    );
    host.release.notify_one();
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_running_tasks_and_joins_within_grace() {
    let mut cooperative = PendingHost::new(true);
    let mut detached = PendingHost::new(false);
    let runner = PendingHost::first_then(cooperative.runner.clone(), 1, detached.runner.clone());
    let ext = Arc::new(configured(
        runner,
        Limits {
            max_in_flight: 2,
            ..limits()
        },
    ));
    let (_, handle, tools) = mounted(ext.clone()).await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            tools[START_TOOL]
                .execute_with_context(context(CancellationToken::new()), arguments())
                .await
                .unwrap()["task_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    cooperative.entered(1).await;
    detached.entered(1).await;
    let before = tokio::time::Instant::now();
    handle.close().await.unwrap();
    assert!(before.elapsed() <= Duration::from_secs(2));
    cooperative.exited(1).await;
    assert_eq!(detached.dropped.load(Ordering::SeqCst), 0);
    assert_eq!(ext.live_runners(), 1);
    for id in ids {
        assert_eq!(
            tools[STATUS_TOOL]
                .execute_with_context(context(CancellationToken::new()), json!({"task_id":id}))
                .await
                .unwrap()["status"],
            "cancelled"
        );
    }
    detached.release.notify_one();
    detached.exited(1).await;
}

#[tokio::test]
async fn start_after_close_is_unavailable_and_reads_still_work() {
    let mut j = Journey::new(runner(Response::Completed("ok".into())), limits());
    let id = j.start().await;
    j.wait(std::slice::from_ref(&id)).await;
    let (_, handle, _) = mounted(j.ext.clone()).await;
    handle.close().await.unwrap();
    assert_eq!(
        result_value(&j.call(START_TOOL, arguments()).await)["status"],
        "unavailable"
    );
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["status"],
        "completed"
    );
    assert_eq!(
        result_value(&j.call(LIST_TOOL, json!({})).await)["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result_value(&j.call(TOOL_NAME, arguments()).await)["status"],
        "completed"
    );
}

#[tokio::test]
async fn shared_instance_close_cancels_tasks_from_every_mount() {
    let mut host = PendingHost::new(true);
    let mut j = Journey::new(host.runner.clone(), limits());
    let (_, handle, _) = mounted(j.ext.clone()).await;
    let id = j.start().await;
    host.entered(1).await;
    handle.close().await.unwrap();
    host.exited(1).await;
    assert_eq!(j.wait(&[id]).await["tasks"][0]["status"], "cancelled");
}

#[tokio::test]
async fn start_publication_is_atomic_with_executor_drop() {
    let mut host = PendingHost::new(true);
    let (_, handle, tools) = mounted(Arc::new(configured(host.runner.clone(), limits()))).await;
    let future =
        tools[START_TOOL].execute_with_context(context(CancellationToken::new()), arguments());
    drop(future);
    assert!(host.seen.lock().unwrap().is_empty());
    assert_eq!(
        tools[LIST_TOOL]
            .execute_with_context(context(CancellationToken::new()), json!({}))
            .await
            .unwrap(),
        json!({"tasks":[]})
    );
    let value = tools[START_TOOL]
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    assert_eq!(value["status"], "started");
    host.entered(1).await;
    assert_eq!(
        tools[LIST_TOOL]
            .execute_with_context(context(CancellationToken::new()), json!({}))
            .await
            .unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    handle.close().await.unwrap();
}

#[tokio::test]
async fn tracked_limit_prunes_own_terminal_then_other_owners() {
    let host = runner(Response::Completed("ok".into()));
    let mut a = Journey::new(
        host,
        Limits {
            max_tracked: 2,
            ..limits()
        },
    );
    let mut b = a.other_owner();
    let a1 = a.start().await;
    a.wait(std::slice::from_ref(&a1)).await;
    let b1 = b.start().await;
    b.wait(std::slice::from_ref(&b1)).await;
    let a2 = a.start().await;
    a.wait(std::slice::from_ref(&a2)).await;
    assert_eq!(
        a.call(STATUS_TOOL, json!({"task_id":a1})).await.status,
        crabber::core::ToolCallStatus::Failed
    );
    assert_eq!(
        result_value(&b.call(STATUS_TOOL, json!({"task_id":b1})).await)["status"],
        "completed"
    );
    // With no own terminal record, another owner's terminal record is pruned.
    // Switch runner behavior via a distinct instance for running admission below.
    let mut host = PendingHost::new(false);
    let runner = host.pending_after(1);
    let mut b = Journey::new(
        runner,
        Limits {
            max_tracked: 2,
            max_in_flight: 3,
            ..limits()
        },
    );
    let b1 = b.start().await;
    b.wait(std::slice::from_ref(&b1)).await;
    let mut a = b.other_owner();
    a.start().await;
    host.entered(1).await;
    a.start().await;
    host.entered(2).await;
    assert_eq!(
        b.call(STATUS_TOOL, json!({"task_id":b1})).await.status,
        crabber::core::ToolCallStatus::Failed
    );
    assert_eq!(
        result_value(&a.call(START_TOOL, arguments()).await)["status"],
        "unavailable"
    );
    assert_eq!(
        result_value(&a.call(LIST_TOOL, json!({})).await)["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    host.release.notify_waiters();
    host.exited(2).await;
}

#[tokio::test]
async fn refused_start_leaves_records_unchanged() {
    let mut host = PendingHost::new(false);
    let runner = host.pending_after(2);
    let mut a = Journey::new(
        runner,
        Limits {
            max_tracked: 2,
            ..limits()
        },
    );
    for _ in 0..2 {
        let id = a.start().await;
        a.wait(&[id]).await;
    }
    let before = result_value(&a.call(LIST_TOOL, json!({})).await);
    let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
    let b = agent(
        a.ext.clone(),
        Arc::new(MemoryStore::new()),
        provider,
        PermissionDecision::Allow,
    );
    let run = b.prompt(None, "hold inline capacity").await.unwrap();
    host.entered(1).await;
    assert_eq!(
        result_value(&a.call(START_TOOL, arguments()).await)["status"],
        "unavailable"
    );
    assert_eq!(result_value(&a.call(LIST_TOOL, json!({})).await), before);
    host.release.notify_one();
    host.exited(1).await;
    run.done().await.unwrap();
}

#[tokio::test]
async fn close_with_in_flight_wait_waits_for_interrupt() {
    use crabber::extension::ExtensionError;
    use crabber_extensions::delegate_task::WAIT_TOOL;
    let mut host = PendingHost::new(true);

    // Dynamic wait ID is supplied through a second scripted Agent below.
    let mut j = Journey::new(host.runner.clone(), limits());
    let id = j.start().await;
    host.entered(1).await;
    let session = j.session.clone().unwrap();
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), None),
        done(),
    ]));
    let a = Agent::builder()
        .store(j.store.clone())
        .provider(provider)
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(j.ext.clone(), Scope::Global)
        .extension_close_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let mut run = a.prompt(Some(session.clone()), "wait").await.unwrap();
    let mut events = run.events();
    loop {
        let e = events.recv().await.unwrap().unwrap();
        if matches!(e.kind, crabber::core::EventKind::ToolCallStarted) {
            break;
        }
    }
    assert!(matches!(
        a.close_extensions().await,
        Err(ExtensionError::MountCloseTimeout { .. })
    ));
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    run.interrupt();
    run.done().await.unwrap();
    a.close_extensions().await.unwrap();
    host.exited(1).await;
    let records = snapshot_tools(&j.store, &session).await;
    assert_eq!(
        records.iter().find(|r| r.name == WAIT_TOOL).unwrap().status,
        crabber::core::ToolCallStatus::Interrupted
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starts_and_close_never_leave_running_records() {
    let host = PendingHost::new(true);
    let ext = Arc::new(configured(
        host.runner.clone(),
        Limits {
            max_in_flight: 64,
            max_tracked: 64,
            ..limits()
        },
    ));
    let (_, handle, tools) = mounted(ext.clone()).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(33));
    let mut starts = Vec::new();
    for _ in 0..32 {
        let barrier = barrier.clone();
        let tool = tools[START_TOOL].clone();
        starts.push(tokio::spawn(async move {
            barrier.wait().await;
            tool.execute_with_context(context(CancellationToken::new()), arguments())
                .await
                .unwrap()
        }));
    }
    barrier.wait().await;
    handle.close().await.unwrap();
    for start in starts {
        let reply = start.await.unwrap();
        match reply["status"].as_str().unwrap() {
            "unavailable" => {}
            "started" => assert_eq!(
                tools[STATUS_TOOL]
                    .execute_with_context(
                        context(CancellationToken::new()),
                        json!({"task_id":reply["task_id"]})
                    )
                    .await
                    .unwrap()["status"],
                "cancelled"
            ),
            status => panic!("unexpected start status: {status}"),
        }
    }
    assert_eq!(ext.live_runners(), 0);
    let list = tools[LIST_TOOL]
        .execute_with_context(context(CancellationToken::new()), json!({}))
        .await
        .unwrap();
    assert!(
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "cancelled")
    );
}

#[tokio::test]
async fn shutdown_before_first_runner_poll_skips_host_callback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runner: Runner = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::Completed("ok".into())) })
        })
    };
    let ext = Arc::new(configured(runner, limits()));
    let (_, handle, tools) = mounted(ext.clone()).await;
    // The current-thread task cannot poll the spawned runner until we yield.
    let started = tools[START_TOOL]
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    crabber::extension::Extension::shutdown(ext.as_ref()).await;
    handle.close().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ext.live_runners(), 0);
    assert_eq!(
        tools[STATUS_TOOL]
            .execute_with_context(
                context(CancellationToken::new()),
                json!({"task_id":started["task_id"]})
            )
            .await
            .unwrap()["status"],
        "cancelled"
    );
}
