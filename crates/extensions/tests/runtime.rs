use async_trait::async_trait;
use crabber::{
    Agent, AgentConfig, FakeProvider, PermissionDecision, Selection, StaticPolicy, StreamDelta,
    core::{RunStatus, ToolCallId, ToolInfo},
    extension::{
        Extension, ExtensionError, ModelRequestError, Point, PromptSection, Registrar, Scope,
        ToolDefinition, ToolExecutor, TransformOutput,
    },
    session::{MemoryStore, SnapshotLimits, SnapshotOutcome, SnapshotRequest, Store},
};
use crabber_extensions::{
    tool_result_redactor::{
        Limits as RedactorLimits, Options as RedactorOptions, ToolResultRedactor,
    },
    workspace_instructions::{
        Limits as InstructionLimits, Options as InstructionOptions, Resolver, TrustedWorkspace,
        WorkspaceInstructions,
    },
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

const SECRET: &str = "ghp_0123456789abcdef";
fn redactor() -> Arc<ToolResultRedactor> {
    Arc::new(
        ToolResultRedactor::new(RedactorOptions {
            order: -100,
            excluded_tools: vec![],
            additional_patterns: vec![],
            limits: RedactorLimits {
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
    )
}
fn instruction_options(resolver: Resolver) -> InstructionOptions {
    InstructionOptions {
        order: 100,
        file_names: vec!["AGENTS.md".into()],
        resolver_identity: "host-v1".into(),
        resolver,
        limits: InstructionLimits {
            max_file_names: 4,
            max_chain_depth: 8,
            max_file_bytes: 1024,
            max_section_bytes: 4096,
            max_in_flight: 4,
            max_wait: Duration::from_secs(1),
        },
    }
}
fn config(root: &str) -> AgentConfig {
    let mut c = AgentConfig::new(Selection {
        provider_id: "fake".into(),
        model_id: "scripted".into(),
    });
    c.workspace_id = "workspace".into();
    c.directory = root.into();
    c.system_prompt = Some("base".into());
    c
}
fn done() -> Vec<StreamDelta> {
    vec![
        StreamDelta::TextDelta("done".into()),
        StreamDelta::Completed,
    ]
}
struct SyntheticTool {
    rewrite: Option<PathBuf>,
    failed: bool,
}
#[async_trait]
impl ToolExecutor for SyntheticTool {
    async fn execute(&self, _: Value) -> Result<Value, ExtensionError> {
        if let Some(file) = &self.rewrite {
            std::fs::write(file, "second-payload").unwrap();
        }
        if self.failed {
            Err(ExtensionError::Tool(SECRET.into()))
        } else {
            Ok(json!({"output":SECRET,"ok":42}))
        }
    }
}
fn tool(failed: bool, rewrite: Option<PathBuf>) -> Arc<ToolDefinition> {
    Arc::new(ToolDefinition {
        info: ToolInfo {
            name: "synthetic".into(),
            description: "Synthetic test".into(),
            parameters: json!({"type":"object"}),
            retry_safe: false,
            required_permissions: vec![],
        },
        executor: Arc::new(SyntheticTool { failed, rewrite }),
    })
}
fn call(name: &str) -> Vec<StreamDelta> {
    let id = ToolCallId::new();
    vec![
        StreamDelta::ToolCallStart {
            call_id: id.clone(),
            name: name.into(),
        },
        StreamDelta::ToolCallArgsDelta {
            call_id: id.clone(),
            text: "{}".into(),
        },
        StreamDelta::ToolCallDone { call_id: id },
        StreamDelta::Completed,
    ]
}
struct Reducer;
#[async_trait]
impl Extension for Reducer {
    fn id(&self) -> &str {
        "fixture/reducer"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn config_hash(&self) -> String {
        "fixture".into()
    }
    async fn install(&self, r: &mut Registrar) -> Result<(), ExtensionError> {
        r.on_result_transform(
            1_000_000,
            "fixture/reducer",
            Arc::new(|_, _| {
                Box::pin(async { Ok(TransformOutput::marked_error(json!({"reduced":SECRET}))) })
            }),
        );
        Ok(())
    }
}
#[tokio::test]
async fn final_redaction_protects_durable_results_events_and_next_provider_after_reducer() {
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![call("synthetic"), done()]));
    let agent = Agent::builder()
        .store(store.clone())
        .provider(provider.clone())
        .config(config("/workspace"))
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .tool(tool(false, None))
        .extension(redactor(), Scope::Global)
        .extension(Arc::new(Reducer), Scope::Global)
        .build()
        .unwrap();
    let run = agent.prompt(None, "fixture").await.unwrap();
    let session = run.session_id().clone();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let next = format!("{:?}", requests[1]);
    assert!(!next.contains(SECRET) && next.contains("[REDACTED]"));
    let messages = store.list_all_messages(&session).await.unwrap();
    let events = store.list_events(&session, None, 100).await.unwrap();
    assert!(!format!("{messages:?}{events:?}").contains(SECRET));
    assert!(format!("{messages:?}{events:?}").contains("[REDACTED]"));
    let SnapshotOutcome::Page(page) = store
        .snapshot(SnapshotRequest {
            session_id: session,
            limits: SnapshotLimits {
                messages: 100,
                tool_calls: 100,
                parts: 100,
                text_bytes: 10000,
                encoded_bytes: 100000,
            },
            continuation: None,
        })
        .await
        .unwrap()
    else {
        panic!("snapshot")
    };
    assert_eq!(page.tool_calls.len(), 1);
    let result = page.tool_calls[0].result.as_ref().unwrap();
    assert_eq!(result.status, crabber::core::ToolResultStatus::Failed);
    assert!(!format!("{result:?}").contains(SECRET));
    agent.close_extensions().await.unwrap();
}
#[tokio::test]
async fn error_and_unknown_tool_paths_remain_errors_after_redaction() {
    for (failed, name) in [(true, "synthetic"), (false, SECRET)] {
        let store = Arc::new(MemoryStore::new());
        let provider = Arc::new(FakeProvider::scripted(vec![call(name), done()]));
        let agent = Agent::builder()
            .store(store.clone())
            .provider(provider.clone())
            .config(config("/workspace"))
            .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
            .tool(tool(failed, None))
            .extension(redactor(), Scope::Global)
            .build()
            .unwrap();
        let run = agent.prompt(None, "fixture").await.unwrap();
        let session = run.session_id().clone();
        run.done().await.unwrap();
        let messages = store.list_all_messages(&session).await.unwrap();
        let results: Vec<_> = messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match &p.content {
                crabber::core::ContentBlock::ToolResult {
                    content, is_error, ..
                } => Some((content, is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 1);
        assert!(*results[0].1);
        assert!(!format!("{:?}", results[0].0).contains(SECRET));
        // Unknown tool names also occur in arguments/call metadata; the redactor
        // protects result payloads, not that separate durable input identity.
        agent.close_extensions().await.unwrap();
    }
}
struct RetryRewrite(PathBuf);
#[async_trait]
impl Extension for RetryRewrite {
    fn id(&self) -> &str {
        "fixture/retry"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn config_hash(&self) -> String {
        "fixture".into()
    }
    async fn install(&self, r: &mut Registrar) -> Result<(), ExtensionError> {
        let file = self.0.clone();
        r.on_transform(
            ModelRequestError::ID,
            0,
            "rewrite",
            Arc::new(move |v| {
                let file = file.clone();
                Box::pin(async move {
                    std::fs::write(file, "second-payload").unwrap();
                    Ok(v)
                })
            }),
        );
        Ok(())
    }
}
#[tokio::test]
async fn instruction_changes_refresh_on_provider_retry_and_later_turn_without_accumulation() {
    use crabber::providers::{ProviderError, ProviderErrorKind};
    for retry in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("AGENTS.md");
        std::fs::write(&file, "first-payload").unwrap();
        let grant = Arc::new(TrustedWorkspace::open(temp.path(), temp.path()).unwrap());
        let directory = grant.root().to_str().unwrap().to_owned();
        let resolver: Resolver = Arc::new(move |ctx| {
            let g = grant.clone();
            Box::pin(async move {
                assert_eq!(ctx.workspace().workspace_id(), Some("workspace"));
                Ok(Some(g))
            })
        });
        let first = if retry {
            vec![StreamDelta::Error(ProviderError {
                kind: ProviderErrorKind::RateLimited,
                message: "fixture".into(),
                retryable: true,
            })]
        } else {
            call("synthetic")
        };
        let provider = Arc::new(FakeProvider::scripted(vec![first, done()]));
        let agent = Agent::builder()
            .memory()
            .provider(provider.clone())
            .config(config(&directory))
            .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
            .prompt_section(Arc::new(PromptSection {
                name: "static".into(),
                order: 0,
                text: "static".into(),
            }))
            .tool(tool(false, Some(file.clone())))
            .extension(
                Arc::new(WorkspaceInstructions::new(instruction_options(resolver)).unwrap()),
                Scope::Global,
            )
            .extension(Arc::new(RetryRewrite(file)), Scope::Global)
            .build()
            .unwrap();
        assert_eq!(
            agent
                .prompt(None, "fixture")
                .await
                .unwrap()
                .done()
                .await
                .unwrap()
                .status,
            RunStatus::Completed
        );
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let first = requests[0].system.as_deref().unwrap();
        let next = requests[1].system.as_deref().unwrap();
        assert!(first.starts_with("base\nstatic\n") && first.contains("first-payload"));
        assert!(
            next.starts_with("base\nstatic\n")
                && next.contains("second-payload")
                && !next.contains("first-payload")
        );
        assert_eq!(next.matches("<workspace_instructions").count(), 1);
        agent.close_extensions().await.unwrap();
    }
}
#[tokio::test]
async fn resolver_failure_is_sanitized_and_calls_no_provider() {
    let resolver: Resolver = Arc::new(|_| {
        Box::pin(async { Err(ExtensionError::Tool("fixture-sensitive-error".into())) })
    });
    let provider = Arc::new(FakeProvider::scripted(vec![done()]));
    let agent = Agent::builder()
        .memory()
        .provider(provider.clone())
        .config(config("/workspace"))
        .extension(
            Arc::new(WorkspaceInstructions::new(instruction_options(resolver)).unwrap()),
            Scope::Global,
        )
        .build()
        .unwrap();
    let error = agent
        .prompt(None, "fixture")
        .await
        .unwrap()
        .done()
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("fixture-sensitive-error"));
    assert!(provider.requests().is_empty());
    agent.close_extensions().await.unwrap();
}

#[tokio::test]
async fn interrupt_during_host_resolution_settles_interrupted_and_cleanup_is_joined() {
    use tokio::sync::Notify;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let resolver: Resolver = {
        let entered = entered.clone();
        let release = release.clone();
        Arc::new(move |context| {
            let entered = entered.clone();
            let release = release.clone();
            Box::pin(async move {
                entered.notify_one();
                context.cancellation().cancelled().await;
                // Cleanup may deliberately finish after callback cancellation.
                release.notified().await;
                Ok(None)
            })
        })
    };
    let provider = Arc::new(FakeProvider::scripted(vec![done()]));
    let agent = Arc::new(
        Agent::builder()
            .memory()
            .provider(provider.clone())
            .config(config("/workspace"))
            .extension(
                Arc::new(WorkspaceInstructions::new(instruction_options(resolver)).unwrap()),
                Scope::Global,
            )
            .build()
            .unwrap(),
    );
    let run = agent.prompt(None, "fixture").await.unwrap();
    entered.notified().await;
    run.interrupt();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Interrupted);
    assert!(provider.requests().is_empty());
    let closer = agent.clone();
    let closing = tokio::spawn(async move { closer.close_extensions().await });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.notify_one();
    closing.await.unwrap().unwrap();
}

#[tokio::test]
async fn concurrent_sessions_resolve_only_their_admitted_workspace() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("AGENTS.md"), "first-workspace-only").unwrap();
    std::fs::write(second.path().join("AGENTS.md"), "second-workspace-only").unwrap();
    let a = Arc::new(TrustedWorkspace::open(first.path(), first.path()).unwrap());
    let b = Arc::new(TrustedWorkspace::open(second.path(), second.path()).unwrap());
    let roots = [
        a.root().to_string_lossy().into_owned(),
        b.root().to_string_lossy().into_owned(),
    ];
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let resolver: Resolver = Arc::new(move |context| {
        let a = a.clone();
        let b = b.clone();
        let barrier = barrier.clone();
        Box::pin(async move {
            barrier.wait().await;
            let workspace = if context.workspace().directory() == a.root().to_str() {
                a
            } else {
                b
            };
            Ok(Some(workspace))
        })
    });
    let extension = Arc::new(WorkspaceInstructions::new(instruction_options(resolver)).unwrap());
    let store = Arc::new(MemoryStore::new());
    let providers = [
        Arc::new(FakeProvider::scripted(vec![done()])),
        Arc::new(FakeProvider::scripted(vec![done()])),
    ];
    let a = Agent::builder()
        .store(store.clone())
        .provider(providers[0].clone())
        .config(config(&roots[0]))
        .extension(extension.clone(), Scope::Global)
        .build()
        .unwrap();
    let b = Agent::builder()
        .store(store)
        .provider(providers[1].clone())
        .config(config(&roots[1]))
        .extension(extension, Scope::Global)
        .build()
        .unwrap();
    let (ar, br) = tokio::join!(a.prompt(None, "first"), b.prompt(None, "second"));
    let ar = ar.unwrap();
    let br = br.unwrap();
    assert_ne!(ar.session_id(), br.session_id());
    let (ad, bd) = tokio::join!(ar.done(), br.done());
    ad.unwrap();
    bd.unwrap();
    let at = providers[0].requests()[0].system.clone().unwrap();
    let bt = providers[1].requests()[0].system.clone().unwrap();
    assert!(at.contains("first-workspace-only") && !at.contains("second-workspace-only"));
    assert!(bt.contains("second-workspace-only") && !bt.contains("first-workspace-only"));
    a.close_extensions().await.unwrap();
    b.close_extensions().await.unwrap();
}
