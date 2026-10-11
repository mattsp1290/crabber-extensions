use super::*;
use crabber_extensions::delegate_task::{LIST_TOOL, START_TOOL, STATUS_TOOL, WAIT_TOOL};

#[tokio::test]
async fn start_then_wait_returns_durable_output_and_reenters_the_model() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner: Runner = {
        let seen = seen.clone();
        Arc::new(move |request| {
            seen.lock().unwrap().push(request);
            Box::pin(async { Ok(Response::Completed("synthetic result".into())) })
        })
    };
    let ext = Arc::new(configured(runner, limits()));
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![
        start_call(arguments()),
        done(),
    ]));
    let first = agent(
        ext.clone(),
        store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = first.prompt(None, "start").await.unwrap();

    let session = run.done().await.unwrap().session_id;
    let call = snapshot_tool(&store, &session).await;
    let id = read_task_id(&call);
    assert_eq!(id.len(), 54);
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.background());
    assert_eq!(request.session_id(), &session);
    assert_eq!(request.call_id(), &call.id);
    assert_eq!(request.max_wait(), limits().max_background_duration);
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), Some(2)),
        done(),
    ]));
    let second = agent(
        ext,
        store.clone(),
        provider.clone(),
        PermissionDecision::Allow,
    );
    let run = second.prompt(Some(session.clone()), "wait").await.unwrap();

    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let records = snapshot_tools(&store, &session).await;
    let wait = records.iter().find(|r| r.name == WAIT_TOOL).unwrap();
    assert_eq!(
        result_value(wait),
        json!({"status":"done","tasks":[{"task_id":id,"status":"completed","output":"synthetic result"}]})
    );
    assert!(format!("{:?}", provider.requests()[1]).contains("synthetic result"));
    assert_eq!(request.run_id(), &call.run_id);
    assert_eq!(request.workspace().workspace_id(), Some("workspace"));
    assert_eq!(request.workspace().directory(), Some("/workspace"));
    first.close_extensions().await.unwrap();
    second.close_extensions().await.unwrap();
}

#[tokio::test]
async fn wait_input_validation() {
    let (_, handle, tools) = mounted(Arc::new(extension(Response::Completed(String::new())))).await;
    let id = "task_00000000000000000000000000000000_0000000000000001";
    for args in [
        json!({"task_ids":[]}),
        json!({"task_ids":[id,id]}),
        json!({"task_ids":[id,id,id,id,id]}),
        json!({"task_ids":[id],"timeout_seconds":null}),
        json!({"task_ids":[id],"timeout_seconds":0}),
        json!({"task_ids":[id],"timeout_seconds":6}),
        json!({"task_ids":["bad"]}),
        json!({"task_ids":[id],"unknown":1}),
        json!({"task_ids":[id]}),
    ] {
        assert!(
            tools[WAIT_TOOL]
                .execute_with_context(context(CancellationToken::new()), args)
                .await
                .unwrap_err()
                .to_string()
                .contains("input invalid")
        );
    }
    handle.close().await.unwrap();
}

#[tokio::test]
async fn status_and_list_report_running_then_terminal() {
    let mut host = PendingHost::new(false);
    let mut j = Journey::new(host.runner.clone(), limits());
    let id = j.start().await;
    host.entered(1).await;
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["status"],
        "running"
    );
    assert_eq!(
        result_value(&j.call(LIST_TOOL, json!({})).await)["tasks"],
        json!([{"task_id":id,"status":"running"}])
    );
    host.release.notify_one();
    j.wait(std::slice::from_ref(&id)).await;
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["output"],
        "released"
    );
    assert_eq!(
        result_value(&j.call(LIST_TOOL, json!({})).await)["tasks"],
        json!([{"task_id":id,"status":"completed"}])
    );
}

// Every operation in this harness runs through Crabber and durable settlement.
pub(super) struct Journey {
    pub(super) ext: Arc<DelegateTask>,
    pub(super) store: Arc<MemoryStore>,
    pub(super) session: Option<SessionId>,
}
impl Journey {
    pub(super) fn new(runner: Runner, limits: Limits) -> Self {
        Self {
            ext: Arc::new(configured(runner, limits)),
            store: Arc::new(MemoryStore::new()),
            session: None,
        }
    }
    pub(super) async fn call(&mut self, name: &str, args: Value) -> crabber::core::ToolCallRecord {
        let provider = Arc::new(FakeProvider::scripted(vec![tool_call(name, args), done()]));
        let a = agent(
            self.ext.clone(),
            self.store.clone(),
            provider.clone(),
            PermissionDecision::Allow,
        );
        let run = a
            .prompt(self.session.clone(), "synthetic journey")
            .await
            .unwrap();
        self.session = Some(run.session_id().clone());
        assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
        let records = snapshot_tools(&self.store, self.session.as_ref().unwrap()).await;
        let record = records.last().unwrap().clone();
        let text = format!("{:?}", record.result.as_ref().unwrap().content);
        assert!(!text.contains("HOST-PRIVATE-CANARY"));
        assert!(!format!("{:?}", provider.requests()).contains("HOST-PRIVATE-CANARY"));
        record
    }
    pub(super) async fn start(&mut self) -> String {
        read_task_id(&self.call(START_TOOL, arguments()).await)
    }
    pub(super) async fn wait(&mut self, ids: &[String]) -> Value {
        result_value(&self.call(WAIT_TOOL, json!({"task_ids":ids})).await)
    }
}

#[tokio::test]
async fn wait_returns_done_immediately_when_all_terminal() {
    let count = Arc::new(AtomicUsize::new(0));
    let host: Runner = Arc::new(move |_| {
        let first = count.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            Ok(if first {
                Response::Completed("ok".into())
            } else {
                Response::Failed("failed".into())
            })
        })
    });
    let mut j = Journey::new(host, limits());
    let a = j.start().await;
    j.wait(std::slice::from_ref(&a)).await;
    let b = j.start().await;
    let reply = j.wait(&[a, b]).await;
    assert_eq!(reply["status"], "done");
    assert_eq!(reply["tasks"][0]["status"], "completed");
    assert_eq!(reply["tasks"][1]["status"], "failed");
}

#[tokio::test]
async fn wait_output_budget_omits_later_outputs() {
    let count = Arc::new(AtomicUsize::new(0));
    let host: Runner = Arc::new(move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::Completed(if n == 3 {
                String::new()
            } else {
                "x".repeat(200)
            }))
        })
    });
    let mut j = Journey::new(
        host,
        Limits {
            max_result_bytes: 256,
            ..limits()
        },
    );
    let mut ids = Vec::new();
    for _ in 0..4 {
        let id = j.start().await;
        j.wait(std::slice::from_ref(&id)).await;
        ids.push(id);
    }
    let reply = j.wait(&ids).await;
    assert_eq!(reply["tasks"][0]["output"], "x".repeat(200));
    for i in [1, 2] {
        assert_eq!(reply["tasks"][i]["output_omitted"], true);
        assert!(reply["tasks"][i].get("output").is_none());
        assert_eq!(
            result_value(&j.call(STATUS_TOOL, json!({"task_id":ids[i]})).await)["output"],
            "x".repeat(200)
        );
    }
    assert!(reply["tasks"][3].get("output_omitted").is_none());
}

#[tokio::test]
async fn background_output_at_byte_limit_survives_escaping() {
    let mut j = Journey::new(runner(Response::Completed("\"".repeat(100))), limits());
    let id = j.start().await;
    assert_eq!(j.wait(&[id]).await["tasks"][0]["output"], "\"".repeat(100));
}

#[tokio::test]
async fn permit_is_free_when_terminal_status_is_visible() {
    let mut j = Journey::new(runner(Response::Completed("ok".into())), limits());
    let id = j.start().await;
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(&[id], None),
        start_call(arguments()),
        done(),
    ]));
    let a = agent(
        j.ext.clone(),
        j.store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    a.prompt(j.session.clone(), "wait then start")
        .await
        .unwrap()
        .done()
        .await
        .unwrap();
    let records = snapshot_tools(&j.store, j.session.as_ref().unwrap()).await;
    assert_eq!(result_value(records.last().unwrap())["status"], "started");
}

#[tokio::test]
async fn runner_faults_become_error_status_and_release_capacity() {
    for mode in 0..5 {
        let host: Runner = Arc::new(move |_| match mode {
            0 => Box::pin(async { Err(ExtensionError::Tool("HOST-PRIVATE-CANARY".into())) }),
            1 => panic!("HOST-PRIVATE-CANARY"),
            2 => Box::pin(PollPanic),
            3 => Box::pin(async { Ok(Response::Completed("x".repeat(101))) }),
            _ => Box::pin(async { Ok(Response::Completed("HOST-PRIVATE-CANARY\0".into())) }),
        });
        let mut j = Journey::new(host, limits());
        let id = j.start().await;
        let reply = j.wait(&[id]).await;
        assert_eq!(reply["tasks"][0]["status"], "error");
        assert!(reply["tasks"][0].get("output").is_none());
        assert_eq!(j.start().await.len(), 54);
    }
}

#[tokio::test]
async fn policy_deny_and_ask_do_not_touch_runner_or_registry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let host: Runner = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::Completed(String::new())) })
        })
    };
    let ext = Arc::new(configured(host, limits()));
    for decision in [PermissionDecision::Deny, PermissionDecision::Ask] {
        let id = "task_00000000000000000000000000000000_0000000000000001";
        for (name, args) in [
            (START_TOOL, arguments()),
            (STATUS_TOOL, json!({"task_id":id})),
            (LIST_TOOL, json!({})),
            (WAIT_TOOL, json!({"task_ids":[id]})),
        ] {
            let store = Arc::new(MemoryStore::new());
            let provider = Arc::new(FakeProvider::scripted(vec![tool_call(name, args), done()]));
            let a = agent(ext.clone(), store.clone(), provider, decision);
            let run = a.prompt(None, "denied").await.unwrap();
            let session = run.session_id().clone();
            run.done().await.unwrap();
            assert_eq!(
                snapshot_tool(&store, &session).await.status,
                crabber::core::ToolCallStatus::Failed
            );
            let provider = Arc::new(FakeProvider::scripted(vec![list_call(), done()]));
            let a = agent(
                ext.clone(),
                store.clone(),
                provider,
                PermissionDecision::Allow,
            );
            a.prompt(Some(session.clone()), "list")
                .await
                .unwrap()
                .done()
                .await
                .unwrap();
            assert_eq!(
                result_value(snapshot_tools(&store, &session).await.last().unwrap()),
                json!({"tasks":[]})
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct PollPanic;
impl Future for PollPanic {
    type Output = Result<Response, ExtensionError>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        panic!("HOST-PRIVATE-CANARY")
    }
}

#[tokio::test]
async fn missing_workspace_id_is_an_owner_failure() {
    let (_, handle, tools) = mounted(Arc::new(extension(Response::Completed(String::new())))).await;
    let id = "task_00000000000000000000000000000000_0000000000000001";
    for (name, args) in [
        (START_TOOL, arguments()),
        (STATUS_TOOL, json!({"task_id":id})),
        (LIST_TOOL, json!({})),
        (WAIT_TOOL, json!({"task_ids":[id]})),
    ] {
        let context = crabber::extension::ToolContext::new(
            SessionId::from("session"),
            RunId::from("run"),
            ToolCallId::from("call"),
            CancellationToken::new(),
            crabber::extension::HostServices::default(),
            crabber::extension::WorkspaceContext::from_persisted("", ""),
            Arc::new(|_| {}),
            None,
        );
        assert_eq!(
            tools[name]
                .execute_with_context(context, args)
                .await
                .unwrap_err()
                .to_string(),
            "tool execution failed: delegate task failed: owner"
        );
    }
    handle.close().await.unwrap();
}

#[tokio::test]
async fn other_session_and_other_workspace_cannot_see_tasks() {
    let mut j = Journey::new(runner(Response::Completed("ok".into())), limits());
    let id = j.start().await;
    j.wait(std::slice::from_ref(&id)).await;
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![
        status_call(&id),
        wait_call(std::slice::from_ref(&id), None),
        list_call(),
        done(),
    ]));
    let other = agent(
        j.ext.clone(),
        store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = other.prompt(None, "foreign session").await.unwrap();
    let session = run.session_id().clone();
    run.done().await.unwrap();
    let records = snapshot_tools(&store, &session).await;
    assert_eq!(records[0].status, crabber::core::ToolCallStatus::Failed);
    assert_eq!(records[1].status, crabber::core::ToolCallStatus::Failed);
    assert_eq!(result_value(&records[2]), json!({"tasks":[]}));
    // Crabber fixes the workspace at session admission. Its public context API
    // additionally proves the same session cannot read through another workspace.
    let (_, handle, tools) = mounted(j.ext.clone()).await;
    for (name, args) in [
        (STATUS_TOOL, json!({"task_id":id})),
        (WAIT_TOOL, json!({"task_ids":[id]})),
        (LIST_TOOL, json!({})),
    ] {
        let context = crabber::extension::ToolContext::new(
            j.session.clone().unwrap(),
            RunId::from("run"),
            ToolCallId::from("call"),
            CancellationToken::new(),
            crabber::extension::HostServices::default(),
            crabber::extension::WorkspaceContext::from_persisted("other", "/workspace"),
            Arc::new(|_| {}),
            None,
        );
        let reply = tools[name].execute_with_context(context, args).await;
        if name == LIST_TOOL {
            assert_eq!(reply.unwrap(), json!({"tasks":[]}));
        } else {
            assert!(
                reply
                    .unwrap_err()
                    .to_string()
                    .contains("input invalid: task_id")
            );
        }
    }
    handle.close().await.unwrap();
}

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

#[tokio::test]
async fn start_input_validation_matches_inline() {
    let mut j = Journey::new(runner(Response::Completed(String::new())), limits());
    for args in [
        json!({"task":"","profile":"x"}),
        json!({"task":"x".repeat(101),"profile":"x"}),
        json!({"task":"x\0","profile":"x"}),
        json!({"task":"x","profile":""}),
        json!({"task":"x","profile":"bad space"}),
        json!({"task":"x","profile":"x".repeat(51)}),
        json!({"task":"x","profile":"x","extra":0}),
    ] {
        let record = j.call(START_TOOL, args).await;
        assert_eq!(record.status, crabber::core::ToolCallStatus::Failed);
    }
    for args in [
        json!({"task":"x".repeat(100),"profile":"x".repeat(50)}),
        json!({"task":"é".repeat(50),"profile":"a._:/-9"}),
    ] {
        let id = read_task_id(&j.call(START_TOOL, args).await);
        j.wait(&[id]).await;
    }
}

#[tokio::test(start_paused = true)]
async fn wait_at_deadline_tie_reports_done() {
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
    let make_context = || {
        crabber::extension::ToolContext::new(
            j.session.clone().unwrap(),
            RunId::from("run"),
            ToolCallId::from("call"),
            CancellationToken::new(),
            crabber::extension::HostServices::default(),
            crabber::extension::WorkspaceContext::from_persisted("workspace", "/workspace"),
            Arc::new(|_| {}),
            None,
        )
    };
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

#[tokio::test]
async fn redactor_composition_protects_wait_output() {
    use crabber_extensions::tool_result_redactor::{self, Pattern, ToolResultRedactor};
    let mut j = Journey::new(
        runner(Response::Completed("SYNTHETIC_SECRET".into())),
        limits(),
    );
    let id = j.start().await;
    let redactor = Arc::new(
        ToolResultRedactor::new(tool_result_redactor::Options {
            order: 0,
            excluded_tools: vec![],
            additional_patterns: vec![Pattern {
                id: "synthetic".into(),
                expression: "SYNTHETIC_SECRET".into(),
            }],
            limits: tool_result_redactor::Limits {
                max_field_bytes: 4096,
                max_total_bytes: 16384,
                max_depth: 16,
                max_nodes: 100,
                max_matches_per_field: 16,
                max_patterns: 4,
                max_pattern_bytes: 256,
                max_in_flight: 4,
            },
        })
        .unwrap(),
    );
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), None),
        status_call(&id),
        done(),
    ]));
    let a = Agent::builder()
        .store(j.store.clone())
        .provider(provider.clone())
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(j.ext.clone(), Scope::Global)
        .extension(redactor, Scope::Global)
        .build()
        .unwrap();
    a.prompt(j.session.clone(), "collect")
        .await
        .unwrap()
        .done()
        .await
        .unwrap();
    let records = snapshot_tools(&j.store, j.session.as_ref().unwrap()).await;
    for record in &records[1..] {
        let text = format!("{:?}", record.result);
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("SYNTHETIC_SECRET"));
    }
    assert!(!format!("{:?}", provider.requests()).contains("SYNTHETIC_SECRET"));
    assert!(format!("{:?}", provider.requests()).contains("[REDACTED]"));
    a.close_extensions().await.unwrap();
}
