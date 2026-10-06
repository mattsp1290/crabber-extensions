use super::*;

#[tokio::test]
async fn session_scoped_mount_is_absent_from_other_sessions() {
    let registry = Registry::new();
    let session = SessionId::from("allowed-session");
    let handle = registry
        .mount(
            Arc::new(extension(Response::Dismissed)),
            Scope::Session(session.clone()),
        )
        .await
        .unwrap();
    let allowed = registry.acquire(&session);
    assert_eq!(allowed.tools.len(), 1);
    drop(allowed);
    let denied = registry.acquire(&SessionId::from("other-session"));
    assert!(denied.tools.is_empty());
    drop(denied);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_agents_receive_their_own_authoritative_identities() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let responder: Responder = {
        let seen = seen.clone();
        let barrier = barrier.clone();
        Arc::new(move |request| {
            let seen = seen.clone();
            let barrier = barrier.clone();
            Box::pin(async move {
                barrier.wait().await;
                seen.lock()
                    .unwrap()
                    .push((request.session_id().clone(), request.call_id().clone()));
                Ok(Response::Dismissed)
            })
        })
    };
    let extension = Arc::new(
        AskUser::new(Options {
            responder_identity: "host-v1".into(),
            responder,
            limits: Limits {
                max_in_flight: 2,
                ..limits()
            },
        })
        .unwrap(),
    );
    let first_store = Arc::new(MemoryStore::new());
    let second_store = Arc::new(MemoryStore::new());
    let first = Agent::builder()
        .store(first_store.clone())
        .provider(Arc::new(FakeProvider::scripted(vec![call(), done()])))
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(extension.clone(), Scope::Global)
        .build()
        .unwrap();
    let second = Agent::builder()
        .store(second_store.clone())
        .provider(Arc::new(FakeProvider::scripted(vec![call(), done()])))
        .config(agent_config())
        .policy(Arc::new(StaticPolicy::new(PermissionDecision::Allow)))
        .extension(extension, Scope::Global)
        .build()
        .unwrap();
    let (first_run, second_run) =
        tokio::join!(first.prompt(None, "one"), second.prompt(None, "two"));
    let first_run = first_run.unwrap();
    let second_run = second_run.unwrap();
    let first_session = first_run.session_id().clone();
    let second_session = second_run.session_id().clone();
    let (first_done, second_done) = tokio::join!(first_run.done(), second_run.done());
    assert_eq!(first_done.unwrap().status, RunStatus::Completed);
    assert_eq!(second_done.unwrap().status, RunStatus::Completed);
    let expected = [
        (
            first_session.clone(),
            snapshot_tool(&first_store, &first_session).await.id,
        ),
        (
            second_session.clone(),
            snapshot_tool(&second_store, &second_session).await.id,
        ),
    ];
    {
        let identities = seen.lock().unwrap();
        assert_eq!(identities.len(), 2);
        assert!(
            identities
                .iter()
                .all(|identity| expected.contains(identity))
        );
    }
    first.close_extensions().await.unwrap();
    second.close_extensions().await.unwrap();
}
