use crabber::{
    core::{RunId, SessionId, TurnId},
    extension::{
        Extension, PromptAttemptContext, PromptContributionOutcome, Registry, Scope,
        WorkspaceContext, collect_prompt_contributions,
    },
};
use crabber_extensions::workspace_instructions::{
    Limits, Options, Resolver, TrustedWorkspace, WorkspaceInstructions,
};
use std::{path::Path, sync::Arc, time::Duration};

fn options(resolver: Resolver) -> Options {
    Options {
        order: 100,
        file_names: vec!["AGENTS.md".into()],
        resolver_identity: "host-v1".into(),
        resolver,
        limits: Limits {
            max_file_names: 4,
            max_chain_depth: 8,
            max_file_bytes: 1024,
            max_section_bytes: 4096,
            max_in_flight: 4,
            max_wait: Duration::from_secs(1),
        },
    }
}
fn resolver(workspace: Arc<TrustedWorkspace>) -> Resolver {
    Arc::new(move |_| {
        let w = workspace.clone();
        Box::pin(async move { Ok(Some(w)) })
    })
}
fn context(root: &Path) -> PromptAttemptContext {
    PromptAttemptContext::new(
        SessionId::from("session"),
        RunId::from("run"),
        TurnId::from("turn"),
        WorkspaceContext::from_persisted("ws", root.to_str().unwrap()),
        "fake".into(),
        "scripted".into(),
        1,
        false,
    )
}
async fn collect(extension: WorkspaceInstructions, root: &Path) -> PromptContributionOutcome {
    let registry = Registry::new();
    let handle = registry
        .mount(Arc::new(extension), Scope::Global)
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let outcome = collect_prompt_contributions(&plan.prompt_contributors, context(root)).await;
    drop(plan);
    handle.close().await.unwrap();
    outcome
}
fn text(outcome: PromptContributionOutcome) -> String {
    let PromptContributionOutcome::Completed { sections } = outcome else {
        panic!("unexpected contribution outcome: {outcome:?}");
    };
    sections
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
#[tokio::test]
async fn ordered_boundary_chain_and_live_file_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("child");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(temp.path().join("AGENTS.md"), "parent").unwrap();
    std::fs::write(root.join("AGENTS.md"), "child-v1\n").unwrap();
    let workspace = Arc::new(TrustedWorkspace::open(temp.path(), &root).unwrap());
    let directory = workspace.root().to_owned();
    let r = resolver(workspace);
    let first = text(
        collect(
            WorkspaceInstructions::new(options(r.clone())).unwrap(),
            &directory,
        )
        .await,
    );
    assert!(first.find("parent").unwrap() < first.find("child-v1").unwrap());
    assert!(first.contains("../AGENTS.md") && first.contains("./AGENTS.md"));
    std::fs::write(root.join("AGENTS.md"), "child-v2").unwrap();
    let second = text(collect(WorkspaceInstructions::new(options(r)).unwrap(), &directory).await);
    assert!(second.contains("child-v2") && !second.contains("child-v1"));
}
#[tokio::test]
async fn invalid_files_and_bounded_truncation_and_omission() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let workspace = Arc::new(TrustedWorkspace::open(root, root).unwrap());
    let mut o = options(resolver(workspace.clone()));
    o.limits.max_file_bytes = 3;
    std::fs::write(root.join("AGENTS.md"), "ééé").unwrap();
    let value = text(collect(WorkspaceInstructions::new(o).unwrap(), workspace.root()).await);
    assert!(value.contains("é\n") && value.contains("truncated=\"true\""));
    for bytes in [vec![0xff], vec![0], b" \n".to_vec()] {
        std::fs::write(root.join("AGENTS.md"), bytes).unwrap();
        assert_eq!(
            text(
                collect(
                    WorkspaceInstructions::new(options(resolver(workspace.clone()))).unwrap(),
                    workspace.root()
                )
                .await
            ),
            ""
        );
    }
    std::fs::write(root.join("AGENTS.md"), "x".repeat(1024)).unwrap();
    let mut o = options(resolver(workspace.clone()));
    o.limits.max_section_bytes = 256;
    let value = text(collect(WorkspaceInstructions::new(o).unwrap(), workspace.root()).await);
    assert!(
        value.len() <= 256
            && value.contains("[omitted:")
            && value.ends_with("</workspace_instructions>")
    );
}
#[cfg(unix)]
#[tokio::test]
async fn final_symlinks_are_skipped_and_admitted_directory_handles_stay_pinned() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(temp.path().join("outside"), "outside-secret").unwrap();
    let workspace = Arc::new(TrustedWorkspace::open(&root, &root).unwrap());
    symlink(temp.path().join("outside"), root.join("AGENTS.md")).unwrap();
    assert_eq!(
        text(
            collect(
                WorkspaceInstructions::new(options(resolver(workspace.clone()))).unwrap(),
                workspace.root()
            )
            .await
        ),
        ""
    );
    std::fs::remove_file(root.join("AGENTS.md")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "admitted").unwrap();
    std::fs::rename(&root, temp.path().join("moved")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("AGENTS.md"), "replacement").unwrap();
    let value = text(
        collect(
            WorkspaceInstructions::new(options(resolver(workspace.clone()))).unwrap(),
            workspace.root(),
        )
        .await,
    );
    assert!(value.contains("admitted") && !value.contains("replacement"));
}
#[tokio::test]
async fn unavailable_or_different_workspace_cannot_route_to_host_default() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Arc::new(TrustedWorkspace::open(temp.path(), temp.path()).unwrap());
    let other = tempfile::tempdir().unwrap();
    assert!(matches!(
        collect(
            WorkspaceInstructions::new(options(resolver(workspace))).unwrap(),
            other.path()
        )
        .await,
        PromptContributionOutcome::Failed { .. }
    ));
    assert_eq!(
        text(
            collect(
                WorkspaceInstructions::new(options(Arc::new(|_| Box::pin(async { Ok(None) }))))
                    .unwrap(),
                temp.path()
            )
            .await
        ),
        ""
    );
}
#[test]
fn validates_configuration_and_fingerprints_policy() {
    let none: Resolver = Arc::new(|_| Box::pin(async { Ok(None) }));
    let hash = WorkspaceInstructions::new(options(none.clone()))
        .unwrap()
        .config_hash();
    let mut o = options(none.clone());
    o.resolver_identity = "host-v2".into();
    assert_ne!(hash, WorkspaceInstructions::new(o).unwrap().config_hash());
    for name in ["../AGENTS.md", "a/b", ".", "", "nul\0"] {
        let mut o = options(none.clone());
        o.file_names = vec![name.into()];
        assert!(WorkspaceInstructions::new(o).is_err());
    }
    let mut o = options(none);
    o.limits.max_section_bytes = 0;
    assert!(WorkspaceInstructions::new(o).is_err());
}

#[tokio::test]
async fn session_mount_shadows_global_and_other_sessions_keep_global() {
    let root = tempfile::tempdir().unwrap();
    let make = |label: &'static str| {
        let resolver: Resolver = Arc::new(move |_| Box::pin(async { Ok(None) }));
        let mut o = options(resolver);
        o.resolver_identity = label.into();
        // Withholding a session's contribution must still shadow global discovery.
        WorkspaceInstructions::new(o).unwrap()
    };
    std::fs::write(root.path().join("AGENTS.md"), "global-content").unwrap();
    let workspace = Arc::new(TrustedWorkspace::open(root.path(), root.path()).unwrap());
    let registry = Registry::new();
    let global = registry
        .mount(
            Arc::new(WorkspaceInstructions::new(options(resolver(workspace.clone()))).unwrap()),
            Scope::Global,
        )
        .await
        .unwrap();
    let local = registry
        .mount(
            Arc::new(make("session-v1")),
            Scope::Session(SessionId::from("session")),
        )
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    assert_eq!(plan.prompt_contributors.len(), 1);
    assert_eq!(
        text(
            collect_prompt_contributions(&plan.prompt_contributors, context(workspace.root()))
                .await
        ),
        ""
    );
    drop(plan);
    let other = registry.acquire(&SessionId::from("other"));
    let result = text(
        collect_prompt_contributions(&other.prompt_contributors, context(workspace.root())).await,
    );
    assert!(result.contains("global-content"));
    drop(other);
    local.close().await.unwrap();
    global.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn deadline_keeps_noncooperative_resolver_tracked_and_capacity_held_until_release() {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::{Notify, oneshot};
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let entered = Arc::new(Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let resolver: Resolver = {
        let entered = entered.clone();
        let count = count.clone();
        Arc::new(move |_| {
            let rx = release.lock().unwrap().take();
            let entered = entered.clone();
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                entered.notify_one();
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
                Ok(None)
            })
        })
    };
    let registry = Registry::new().with_close_timeout(Duration::from_millis(50));
    let mut o = options(resolver);
    o.limits.max_in_flight = 1;
    o.limits.max_wait = Duration::from_millis(20);
    let handle = registry
        .mount(
            Arc::new(WorkspaceInstructions::new(o).unwrap()),
            Scope::Global,
        )
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let task_plan = plan.clone();
    let first = tokio::spawn(async move {
        let task_plan = task_plan; // Move the whole plan, including its lease.
        collect_prompt_contributions(
            &task_plan.prompt_contributors,
            context(Path::new("/workspace")),
        )
        .await
    });
    entered.notified().await;
    tokio::time::advance(Duration::from_millis(21)).await;
    assert!(matches!(
        first.await.unwrap(),
        PromptContributionOutcome::Failed { .. }
    ));
    assert!(matches!(
        collect_prompt_contributions(&plan.prompt_contributors, context(Path::new("/workspace")))
            .await,
        PromptContributionOutcome::Failed { .. }
    ));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "saturated calls must not enter host resolver"
    );
    drop(plan);
    assert!(matches!(
        handle.close().await,
        Err(crabber::ExtensionError::MountCloseTimeout { .. })
    ));
    release_tx.send(()).unwrap();
    // The callback ended at its deadline; its work remains joined by the mount.
    handle.close().await.unwrap();
}

#[tokio::test]
async fn boundary_admission_and_configured_chain_limit_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    assert!(TrustedWorkspace::open(temp.path(), outside.path()).is_err());
    assert!(TrustedWorkspace::open(Path::new("relative"), temp.path()).is_err());
    let root = temp.path().join("child");
    std::fs::create_dir(&root).unwrap();
    let workspace = Arc::new(TrustedWorkspace::open(temp.path(), &root).unwrap());
    let mut o = options(resolver(workspace.clone()));
    o.limits.max_chain_depth = 1;
    assert!(matches!(
        collect(WorkspaceInstructions::new(o).unwrap(), workspace.root()).await,
        PromptContributionOutcome::Failed { .. }
    ));
}
