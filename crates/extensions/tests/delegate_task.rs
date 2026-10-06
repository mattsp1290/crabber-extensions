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

fn limits() -> Limits {
    Limits {
        max_task_bytes: 100,
        max_profile_bytes: 50,
        max_result_bytes: 100,
        max_in_flight: 1,
        max_wait: Duration::from_secs(5),
        shutdown_grace: Duration::from_secs(2),
    }
}
fn runner(reply: Response) -> Runner {
    Arc::new(move |_| {
        let reply = match &reply {
            Response::Completed(v) => Response::Completed(v.clone()),
            Response::Failed(v) => Response::Failed(v.clone()),
            Response::Rejected(v) => Response::Rejected(v.clone()),
            Response::Unavailable(v) => Response::Unavailable(v.clone()),
        };
        Box::pin(async move { Ok(reply) })
    })
}
fn configured(runner: Runner, limits: Limits) -> DelegateTask {
    DelegateTask::new(Options {
        runner_identity: "host-v1".into(),
        runner,
        limits,
    })
    .unwrap()
}
fn extension(reply: Response) -> DelegateTask {
    configured(runner(reply), limits())
}
fn arguments() -> Value {
    json!({"task":" Inspect the synthetic project. ","profile":"read-only"})
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
    assert!(!format!("{page:?}").contains("HOST-PRIVATE-CANARY"));
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
async fn executor(extension: DelegateTask) -> Arc<dyn ToolExecutor> {
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

fn agent(
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

struct DropFlag(Arc<AtomicUsize>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct PendingHost {
    runner: Runner,
    counts: tokio::sync::watch::Receiver<(usize, usize)>,
    seen: Arc<std::sync::Mutex<Vec<Request>>>,
    dropped: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
}
impl PendingHost {
    fn new(cooperative: bool) -> Self {
        let (counts, rx) = tokio::sync::watch::channel((0, 0));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let runner: Runner = {
            let seen = seen.clone();
            let dropped = dropped.clone();
            let release = release.clone();
            Arc::new(move |request| {
                counts.send_modify(|c| c.0 += 1);
                let token = request.cancellation().clone();
                seen.lock().unwrap().push(request);
                let counts = counts.clone();
                let release = release.clone();
                let flag = DropFlag(dropped.clone());
                Box::pin(async move {
                    let _flag = flag;
                    if cooperative {
                        token.cancelled().await;
                    } else {
                        release.notified().await;
                    }
                    counts.send_modify(|c| c.1 += 1);
                    Ok(Response::Completed("released".into()))
                })
            })
        };
        Self {
            runner,
            counts: rx,
            seen,
            dropped,
            release,
        }
    }
    async fn entered(&mut self, count: usize) {
        self.wait(count, false).await;
    }
    async fn exited(&mut self, count: usize) {
        self.wait(count, true).await;
    }
    async fn wait(&mut self, count: usize, exit: bool) {
        loop {
            let value = *self.counts.borrow_and_update();
            if (if exit { value.1 } else { value.0 }) >= count {
                return;
            }
            self.counts.changed().await.unwrap();
        }
    }
}
fn spawn_call(
    tool: Arc<dyn ToolExecutor>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<Value, ExtensionError>> {
    tokio::spawn(async move {
        tool.execute_with_context(context(cancel), arguments())
            .await
    })
}

#[path = "delegate_task/contract.rs"]
mod contract;

#[path = "delegate_task/runtime.rs"]
mod runtime;

#[path = "delegate_task/lifecycle.rs"]
mod lifecycle;

#[path = "delegate_task/child.rs"]
mod child;

#[path = "delegate_task/routing.rs"]
mod routing;
