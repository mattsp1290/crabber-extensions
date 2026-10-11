use super::*;
use crabber_extensions::delegate_task::{LIST_TOOL, START_TOOL, STATUS_TOOL, WAIT_TOOL};

#[tokio::test]
async fn start_then_wait_returns_durable_output_and_reenters_the_model() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner: Runner = {
        let seen = seen.clone();
        Arc::new(move |request| {
            seen.lock().unwrap().push(request);
            Box::pin(async { Ok(Response::Completed("synthetic result".into())) })
        })
    };
    let ext = Arc::new(configured(runner, limits()));
    let store = Arc::new(MemoryStore::new());
    let provider = Arc::new(FakeProvider::scripted(vec![
        start_call(arguments()),
        done(),
    ]));
    let first = agent(
        ext.clone(),
        store.clone(),
        provider,
        PermissionDecision::Allow,
    );
    let run = first.prompt(None, "start").await.unwrap();

    let session = run.done().await.unwrap().session_id;
    let call = snapshot_tool(&store, &session).await;
    let id = read_task_id(&call);
    assert_eq!(id.len(), 54);
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.background());
    assert_eq!(request.session_id(), &session);
    assert_eq!(request.call_id(), &call.id);
    assert_eq!(request.max_wait(), limits().max_background_duration);
    let provider = Arc::new(FakeProvider::scripted(vec![
        wait_call(std::slice::from_ref(&id), Some(2)),
        done(),
    ]));
    let second = agent(
        ext,
        store.clone(),
        provider.clone(),
        PermissionDecision::Allow,
    );
    let run = second.prompt(Some(session.clone()), "wait").await.unwrap();

    assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
    let messages = store.list_all_messages(&session).await.unwrap();
    assert!(format!("{messages:?}").contains("synthetic result"));
}

#[tokio::test]
async fn wait_input_validation() {
    let (_, handle, tools) = mounted(Arc::new(extension(Response::Completed(String::new())))).await;
    let id = "task_00000000000000000000000000000000_0000000000000001";
    for args in [
        json!({"task_ids":[]}),
        json!({"task_ids":[id,id]}),
        json!({"task_ids":[id],"timeout_seconds":null}),
        json!({"task_ids":[id],"timeout_seconds":0}),
        json!({"task_ids":[id],"timeout_seconds":6}),
        json!({"task_ids":["bad"]}),
        json!({"task_ids":[id],"unknown":1}),
        json!({"task_ids":[id]}),
    ] {
        assert!(
            tools[WAIT_TOOL]
                .execute_with_context(context(CancellationToken::new()), args)
                .await
                .unwrap_err()
                .to_string()
                .contains("input invalid")
        );
    }
    handle.close().await.unwrap();
}

#[tokio::test]
async fn status_and_list_report_running_then_terminal() {
    let mut host = PendingHost::new(false);
    let (_, handle, tools) = mounted(Arc::new(configured(host.runner.clone(), limits()))).await;
    let value = tools[START_TOOL]
        .execute_with_context(context(CancellationToken::new()), arguments())
        .await
        .unwrap();
    let id = value["task_id"].as_str().unwrap();
    host.entered(1).await;
    let status = || {
        tools[STATUS_TOOL]
            .execute_with_context(context(CancellationToken::new()), json!({"task_id":id}))
    };
    assert_eq!(status().await.unwrap()["status"], "running");
    assert_eq!(
        tools[LIST_TOOL]
            .execute_with_context(context(CancellationToken::new()), json!({}))
            .await
            .unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    host.release.notify_one();
    let value = tools[WAIT_TOOL]
        .execute_with_context(context(CancellationToken::new()), json!({"task_ids":[id]}))
        .await
        .unwrap();
    assert_eq!(value["status"], "done");
    assert_eq!(status().await.unwrap()["output"], "released");
    handle.close().await.unwrap();
}
