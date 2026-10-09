use super::*;
#[tokio::test(flavor = "multi_thread")]
async fn start_then_status_are_durable_and_reenter_the_model() {
    if !shell_available() {
        return;
    }
    let h = Harness::new(|_| {}).await;
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![
        call(
            START_TOOL,
            json!({"command":command("printf out; printf err >&2; exit 3")}),
        ),
        done(),
    ]));
    let a = agent(
        h.ext.clone(),
        store.clone(),
        provider.clone(),
        PermissionDecision::Allow,
        h.dir.path(),
    );
    let mut run = a.prompt(None, "start").await.unwrap();
    let session = run.session_id().clone();
    let mut events = run.events();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let records = snapshot_tools(&store, &session).await;
    let start = result_value(&records[0]);
    assert_eq!(start["state"], "running");
    let id = start["id"].as_str().unwrap();
    assert_eq!(id.len(), 53);
    assert_eq!(start["timeout_seconds"], 0);
    assert_eq!(start["started_at"].as_str().unwrap().len(), 30);
    let status = timeout(Duration::from_secs(10), async {
        loop {
            let value = h.tools[STATUS_TOOL]
                .execute_with_context(
                    context_for(
                        h.dir.path(),
                        CancellationToken::new(),
                        session.clone(),
                        "workspace",
                    ),
                    json!({"id":id}),
                )
                .await
                .unwrap();
            if value["state"] != "running" {
                return value;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(status["state"], "failed");
    assert_eq!(status["exit_code"], 3);
    assert_eq!(status["stdout"], json!({"text":"out","truncated":false}));
    assert_eq!(status["stderr"], json!({"text":"err","truncated":false}));
    let provider2 = Arc::new(FakeProvider::scripted(vec![
        call(STATUS_TOOL, json!({"id":id})),
        done(),
    ]));
    let a2 = agent(
        h.ext.clone(),
        store.clone(),
        provider2.clone(),
        PermissionDecision::Allow,
        h.dir.path(),
    );
    let mut run = a2.prompt(Some(session.clone()), "status").await.unwrap();
    let mut events2 = run.events();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let records = snapshot_tools(&store, &session).await;
    let record = records.iter().find(|r| r.name == STATUS_TOOL).unwrap();
    assert_eq!(result_value(record), status);
    provider_contains(&provider2, 1, result_text(record));
    let mut settled = String::new();
    while let Ok(Some(event)) = events.recv().await {
        settled.push_str(&format!("{event:?}"));
    }
    while let Ok(Some(event)) = events2.recv().await {
        settled.push_str(&format!("{event:?}"));
    }
    assert!(settled.contains("ToolCallSettled"));
    assert!(settled.contains("out") && settled.contains("err"));
    a.close_extensions().await.unwrap();
    a2.close_extensions().await.unwrap();
    h.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn owner_isolation_and_list_order() {
    if !shell_available() {
        return;
    }
    let h = Harness::new(|_| {}).await;
    let first = h.start("sleep 30").await;
    let second = h.start("sleep 30").await;
    let ids = [
        first["id"].as_str().unwrap(),
        second["id"].as_str().unwrap(),
    ];
    for (session, workspace) in [
        (SessionId::from("other"), "workspace"),
        (SessionId::from("session"), "other"),
    ] {
        let ctx = || {
            context_for(
                h.dir.path(),
                CancellationToken::new(),
                session.clone(),
                workspace,
            )
        };
        assert_eq!(
            h.tools[LIST_TOOL]
                .execute_with_context(ctx(), json!({}))
                .await
                .unwrap(),
            json!({"jobs":[]})
        );
        for tool in [STATUS_TOOL, KILL_TOOL] {
            assert!(
                h.tools[tool]
                    .execute_with_context(ctx(), json!({"id":ids[0]}))
                    .await
                    .unwrap_err()
                    .to_string()
                    .ends_with("job-not-found")
            );
        }
    }
    let list = h.execute(LIST_TOOL, json!({})).await.unwrap();
    assert_eq!(list["jobs"][0]["id"], ids[0]);
    assert_eq!(list["jobs"][1]["id"], ids[1]);
    for id in ids {
        assert_eq!(h.kill(id).await["state"], "killed");
    }
    h.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn permission_deny_and_ask_never_spawn() {
    if !shell_available() {
        return;
    }
    for decision in [PermissionDecision::Deny, PermissionDecision::Ask] {
        let h = Harness::new(|_| {}).await;
        let store = Arc::new(MemoryStore::new());
        let provider = Arc::new(FakeProvider::scripted(vec![
            call(START_TOOL, json!({"command":command(": > \"$CANARY\"")})),
            done(),
        ]));
        let a = agent(
            h.ext.clone(),
            store.clone(),
            provider,
            decision,
            h.dir.path(),
        );
        let run = a.prompt(None, "deny").await.unwrap();
        let session = run.session_id().clone();
        run.done().await.unwrap();
        let records = snapshot_tools(&store, &session).await;
        assert_eq!(records[0].status, ToolCallStatus::Failed);
        assert_eq!(result_value(&records[0]), json!("permission denied"));
        assert_eq!(h.ext.live_jobs(), 0);
        assert!(!h.dir.path().join("canary").exists());
        assert!(pgids(h.dir.path()).is_empty());
        assert_eq!(
            h.execute(LIST_TOOL, json!({})).await.unwrap(),
            json!({"jobs":[]})
        );
        a.close_extensions().await.unwrap();
        h.close().await;
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn input_validation_is_durable_and_spawns_nothing() {
    if !shell_available() {
        return;
    }
    let h = Harness::new(|_| {}).await;
    let cases = vec![
        (START_TOOL, json!({}), "shape"),
        (START_TOOL, json!({"command":""}), "command"),
        (START_TOOL, json!({"command":"x\0"}), "command"),
        (
            START_TOOL,
            json!({"command":"x","working_directory":"../x"}),
            "working-directory",
        ),
        (
            START_TOOL,
            json!({"command":"x","timeout_seconds":31}),
            "timeout",
        ),
        (
            START_TOOL,
            json!({"command":"x","timeout_seconds":null}),
            "shape",
        ),
        (STATUS_TOOL, json!({"id":"bad"}), "id"),
        (KILL_TOOL, json!({"id":"bad"}), "id"),
        (LIST_TOOL, json!({"extra":true}), "shape"),
    ];
    for (tool, args, code) in cases {
        let store = Arc::new(MemoryStore::new());
        let provider = Arc::new(FakeProvider::scripted(vec![call(tool, args), done()]));
        let a = agent(
            h.ext.clone(),
            store.clone(),
            provider,
            PermissionDecision::Allow,
            h.dir.path(),
        );
        let run = a.prompt(None, "invalid").await.unwrap();
        let session = run.session_id().clone();
        run.done().await.unwrap();
        let records = snapshot_tools(&store, &session).await;
        assert_eq!(records[0].status, ToolCallStatus::Failed);
        if code != "shape" {
            assert!(
                result_value(&records[0])
                    .as_str()
                    .unwrap()
                    .contains(&format!("background jobs input invalid: {code}"))
            );
        }
        assert_eq!(h.ext.live_jobs(), 0);
    }
    assert!(!h.dir.path().join("canary").exists());
    assert!(pgids(h.dir.path()).is_empty());
    h.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn working_directory_is_contained_and_resolved() {
    if !shell_available() {
        return;
    }
    let h = Harness::new(|_| {}).await;
    std::fs::create_dir(h.dir.path().join("inside")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), h.dir.path().join("escape")).unwrap();
    std::os::unix::fs::symlink(h.dir.path().join("inside"), h.dir.path().join("link")).unwrap();
    std::fs::write(h.dir.path().join("file"), b"fixture").unwrap();
    for cwd in ["inside", "link"] {
        let start = h
            .execute(
                START_TOOL,
                json!({"command":command("pwd -P"),"working_directory":cwd}),
            )
            .await
            .unwrap();
        let status = h.terminal(start["id"].as_str().unwrap()).await;
        assert_eq!(
            status["stdout"]["text"],
            format!(
                "{}\n",
                h.dir
                    .path()
                    .join("inside")
                    .canonicalize()
                    .unwrap()
                    .display()
            )
        );
    }
    for cwd in ["../x", "/tmp", "escape", "file"] {
        assert!(
            h.execute(START_TOOL, json!({"command":"pwd","working_directory":cwd}))
                .await
                .unwrap_err()
                .to_string()
                .ends_with("working-directory")
        );
    }
    let error = h.tools[START_TOOL]
        .execute_with_context(
            context(&h.dir.path().join("missing"), CancellationToken::new()),
            json!({"command":"pwd"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().ends_with("workspace-root"));
    h.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn direct_executor_rejects_missing_context_and_owner() {
    let h = Harness::new(|_| {}).await;
    assert!(
        h.tools[START_TOOL]
            .execute(json!({"command":"x"}))
            .await
            .unwrap_err()
            .to_string()
            .ends_with("context")
    );
    let mut ctx = context(h.dir.path(), CancellationToken::new());
    ctx = ToolContext::new(
        ctx.session_id,
        ctx.run_id,
        ctx.call_id,
        ctx.cancel,
        ctx.host,
        WorkspaceContext::from_persisted("", h.dir.path().to_str().unwrap()),
        Arc::new(|_| {}),
        None,
    );
    assert!(
        h.tools[START_TOOL]
            .execute_with_context(ctx, json!({"command":"x"}))
            .await
            .unwrap_err()
            .to_string()
            .ends_with("owner")
    );
    let ctx = ToolContext::new(
        SessionId::from("session"),
        RunId::from("run"),
        ToolCallId::from("call"),
        CancellationToken::new(),
        HostServices::default(),
        WorkspaceContext::from_persisted("workspace", ""),
        Arc::new(|_| {}),
        None,
    );
    assert!(
        h.tools[START_TOOL]
            .execute_with_context(ctx, json!({"command":"x"}))
            .await
            .unwrap_err()
            .to_string()
            .ends_with("workspace-root")
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    for tool in [START_TOOL, STATUS_TOOL, LIST_TOOL, KILL_TOOL] {
        assert!(
            h.tools[tool]
                .execute_with_context(context(h.dir.path(), cancel.clone()), json!({}))
                .await
                .unwrap_err()
                .to_string()
                .ends_with("cancelled")
        );
    }
    assert_eq!(h.ext.live_jobs(), 0);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn status_result_at_capacity_survives_json_escaping() {
    if !shell_available() {
        return;
    }
    let h = Harness::new(|o| o.limits.max_output_bytes_per_stream = 32).await;
    let start=h.start("i=0; while [ \"$i\" -lt 16 ]; do printf '\"\\001'; printf '\"\\001' >&2; i=$((i+1)); done").await;
    let status = h.terminal(start["id"].as_str().unwrap()).await;
    let text = status.to_string();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), status);
    assert_eq!(status["stdout"]["text"].as_str().unwrap().len(), 32);
    assert_eq!(status["stdout"]["truncated"], false);
    assert!(
        text.len()
            <= Limits {
                max_output_bytes_per_stream: 32,
                ..limits()
            }
            .worst_case_status_bytes()
    );
    h.close().await;
}
