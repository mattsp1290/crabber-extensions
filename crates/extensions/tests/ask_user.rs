use crabber::{
    Agent, AgentConfig, FakeProvider, PermissionDecision, Selection, StaticPolicy, StreamDelta,
    core::{RunId, RunStatus, SessionId, ToolCallId},
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
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
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
    call_with(arguments())
}
fn call_with(arguments: Value) -> Vec<StreamDelta> {
    let call_id = ToolCallId::new();
    vec![
        StreamDelta::ToolCallStart {
            call_id: call_id.clone(),
            name: TOOL_NAME.into(),
        },
        StreamDelta::ToolCallArgsDelta {
            call_id: call_id.clone(),
            text: arguments.to_string(),
        },
        StreamDelta::ToolCallDone { call_id },
        StreamDelta::Completed,
    ]
}
async fn snapshot_tool(store: &MemoryStore, session: &SessionId) -> crabber::core::ToolCallRecord {
    let SnapshotOutcome::Page(page) = store
        .snapshot(SnapshotRequest {
            session_id: session.clone(),
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
    assert_eq!(page.tool_calls.len(), 1);
    page.tool_calls.into_iter().next().unwrap()
}
fn result_value(call: &crabber::core::ToolCallRecord) -> Value {
    let result = call.result.as_ref().unwrap();
    let [crabber::core::ContentBlock::Text { text }] = result.content.as_slice() else {
        panic!("unexpected result content: {:?}", result.content)
    };
    serde_json::from_str(text).unwrap()
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

#[path = "ask_user/contract.rs"]
mod contract;

#[path = "ask_user/runtime.rs"]
mod runtime;

#[path = "ask_user/lifecycle.rs"]
mod lifecycle;

#[path = "ask_user/faults.rs"]
mod faults;

#[path = "ask_user/routing.rs"]
mod routing;
