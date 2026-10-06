use super::*;

struct PollPanic;
impl Future for PollPanic {
    type Output = Result<Response, ExtensionError>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        panic!("HOST-PRIVATE-CANARY")
    }
}
struct DropPanic;
impl Future for DropPanic {
    type Output = Result<Response, ExtensionError>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}
impl Drop for DropPanic {
    fn drop(&mut self) {
        panic!("HOST-PRIVATE-CANARY")
    }
}

#[tokio::test]
async fn rejects_contextless_invalid_and_unsound_responses_without_host_data() {
    let calls = Arc::new(AtomicUsize::new(0));
    let responder: Responder = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::Selected(0)) })
        })
    };
    let ask = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder,
        limits: limits(),
    })
    .unwrap();
    let tool = executor(ask).await;
    assert_eq!(
        tool.execute(arguments()).await.unwrap_err().to_string(),
        "tool execution failed: ask user failed: context"
    );
    assert_eq!(
        tool.execute_with_context(
            context(CancellationToken::new()),
            json!({"question":"x","options":[{"label":"a"},{"label":"a"}]})
        )
        .await
        .unwrap_err()
        .to_string(),
        "tool execution failed: ask user input invalid: duplicate-option-label"
    );
    assert_eq!(
        tool.execute_with_context(context(CancellationToken::new()), arguments())
            .await
            .unwrap_err()
            .to_string(),
        "tool execution failed: ask user failed: responder"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn responder_panics_are_sanitized_and_do_not_abort_the_executor() {
    let responder: Responder = Arc::new(|_| panic!("HOST-PRIVATE-CANARY"));
    let ask = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder,
        limits: limits(),
    })
    .unwrap();
    let error = executor(ask)
        .await
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(error, "tool execution failed: ask user failed: responder");
    assert!(!error.contains("HOST-PRIVATE-CANARY"));
}

#[tokio::test]
async fn responder_errors_and_invalid_replies_have_one_sanitized_failure() {
    let canary: Responder =
        Arc::new(|_| Box::pin(async { Err(ExtensionError::Tool("HOST-PRIVATE-CANARY".into())) }));
    let mut cases: Vec<Responder> = vec![canary];
    for reply in [
        Response::Selected(0),
        Response::Selected(3),
        Response::Custom(" ".into()),
        Response::Custom("x".repeat(101)),
        Response::Custom("nul\0answer".into()),
    ] {
        cases.push(Arc::new(move |_| {
            let reply = match &reply {
                Response::Selected(value) => Response::Selected(*value),
                Response::Custom(value) => Response::Custom(value.clone()),
                Response::Dismissed => Response::Dismissed,
                Response::Unavailable => Response::Unavailable,
            };
            Box::pin(async move { Ok(reply) })
        }));
    }
    for responder in cases {
        let ask = AskUser::new(Options {
            responder_identity: "host-v1".into(),
            responder,
            limits: limits(),
        })
        .unwrap();
        let error = executor(ask)
            .await
            .execute_with_context(context(CancellationToken::new()), arguments())
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(error, "tool execution failed: ask user failed: responder");
        assert!(!error.contains("HOST-PRIVATE-CANARY"));
    }
}

#[tokio::test]
async fn responder_poll_panic_is_a_sanitized_durable_failure() {
    let responder: Responder = Arc::new(|_| Box::pin(PollPanic));
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
    let run = agent.prompt(None, "fixture").await.unwrap();
    let session = run.session_id().clone();
    run.done().await.unwrap();
    let call = snapshot_tool(&store, &session).await;
    assert_eq!(call.status, crabber::core::ToolCallStatus::Failed);
    assert_eq!(
        result_value(&call),
        json!("tool execution failed: ask user failed: responder")
    );
    assert!(!format!("{:?}", provider.requests()).contains("HOST-PRIVATE-CANARY"));
    agent.close_extensions().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn responder_drop_panic_does_not_change_the_deadline_outcome() {
    let responder: Responder = Arc::new(|_| Box::pin(DropPanic));
    let store = Arc::new(MemoryStore::new());
    let agent = Agent::builder()
        .store(store.clone())
        .provider(Arc::new(FakeProvider::scripted(vec![call(), done()])))
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
    tokio::time::advance(Duration::from_secs(5)).await;
    let session = run.session_id().clone();
    run.done().await.unwrap();
    assert_eq!(
        result_value(&snapshot_tool(&store, &session).await),
        json!({"status":"timed_out"})
    );
    agent.close_extensions().await.unwrap();
}
