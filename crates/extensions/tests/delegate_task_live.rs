//! Opt-in provider acceptance; default workspace tests never compile this target.
#[path = "support/delegate_task.rs"]
mod delegate_support;
use crabber::{
    Agent, AgentConfig, FakeProvider, PermissionDecision, Selection, StaticPolicy,
    core::{ContentBlock, EventKind, Role, RunStatus, Usage},
    extension::ExtensionError,
    providers::{HttpAdapter, HttpResolver, Protocol},
    session::{MemoryStore, Store},
};
use crabber_extensions::delegate_task::{
    DelegateTask, Limits, Options, Response, Runner, START_TOOL, STATUS_TOOL,
};
use delegate_support::*;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

static LIVE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[derive(Clone)]
struct LiveConfig {
    model: String,
    protocol: Protocol,
}
fn live_config() -> Option<LiveConfig> {
    if !std::env::var_os("OPENCODE_GO_API_KEY").is_some_and(|v| !v.is_empty()) {
        assert_ne!(
            std::env::var("DELEGATE_TASK_REQUIRE_OPENCODE_GO").as_deref(),
            Ok("1"),
            "OPENCODE_GO_API_KEY is required"
        );
        eprintln!("skipping live OpenCode Go suite: OPENCODE_GO_API_KEY is not set");
        return None;
    }
    let model = std::env::var("OPENCODE_GO_TEST_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into());
    let protocol = match std::env::var("OPENCODE_GO_TEST_PROTOCOL")
        .as_deref()
        .unwrap_or("responses")
    {
        "responses" => Protocol::Responses,
        "messages" => Protocol::Messages,
        "chat-completions" => Protocol::ChatCompletions,
        _ => panic!("invalid OPENCODE_GO_TEST_PROTOCOL"),
    };
    Some(LiveConfig { model, protocol })
}
fn limits(grace: u64) -> Limits {
    Limits {
        max_task_bytes: 4096,
        max_profile_bytes: 50,
        max_result_bytes: 4096,
        max_in_flight: 1,
        max_wait: Duration::from_secs(180),
        shutdown_grace: Duration::from_secs(grace),
        max_tracked: 4,
        max_background_duration: Duration::from_secs(180),
        max_wait_ids: 4,
        max_wait_reply_bytes: 4096,
    }
}
fn live_child(config: &LiveConfig, store: Arc<MemoryStore>) -> Agent {
    Agent::builder()
        .store(store)
        .provider(Arc::new(
            HttpResolver::new().with_adapter(HttpAdapter::opencode_go(config.protocol)),
        ))
        .config(AgentConfig::new(Selection {
            provider_id: "opencode-go".into(),
            model_id: config.model.clone(),
        }))
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .build()
        .unwrap()
}
#[derive(Default)]
struct Observation {
    status: Option<RunStatus>,
    usage: Usage,
    events: Vec<String>,
    cancelled: bool,
}
struct DropFlag(Arc<AtomicUsize>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Host {
    runner: Runner,
    observed: Arc<Mutex<Observation>>,
    streaming: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicUsize>,
}
impl Host {
    fn new(config: LiveConfig, notify_at: usize) -> Self {
        let observed = Arc::new(Mutex::new(Observation::default()));
        let streaming = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let runner: Runner = {
            let observed = observed.clone();
            let streaming = streaming.clone();
            let dropped = dropped.clone();
            Arc::new(move |request| {
                let config = config.clone();
                let observed = observed.clone();
                let streaming = streaming.clone();
                let flag = DropFlag(dropped.clone());
                Box::pin(async move {
                    let _flag = flag;
                    let store = Arc::new(MemoryStore::new());
                    let child = live_child(&config, store.clone());
                    let mut run = child
                        .prompt(None, request.task())
                        .await
                        .map_err(|_| ExtensionError::Tool("live child admission failed".into()))?;
                    let session = run.session_id().clone();
                    let mut events = run.events();
                    let mut text_deltas = 0;
                    loop {
                        tokio::select! {biased;
                            _=request.cancellation().cancelled()=>{observed.lock().unwrap().cancelled=true;run.interrupt();break;}
                            event=events.recv()=>match event {
                                Ok(Some(event))=>{
                                    observed.lock().unwrap().events.push(format!("{:?}",event.kind));
                                    if event.kind==EventKind::TextDelta {text_deltas+=1;if text_deltas==notify_at {streaming.notify_one();}}
                                }
                                Ok(None)=>break,
                                Err(_)=>return Err(ExtensionError::Tool("live child event stream failed".into())),
                            }
                        }
                    }
                    let result = run
                        .done()
                        .await
                        .map_err(|_| ExtensionError::Tool("live child settlement failed".into()))?;
                    {
                        let mut o = observed.lock().unwrap();
                        o.status = Some(result.status);
                        o.usage = result.usage.clone();
                    }
                    if result.status != RunStatus::Completed {
                        return Ok(Response::Failed(format!("{:?}", result.status)));
                    }
                    let messages = store.list_all_messages(&session).await.map_err(|_| {
                        ExtensionError::Tool("live child message read failed".into())
                    })?;
                    let text = messages
                        .iter()
                        .rev()
                        .find(|m| m.role == Role::Assistant)
                        .map(|m| {
                            m.parts
                                .iter()
                                .filter_map(|p| match &p.content {
                                    ContentBlock::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    Ok(Response::Completed(text))
                })
            })
        };
        Self {
            runner,
            observed,
            streaming,
            dropped,
        }
    }
    fn marker(&self, name: &str) {
        let o = self.observed.lock().unwrap();
        eprintln!(
            "live: executed test={name} child_status={:?} output_tokens={}",
            o.status.unwrap(),
            o.usage.output_tokens
        );
    }
}
fn extension(host: &Host, grace: u64) -> Arc<DelegateTask> {
    Arc::new(
        DelegateTask::new(Options {
            runner_identity: "live-opencode-go-v1".into(),
            runner: host.runner.clone(),
            limits: limits(grace),
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn live_background_child_completes_and_wait_returns_reply() {
    let _serial = LIVE.lock().await;
    let Some(config) = live_config() else {
        return;
    };
    let host = Host::new(config, 1);
    let ext = extension(&host, 10);
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![
        start_call(
            json!({"task":"Reply with exactly the single word CRABBER_OK and nothing else.","profile":"live"}),
        ),
        done(),
    ]));
    let first = agent(
        ext.clone(),
        store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = first.prompt(None, "start live child").await.unwrap();
    let session = run.session_id().clone();
    run.done().await.unwrap();
    let id = read_task_id(&snapshot_tool(&store, &session).await);
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(&[id], Some(120)),
        done(),
    ]));
    let second = agent(ext, store.clone(), provider, PermissionDecision::Allow);
    second
        .prompt(Some(session.clone()), "collect child")
        .await
        .unwrap()
        .done()
        .await
        .unwrap();
    let records = snapshot_tools(&store, &session).await;
    let value = result_value(records.last().unwrap());
    {
        let o = host.observed.lock().unwrap();
        assert_eq!(
            o.status,
            Some(RunStatus::Completed),
            "child status {:?}, events {:?}",
            o.status,
            o.events
        );
        assert!(o.usage.output_tokens > 0);
    }
    assert_eq!(value["status"], "done");
    assert_eq!(value["tasks"][0]["status"], "completed");
    assert!(
        value["tasks"][0]["output"]
            .as_str()
            .unwrap()
            .contains("CRABBER_OK")
    );
    first.close_extensions().await.unwrap();
    second.close_extensions().await.unwrap();
    host.marker("live_background_child_completes_and_wait_returns_reply");
}

#[tokio::test]
async fn live_close_interrupts_streaming_child_and_joins() {
    let _serial = LIVE.lock().await;
    let Some(config) = live_config() else {
        return;
    };
    let host = Host::new(config, 3);
    let (_, handle, tools) = mounted(extension(&host, 15)).await;
    let start = tools[START_TOOL]
        .execute_with_context(
            context(CancellationToken::new()),
            json!({"task":"Write an 800 word essay about Rust async runtimes.","profile":"live"}),
        )
        .await
        .unwrap();
    let streaming = tokio::time::timeout(Duration::from_secs(60), host.streaming.notified()).await;
    if streaming.is_err() {
        handle.close().await.unwrap();
        let o = host.observed.lock().unwrap();
        panic!(
            "streaming timeout: child status {:?}, events {:?}",
            o.status, o.events
        );
    }
    let before = tokio::time::Instant::now();
    handle.close().await.unwrap();
    assert!(before.elapsed() <= Duration::from_secs(15));
    {
        let o = host.observed.lock().unwrap();
        assert!(o.cancelled);
        assert_eq!(
            o.status,
            Some(RunStatus::Interrupted),
            "model finished before cancellation; rerun"
        );
    }
    assert_eq!(host.dropped.load(Ordering::SeqCst), 1);
    let status = tools[STATUS_TOOL]
        .execute_with_context(
            context(CancellationToken::new()),
            json!({"task_id":start["task_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["status"], "cancelled");
    host.marker("live_close_interrupts_streaming_child_and_joins");
}
