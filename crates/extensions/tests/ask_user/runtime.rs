use super::*;

#[tokio::test]
async fn maps_all_host_outcomes_and_preserves_authoritative_request() {
    for (reply, expected) in [
        (
            Response::Selected(2),
            json!({"status":"selected","answer":"second","selected_option":2}),
        ),
        (
            Response::Custom("answer".into()),
            json!({"status":"custom","answer":"answer"}),
        ),
        (Response::Dismissed, json!({"status":"dismissed"})),
        (Response::Unavailable, json!({"status":"unavailable"})),
    ] {
        let result = executor(extension(reply))
            .await
            .execute_with_context(context(CancellationToken::new()), arguments())
            .await
            .unwrap();
        assert_eq!(result, expected);
    }
    let seen = Arc::new(std::sync::Mutex::new(None));
    let responder: Responder = {
        let seen = seen.clone();
        Arc::new(move |request: Request| {
            *seen.lock().unwrap() = Some(request);
            Box::pin(async { Ok(Response::Dismissed) })
        })
    };
    let ask = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder,
        limits: limits(),
    })
    .unwrap();
    executor(ask)
        .await
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    let request = seen.lock().unwrap().take().unwrap();
    assert_eq!(request.session_id(), &SessionId::from("session"));
    assert_eq!(request.run_id(), &RunId::from("run"));
    assert_eq!(request.call_id(), &ToolCallId::from("call"));
    assert_eq!(request.question(), " Pick one ");
    assert_eq!(request.options()[0].label, " first ");
    assert_eq!(request.custom_label(), CUSTOM_OPTION_LABEL);
}

#[tokio::test]
async fn runtime_policy_bypasses_host_and_allowed_results_persist_and_reenter_model() {
    let calls = Arc::new(AtomicUsize::new(0));
    let responder: Responder = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::Selected(2)) })
        })
    };
    for decision in [PermissionDecision::Deny, PermissionDecision::Ask] {
        let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
        let store = Arc::new(MemoryStore::new());
        let agent = Agent::builder()
            .store(store.clone())
            .provider(provider)
            .config(agent_config())
            .policy(Arc::new(StaticPolicy::new(decision)))
            .extension(
                Arc::new(
                    AskUser::new(Options {
                        responder_identity: "host-v1".into(),
                        responder: responder.clone(),
                        limits: limits(),
                    })
                    .unwrap(),
                ),
                Scope::Global,
            )
            .build()
            .unwrap();
        let run = agent.prompt(None, "fixture").await.unwrap();
        let session = run.session_id().clone();
        run.done().await.unwrap();
        let SnapshotOutcome::Page(page) = store
            .snapshot(SnapshotRequest {
                session_id: session,
                limits: SnapshotLimits {
                    messages: 100,
                    tool_calls: 100,
                    parts: 100,
                    text_bytes: 10_000,
                    encoded_bytes: 100_000,
                },
                continuation: None,
            })
            .await
            .unwrap()
        else {
            panic!("snapshot")
        };
        assert_eq!(format!("{:?}", page.tool_calls[0].status), "Failed");
        agent.close_extensions().await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
    let store = Arc::new(MemoryStore::new());
    let agent = Agent::builder()
        .store(store.clone())
        .provider(provider.clone())
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(
            Arc::new(
                AskUser::new(Options {
                    responder_identity: "host-v1".into(),
                    responder,
                    limits: limits(),
                })
                .unwrap(),
            ),
            Scope::Global,
        )
        .build()
        .unwrap();
    let run = agent.prompt(None, "fixture").await.unwrap();
    let session = run.session_id().clone();
    run.done().await.unwrap();
    let SnapshotOutcome::Page(page) = store
        .snapshot(SnapshotRequest {
            session_id: session,
            limits: SnapshotLimits {
                messages: 100,
                tool_calls: 100,
                parts: 100,
                text_bytes: 10_000,
                encoded_bytes: 100_000,
            },
            continuation: None,
        })
        .await
        .unwrap()
    else {
        panic!("snapshot")
    };
    assert_eq!(format!("{:?}", page.tool_calls[0].status), "Completed");
    assert!(format!("{:?}", provider.requests()[1]).contains("selected_option"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    agent.close_extensions().await.unwrap();
}

#[tokio::test]
async fn each_immediate_outcome_is_completed_durable_json_and_next_model_input() {
    for (reply, expected) in [
        (
            Response::Selected(2),
            json!({"status":"selected","answer":"second","selected_option":2}),
        ),
        (
            Response::Custom("answer".into()),
            json!({"status":"custom","answer":"answer"}),
        ),
        (Response::Dismissed, json!({"status":"dismissed"})),
        (Response::Unavailable, json!({"status":"unavailable"})),
    ] {
        let store = Arc::new(MemoryStore::new());
        let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
        let agent = Agent::builder()
            .store(store.clone())
            .provider(provider.clone())
            .config(agent_config())
            .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
            .extension(Arc::new(extension(reply)), Scope::Global)
            .build()
            .unwrap();
        let run = agent.prompt(None, "fixture").await.unwrap();
        let session = run.session_id().clone();
        assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
        let call = snapshot_tool(&store, &session).await;
        assert_eq!(call.status, crabber::core::ToolCallStatus::Completed);
        assert_eq!(
            call.result.as_ref().unwrap().status,
            crabber::core::ToolResultStatus::Completed
        );
        assert_eq!(result_value(&call), expected);
        assert!(
            format!("{:?}", provider.requests()[1]).contains(expected["status"].as_str().unwrap())
        );
        agent.close_extensions().await.unwrap();
    }
}

#[tokio::test]
async fn runtime_input_failures_do_not_invoke_the_host() {
    let calls = Arc::new(AtomicUsize::new(0));
    let responder: Responder = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::Dismissed) })
        })
    };
    for input in [
        json!({"question":"x","options":[{"label":"one"}]}),
        json!({"question":" ","options":[{"label":"one"},{"label":"two"}]}),
        json!({"question":"x".repeat(101),"options":[{"label":"one"},{"label":"two"}]}),
        json!({"question":"x","options":[{"label":"one"},{"label":"two"}],"extra":true}),
    ] {
        let store = Arc::new(MemoryStore::new());
        let agent = Agent::builder()
            .store(store.clone())
            .provider(Arc::new(FakeProvider::scripted(vec![
                call_with(input),
                done(),
            ])))
            .config(agent_config())
            .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
            .extension(
                Arc::new(
                    AskUser::new(Options {
                        responder_identity: "host-v1".into(),
                        responder: responder.clone(),
                        limits: limits(),
                    })
                    .unwrap(),
                ),
                Scope::Global,
            )
            .build()
            .unwrap();
        let run = agent.prompt(None, "fixture").await.unwrap();
        let session = run.session_id().clone();
        run.done().await.unwrap();
        assert_eq!(
            snapshot_tool(&store, &session).await.status,
            crabber::core::ToolCallStatus::Failed
        );
        agent.close_extensions().await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn runtime_deadline_is_completed_and_reaches_the_next_model_request() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let responder: Responder = {
        let entered = entered.clone();
        Arc::new(move |_| {
            entered.notify_one();
            Box::pin(async { std::future::pending::<Result<Response, ExtensionError>>().await })
        })
    };
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![call(), done()]));
    let agent = Agent::builder()
        .store(store.clone())
        .provider(provider.clone())
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(
            Arc::new(
                AskUser::new(Options {
                    responder_identity: "host-v1".into(),
                    responder,
                    limits: limits(),
                })
                .unwrap(),
            ),
            Scope::Global,
        )
        .build()
        .unwrap();
    let waiting = entered.notified();
    tokio::pin!(waiting);
    let run = agent.prompt(None, "fixture").await.unwrap();
    waiting.await;
    tokio::time::advance(Duration::from_secs(5)).await;
    let session = run.session_id().clone();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let call = snapshot_tool(&store, &session).await;
    assert_eq!(call.status, crabber::core::ToolCallStatus::Completed);
    assert_eq!(result_value(&call), json!({"status":"timed_out"}));
    assert!(format!("{:?}", provider.requests()[1]).contains("timed_out"));
    agent.close_extensions().await.unwrap();
}
