use super::*;

#[tokio::test(start_paused = true)]
async fn wait_times_out_with_current_statuses() {
    let mut host = PendingHost::new(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let runner: Runner = {
        let pending = host.runner.clone();
        Arc::new(move |r| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Box::pin(async { Ok(Response::Completed("ok".into())) })
            } else {
                pending(r)
            }
        })
    };
    let mut j = Journey::new(
        runner,
        Limits {
            max_in_flight: 2,
            max_background_duration: Duration::from_secs(60),
            ..limits()
        },
    );
    let a = j.start().await;
    j.wait(std::slice::from_ref(&a)).await;
    let b = j.start().await;
    host.entered(1).await;
    let before = tokio::time::Instant::now();
    let value = result_value(
        &j.call(WAIT_TOOL, json!({"task_ids":[a,b],"timeout_seconds":1}))
            .await,
    );
    assert_eq!(before.elapsed(), Duration::from_secs(1));
    assert_eq!(value["status"], "timed_out");
    assert_eq!(value["tasks"][0]["status"], "completed");
    assert_eq!(value["tasks"][1]["status"], "running");
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    host.release.notify_one();
    j.wait(&[b]).await;
}

#[tokio::test(start_paused = true)]
async fn wait_wakes_on_settlement_without_timeout() {
    for early in [false, true] {
        let mut host = PendingHost::new(false);
        let (_, handle, tools) = mounted(Arc::new(configured(
            host.runner.clone(),
            Limits {
                max_wait: Duration::from_secs(60),
                max_background_duration: Duration::from_secs(120),
                ..limits()
            },
        )))
        .await;
        let start = tools[START_TOOL]
            .execute_with_context(context(CancellationToken::new()), arguments())
            .await
            .unwrap();
        host.entered(1).await;
        let wait = tools[WAIT_TOOL].execute_with_context(
            context(CancellationToken::new()),
            json!({"task_ids":[start["task_id"]],"timeout_seconds":60}),
        );
        tokio::pin!(wait);
        let before = tokio::time::Instant::now();
        if !early {
            tokio::select! {biased; _=&mut wait=>panic!("completed too early"), _=tokio::task::yield_now()=>{}}
        }
        host.release.notify_one();
        assert_eq!(wait.await.unwrap()["status"], "done");
        assert_eq!(before.elapsed(), Duration::ZERO);
        handle.close().await.unwrap();
    }
}

#[tokio::test]
async fn parent_cancellation_during_wait_leaves_task_running() {
    let mut host = PendingHost::new(false);
    let (_, handle, tools) = mounted(Arc::new(configured(host.runner.clone(), limits()))).await;
    let start = tools[START_TOOL]
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    host.entered(1).await;
    let cancel = CancellationToken::new();
    let wait = tools[WAIT_TOOL].execute_with_context(
        context(cancel.clone()),
        json!({"task_ids":[start["task_id"]]}),
    );
    tokio::pin!(wait);
    tokio::select! {biased; _=&mut wait=>panic!("completed too early"), _=tokio::task::yield_now()=>{}}
    cancel.cancel();
    assert!(
        wait.await
            .unwrap_err()
            .to_string()
            .contains("failed: cancelled")
    );
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    assert_eq!(
        tools[STATUS_TOOL]
            .execute_with_context(
                context(CancellationToken::new()),
                json!({"task_id":start["task_id"]})
            )
            .await
            .unwrap()["status"],
        "running"
    );
    host.release.notify_one();
    host.exited(1).await;
    handle.close().await.unwrap();
}

#[tokio::test]
async fn run_interrupt_during_wait_settles_interrupted_and_task_continues() {
    let mut host = PendingHost::new(false);
    let mut j = Journey::new(host.runner.clone(), limits());
    let id = j.start().await;
    host.entered(1).await;
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), None),
        done(),
    ]));
    let a = agent(
        j.ext.clone(),
        j.store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let mut run = a.prompt(j.session.clone(), "wait").await.unwrap();
    // ToolCallStarted is emitted once execution has been admitted.
    let mut events = run.events();
    loop {
        let event = events.recv().await.unwrap().unwrap();
        if matches!(event.kind, crabber::core::EventKind::ToolCallStarted) {
            break;
        }
    }
    run.interrupt();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Interrupted);
    let records = snapshot_tools(&j.store, j.session.as_ref().unwrap()).await;
    assert_eq!(
        records.last().unwrap().status,
        crabber::core::ToolCallStatus::Interrupted
    );
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["status"],
        "running"
    );
    host.release.notify_one();
    assert_eq!(j.wait(&[id]).await["status"], "done");
}

#[tokio::test(start_paused = true)]
async fn wait_at_deadline_tie_reports_done() {
    // Already-settled rows win even after the wait deadline fires. At a true
    // scheduling tie the runner may still be running when the wait snapshots.
    let host: Runner = Arc::new(|_| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(Response::Completed("ok".into()))
        })
    });
    let (_, handle, tools) = mounted(Arc::new(configured(host, limits()))).await;
    let start = tools[START_TOOL]
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    tokio::task::yield_now().await;
    let wait = tools[WAIT_TOOL].execute_with_context(
        context(CancellationToken::new()),
        json!({"task_ids":[start["task_id"]],"timeout_seconds":1}),
    );
    tokio::pin!(wait);
    tokio::select! {biased;_=&mut wait=>panic!("early"),_=tokio::task::yield_now()=>{}}
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(wait.await.unwrap()["status"], "done");
    handle.close().await.unwrap();
}

#[tokio::test]
async fn background_token_survives_run_end_and_run_interrupt() {
    let mut host = PendingHost::new(false);
    let mut j = Journey::new(
        host.runner.clone(),
        Limits {
            max_in_flight: 2,
            ..limits()
        },
    );
    let id = j.start().await;
    host.entered(1).await;
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
    let a = agent(
        j.ext.clone(),
        j.store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = a.prompt(j.session.clone(), "inline").await.unwrap();
    host.entered(2).await;
    run.interrupt();
    run.done().await.unwrap();
    assert!(!host.seen.lock().unwrap()[0].cancellation().is_cancelled());
    assert_eq!(
        result_value(
            &j.call(WAIT_TOOL, json!({"task_ids":[id],"timeout_seconds":1}))
                .await
        )["status"],
        "timed_out"
    );
    host.release.notify_waiters();
    assert_eq!(j.wait(&[id]).await["status"], "done");
    host.exited(2).await;
}

#[tokio::test]
async fn wait_holds_records_across_eviction() {
    let mut host = PendingHost::new(false);
    let count = Arc::new(AtomicUsize::new(0));
    let runner: Runner = {
        let pending = host.runner.clone();
        Arc::new(move |r| {
            if count.fetch_add(1, Ordering::SeqCst) == 1 {
                pending(r)
            } else {
                Box::pin(async { Ok(Response::Completed("retained".into())) })
            }
        })
    };
    let mut j = Journey::new(
        runner,
        Limits {
            max_tracked: 2,
            max_in_flight: 2,
            ..limits()
        },
    );
    let first = j.start().await;
    j.wait(std::slice::from_ref(&first)).await;
    let second = j.start().await;
    host.entered(1).await;
    let (_, handle, tools) = mounted(j.ext.clone()).await;
    let make_context = || context_for(j.session.clone().unwrap(), "workspace");
    let wait =
        tools[WAIT_TOOL].execute_with_context(make_context(), json!({"task_ids":[first,second]}));
    tokio::pin!(wait);
    tokio::select! {biased;_=&mut wait=>panic!("early"),_=tokio::task::yield_now()=>{}}
    let mut other = Journey {
        ext: j.ext.clone(),
        store: Arc::new(MemoryStore::new()),
        session: None,
    };
    other.start().await;
    host.release.notify_one();
    let value = wait.await.unwrap();
    assert_eq!(value["status"], "done");
    assert_eq!(value["tasks"][0]["output"], "retained");
    assert!(
        tools[STATUS_TOOL]
            .execute_with_context(make_context(), json!({"task_id":first}))
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid: task_id")
    );
    handle.close().await.unwrap();
}
