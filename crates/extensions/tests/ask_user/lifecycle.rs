use super::*;

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

#[tokio::test(start_paused = true)]
async fn pending_ask_makes_mount_close_time_out_then_closes_after_cancellation() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let responder: Responder = {
        let entered = entered.clone();
        Arc::new(move |_| {
            entered.notify_one();
            Box::pin(async { std::future::pending::<Result<Response, ExtensionError>>().await })
        })
    };
    let registry = Registry::new().with_close_timeout(Duration::from_secs(1));
    let handle = registry
        .mount(
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
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let tool = plan.tools[0].executor.clone();
    let cancel = CancellationToken::new();
    let waiting = entered.notified();
    tokio::pin!(waiting);
    let child_cancel = cancel.clone();
    let pending = tokio::spawn(async move {
        tool.execute_with_context(context(child_cancel), arguments())
            .await
    });
    waiting.await;
    let close = handle.close();
    tokio::pin!(close);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        close.await,
        Err(ExtensionError::MountCloseTimeout { .. })
    ));
    cancel.cancel();
    assert_eq!(
        pending.await.unwrap().unwrap_err().to_string(),
        "tool execution failed: ask user failed: cancelled"
    );
    drop(plan);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn runtime_interrupt_drops_responder_and_releases_agent_close() {
    use tokio::sync::Notify;
    struct DropFlag(Arc<AtomicUsize>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let responder: Responder = {
        let entered = entered.clone();
        let dropped = dropped.clone();
        Arc::new(move |_| {
            entered.notify_one();
            let flag = DropFlag(dropped.clone());
            Box::pin(async move {
                let _flag = flag;
                std::future::pending::<Result<Response, ExtensionError>>().await
            })
        })
    };
    let agent = Arc::new(
        Agent::builder()
            .memory()
            .provider(Arc::new(FakeProvider::scripted(vec![call(), done()])))
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
            .unwrap(),
    );
    let waiting = entered.notified();
    tokio::pin!(waiting);
    let run = agent.prompt(None, "fixture").await.unwrap();
    waiting.await;
    run.interrupt();
    assert_eq!(run.done().await.unwrap().status, RunStatus::Interrupted);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    agent.close_extensions().await.unwrap();
}
