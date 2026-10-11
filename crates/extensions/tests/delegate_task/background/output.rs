use super::*;

#[tokio::test]
async fn wait_returns_done_immediately_when_all_terminal() {
    let count = Arc::new(AtomicUsize::new(0));
    let host: Runner = Arc::new(move |_| {
        let first = count.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            Ok(if first {
                Response::Completed("ok".into())
            } else {
                Response::Failed("failed".into())
            })
        })
    });
    let mut j = Journey::new(host, limits());
    let a = j.start().await;
    j.wait(std::slice::from_ref(&a)).await;
    let b = j.start().await;
    let reply = j.wait(&[a, b]).await;
    assert_eq!(reply["status"], "done");
    assert_eq!(reply["tasks"][0]["status"], "completed");
    assert_eq!(reply["tasks"][1]["status"], "failed");
}

#[tokio::test]
async fn wait_output_budget_omits_later_outputs() {
    let count = Arc::new(AtomicUsize::new(0));
    let host: Runner = Arc::new(move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::Completed(if n == 3 {
                String::new()
            } else {
                "x".repeat(200)
            }))
        })
    });
    let mut j = Journey::new(
        host,
        Limits {
            max_result_bytes: 256,
            ..limits()
        },
    );
    let mut ids = Vec::new();
    for _ in 0..4 {
        let id = j.start().await;
        j.wait(std::slice::from_ref(&id)).await;
        ids.push(id);
    }
    let reply = j.wait(&ids).await;
    assert_eq!(reply["tasks"][0]["output"], "x".repeat(200));
    for i in [1, 2] {
        assert_eq!(reply["tasks"][i]["output_omitted"], true);
        assert!(reply["tasks"][i].get("output").is_none());
        assert_eq!(
            result_value(&j.call(STATUS_TOOL, json!({"task_id":ids[i]})).await)["output"],
            "x".repeat(200)
        );
    }
    assert!(reply["tasks"][3].get("output_omitted").is_none());
}

#[tokio::test]
async fn background_output_at_byte_limit_survives_escaping() {
    let mut j = Journey::new(runner(Response::Completed("\"".repeat(100))), limits());
    let id = j.start().await;
    assert_eq!(j.wait(&[id]).await["tasks"][0]["output"], "\"".repeat(100));
}

#[tokio::test]
async fn permit_is_free_when_terminal_status_is_visible() {
    let mut j = Journey::new(runner(Response::Completed("ok".into())), limits());
    let id = j.start().await;
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(&[id], None),
        start_call(arguments()),
        done(),
    ]));
    let a = agent(
        j.ext.clone(),
        j.store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    a.prompt(j.session.clone(), "wait then start")
        .await
        .unwrap()
        .done()
        .await
        .unwrap();
    let records = snapshot_tools(&j.store, j.session.as_ref().unwrap()).await;
    assert_eq!(result_value(records.last().unwrap())["status"], "started");
}

#[tokio::test]
async fn runner_faults_become_error_status_and_release_capacity() {
    for mode in 0..5 {
        let host: Runner = Arc::new(move |_| match mode {
            0 => Box::pin(async { Err(ExtensionError::Tool("HOST-PRIVATE-CANARY".into())) }),
            1 => panic!("HOST-PRIVATE-CANARY"),
            2 => Box::pin(PollPanic),
            3 => Box::pin(async { Ok(Response::Completed("x".repeat(101))) }),
            _ => Box::pin(async { Ok(Response::Completed("HOST-PRIVATE-CANARY\0".into())) }),
        });
        let mut j = Journey::new(host, limits());
        let id = j.start().await;
        let reply = j.wait(&[id]).await;
        assert_eq!(reply["tasks"][0]["status"], "error");
        assert!(reply["tasks"][0].get("output").is_none());
        assert_eq!(j.start().await.len(), 54);
    }
}

#[tokio::test]
async fn redactor_composition_protects_wait_output() {
    use crabber_extensions::tool_result_redactor::{self, Pattern, ToolResultRedactor};
    let mut j = Journey::new(
        runner(Response::Completed("SYNTHETIC_SECRET".into())),
        limits(),
    );
    let id = j.start().await;
    let redactor = Arc::new(
        ToolResultRedactor::new(tool_result_redactor::Options {
            order: 0,
            excluded_tools: vec![],
            additional_patterns: vec![Pattern {
                id: "synthetic".into(),
                expression: "SYNTHETIC_SECRET".into(),
            }],
            limits: tool_result_redactor::Limits {
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
    );
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), None),
        status_call(&id),
        done(),
    ]));
    let a = Agent::builder()
        .store(j.store.clone())
        .provider(provider.clone())
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(j.ext.clone(), Scope::Global)
        .extension(redactor, Scope::Global)
        .build()
        .unwrap();
    a.prompt(j.session.clone(), "collect")
        .await
        .unwrap()
        .done()
        .await
        .unwrap();
    let records = snapshot_tools(&j.store, j.session.as_ref().unwrap()).await;
    for record in &records[1..] {
        let text = format!("{:?}", record.result);
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("SYNTHETIC_SECRET"));
    }
    assert!(!format!("{:?}", provider.requests()).contains("SYNTHETIC_SECRET"));
    assert!(format!("{:?}", provider.requests()).contains("[REDACTED]"));
    a.close_extensions().await.unwrap();
}
