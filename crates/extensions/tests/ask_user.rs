use crabber::{
    Agent, AgentConfig, FakeProvider, PermissionDecision, Selection, StaticPolicy, StreamDelta,
    core::{RunId, SessionId, ToolCallId},
    extension::{
        Extension, ExtensionError, HostServices, Registry, Scope, ToolContext, ToolExecutor,
        WorkspaceContext,
    },
    session::{MemoryStore, SnapshotLimits, SnapshotOutcome, SnapshotRequest, Store},
};
use crabber_extensions::ask_user::{
    AskUser, CUSTOM_OPTION_LABEL, Limits, Options, PERMISSION_ASK, Request, Responder, Response,
    TOOL_NAME,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn limits() -> Limits {
    Limits {
        max_question_bytes: 100,
        max_option_label_bytes: 50,
        max_option_description_bytes: 50,
        max_custom_answer_bytes: 100,
        max_in_flight: 1,
        max_wait: Duration::from_secs(5),
    }
}
fn responder(reply: Response) -> Responder {
    Arc::new(move |_| {
        let reply = match &reply {
            Response::Selected(value) => Response::Selected(*value),
            Response::Custom(value) => Response::Custom(value.clone()),
            Response::Dismissed => Response::Dismissed,
            Response::Unavailable => Response::Unavailable,
        };
        Box::pin(async move { Ok(reply) })
    })
}
fn extension(reply: Response) -> AskUser {
    AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder: responder(reply),
        limits: limits(),
    })
    .unwrap()
}
fn arguments() -> Value {
    json!({"question":" Pick one ","options":[{"label":" first ","description":"detail"},{"label":"second"}]})
}
fn context(cancel: CancellationToken) -> ToolContext {
    ToolContext::new(
        SessionId::from("session"),
        RunId::from("run"),
        ToolCallId::from("call"),
        cancel,
        HostServices::default(),
        WorkspaceContext::from_persisted("workspace", "/workspace"),
        Arc::new(|_| {}),
        None,
    )
}
fn agent_config() -> AgentConfig {
    let mut config = AgentConfig::new(Selection {
        provider_id: "fake".into(),
        model_id: "scripted".into(),
    });
    config.workspace_id = "workspace".into();
    config.directory = "/workspace".into();
    config
}
fn call() -> Vec<StreamDelta> {
    let call_id = ToolCallId::new();
    vec![
        StreamDelta::ToolCallStart {
            call_id: call_id.clone(),
            name: TOOL_NAME.into(),
        },
        StreamDelta::ToolCallArgsDelta {
            call_id: call_id.clone(),
            text: arguments().to_string(),
        },
        StreamDelta::ToolCallDone { call_id },
        StreamDelta::Completed,
    ]
}
fn done() -> Vec<StreamDelta> {
    vec![
        StreamDelta::TextDelta("done".into()),
        StreamDelta::Completed,
    ]
}
async fn executor(extension: AskUser) -> Arc<dyn ToolExecutor> {
    let registry = Registry::new();
    let handle = registry
        .mount(Arc::new(extension), Scope::Global)
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let executor = plan.tools[0].executor.clone();
    drop(plan);
    handle.close().await.unwrap();
    executor
}

#[test]
fn validates_limits_identity_and_hashes_only_policy() {
    let callback: Responder = Arc::new(|_| Box::pin(async { Ok(Response::Dismissed) }));
    let first = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder: callback.clone(),
        limits: limits(),
    })
    .unwrap();
    let mut changed = limits();
    changed.max_wait = Duration::from_secs(4);
    let second = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder: callback.clone(),
        limits: changed,
    })
    .unwrap();
    assert_ne!(first.config_hash(), second.config_hash());
    for bad_identity in ["", " ", "x\n", &"x".repeat(257)] {
        assert!(
            AskUser::new(Options {
                responder_identity: bad_identity.into(),
                responder: callback.clone(),
                limits: limits()
            })
            .is_err()
        );
    }
    let mut bad = limits();
    bad.max_in_flight = 0;
    assert!(
        AskUser::new(Options {
            responder_identity: "host-v1".into(),
            responder: callback,
            limits: bad
        })
        .is_err()
    );
}

#[test]
fn rejects_each_limit_at_zero_and_above_its_cap() {
    let callback: Responder = Arc::new(|_| Box::pin(async { Ok(Response::Dismissed) }));
    let invalid = [
        (0, 50, 50, 100, 1, Duration::from_secs(1)),
        (100, 0, 50, 100, 1, Duration::from_secs(1)),
        (100, 50, 0, 100, 1, Duration::from_secs(1)),
        (100, 50, 50, 0, 1, Duration::from_secs(1)),
        (100, 50, 50, 100, 0, Duration::from_secs(1)),
        (100, 50, 50, 100, 1, Duration::ZERO),
        (16 * 1024 + 1, 50, 50, 100, 1, Duration::from_secs(1)),
        (100, 1025, 50, 100, 1, Duration::from_secs(1)),
        (100, 50, 4 * 1024 + 1, 100, 1, Duration::from_secs(1)),
        (100, 50, 50, 16 * 1024 + 1, 1, Duration::from_secs(1)),
        (100, 50, 50, 100, 257, Duration::from_secs(1)),
        (100, 50, 50, 100, 1, Duration::from_secs(601)),
    ];
    for (question, label, description, custom, in_flight, wait) in invalid {
        assert!(
            AskUser::new(Options {
                responder_identity: "host-v1".into(),
                responder: callback.clone(),
                limits: Limits {
                    max_question_bytes: question,
                    max_option_label_bytes: label,
                    max_option_description_bytes: description,
                    max_custom_answer_bytes: custom,
                    max_in_flight: in_flight,
                    max_wait: wait
                },
            })
            .is_err()
        );
    }
}

#[tokio::test]
async fn registered_tool_has_contract_metadata() {
    let registry = Registry::new();
    let handle = registry
        .mount(Arc::new(extension(Response::Dismissed)), Scope::Global)
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let info = &plan.tools[0].info;
    assert_eq!(info.name, TOOL_NAME);
    assert!(!info.retry_safe);
    assert_eq!(info.required_permissions, [PERMISSION_ASK]);
    assert_eq!(info.parameters["required"], json!(["question", "options"]));
    assert_eq!(info.parameters["additionalProperties"], false);
    assert_eq!(info.parameters["properties"]["options"]["minItems"], 2);
    assert_eq!(info.parameters["properties"]["options"]["maxItems"], 5);
    drop(plan);
    handle.close().await.unwrap();
}

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

#[tokio::test(start_paused = true)]
async fn cancellation_deadline_and_capacity_drop_host_work() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    struct DropFlag(Arc<AtomicUsize>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let responder: Responder = {
        let calls = calls.clone();
        let dropped = dropped.clone();
        let entered = entered.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            entered.notify_one();
            let flag = DropFlag(dropped.clone());
            Box::pin(async move {
                let _flag = flag;
                std::future::pending::<Result<Response, ExtensionError>>().await
            })
        })
    };
    let ask = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder,
        limits: limits(),
    })
    .unwrap();
    let tool = executor(ask).await;
    let cancel = CancellationToken::new();
    let first_entered = entered.notified();
    tokio::pin!(first_entered);
    let first_tool = tool.clone();
    let child_cancel = cancel.clone();
    let first = tokio::spawn(async move {
        first_tool
            .execute_with_context(context(child_cancel), arguments())
            .await
    });
    first_entered.await;
    assert_eq!(
        tool.execute_with_context(context(CancellationToken::new()), arguments())
            .await
            .unwrap(),
        json!({"status":"unavailable"})
    );
    cancel.cancel();
    assert_eq!(
        first.await.unwrap().unwrap_err().to_string(),
        "tool execution failed: ask user failed: cancelled"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    let entered_again = entered.notified();
    tokio::pin!(entered_again);
    let timed_tool = tool.clone();
    let timed = tokio::spawn(async move {
        timed_tool
            .execute_with_context(context(CancellationToken::new()), arguments())
            .await
    });
    entered_again.await;
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(timed.await.unwrap().unwrap(), json!({"status":"timed_out"}));
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
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
