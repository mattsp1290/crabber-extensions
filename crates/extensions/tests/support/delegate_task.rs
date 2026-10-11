#![allow(dead_code, unused_imports)]
use crabber::{
    Agent, AgentConfig, FakeProvider, PermissionDecision, Selection, StaticPolicy, StreamDelta,
    core::{RunId, RunStatus, SessionId, ToolCallId},
    extension::{
        Extension, ExtensionError, HostServices, Registry, Scope, ToolContext, ToolExecutor,
        WorkspaceContext,
    },
    session::{MemoryStore, SnapshotLimits, SnapshotOutcome, SnapshotRequest, Store},
};
use crabber_extensions::delegate_task::{
    DelegateTask, Limits, Options, PERMISSION_DELEGATE, Request, Response, Runner, TOOL_NAME,
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

pub fn context(cancel: CancellationToken) -> ToolContext {
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

pub fn agent_config() -> AgentConfig {
    let mut config = AgentConfig::new(Selection {
        provider_id: "fake".into(),
        model_id: "scripted".into(),
    });
    config.workspace_id = "workspace".into();
    config.directory = "/workspace".into();
    config
}

pub fn call_with(arguments: Value) -> Vec<StreamDelta> {
    tool_call(TOOL_NAME, arguments)
}
pub fn tool_call(name: &str, arguments: Value) -> Vec<StreamDelta> {
    let call_id = ToolCallId::new();
    vec![
        StreamDelta::ToolCallStart {
            call_id: call_id.clone(),
            name: name.into(),
        },
        StreamDelta::ToolCallArgsDelta {
            call_id: call_id.clone(),
            text: arguments.to_string(),
        },
        StreamDelta::ToolCallDone { call_id },
        StreamDelta::Completed,
    ]
}

pub async fn snapshot_tools(
    store: &MemoryStore,
    session: &SessionId,
) -> Vec<crabber::core::ToolCallRecord> {
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
    assert!(!format!("{page:?}").contains("HOST-PRIVATE-CANARY"));
    page.tool_calls
}

pub fn result_value(call: &crabber::core::ToolCallRecord) -> Value {
    let result = call.result.as_ref().unwrap();
    let [crabber::core::ContentBlock::Text { text }] = result.content.as_slice() else {
        panic!("unexpected result content: {:?}", result.content)
    };
    serde_json::from_str(text).unwrap()
}

pub fn done() -> Vec<StreamDelta> {
    vec![
        StreamDelta::TextDelta("done".into()),
        StreamDelta::Completed,
    ]
}

pub async fn executor(extension: DelegateTask) -> Arc<dyn ToolExecutor> {
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

pub fn agent(
    ext: Arc<DelegateTask>,
    store: Arc<MemoryStore>,
    provider: Arc<FakeProvider>,
    decision: PermissionDecision,
) -> Agent {
    Agent::builder()
        .store(store)
        .provider(provider)
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(decision)))
        .extension(ext, Scope::Global)
        .build()
        .unwrap()
}

pub async fn mounted(
    ext: Arc<DelegateTask>,
) -> (
    Registry,
    crabber::extension::MountHandle,
    std::collections::HashMap<String, Arc<dyn ToolExecutor>>,
) {
    let registry = Registry::new();
    let handle = registry.mount(ext, Scope::Global).await.unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let tools = plan
        .tools
        .iter()
        .map(|t| (t.info.name.clone(), t.executor.clone()))
        .collect();
    drop(plan);
    (registry, handle, tools)
}
pub fn start_call(arguments: Value) -> Vec<StreamDelta> {
    tool_call("delegate_task_start", arguments)
}
pub fn status_call(id: &str) -> Vec<StreamDelta> {
    tool_call("delegate_task_status", json!({"task_id":id}))
}
pub fn list_call() -> Vec<StreamDelta> {
    tool_call("delegate_task_list", json!({}))
}
pub fn wait_call(ids: &[String], timeout: Option<u64>) -> Vec<StreamDelta> {
    let mut args = json!({"task_ids":ids});
    if let Some(timeout) = timeout {
        args["timeout_seconds"] = timeout.into();
    }
    tool_call("delegate_task_wait", args)
}
pub fn read_task_id(call: &crabber::core::ToolCallRecord) -> String {
    result_value(call)["task_id"].as_str().unwrap().into()
}

pub async fn snapshot_tool(
    store: &MemoryStore,
    session: &SessionId,
) -> crabber::core::ToolCallRecord {
    let mut records = snapshot_tools(store, session).await;
    assert_eq!(records.len(), 1);
    records.remove(0)
}
