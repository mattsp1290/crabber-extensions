use super::*;

#[test]
fn configuration_bounds_and_hash_cover_every_limit_and_identity() {
    let base = extension(Response::Completed(String::new())).config_hash();
    assert_eq!(
        base,
        extension(Response::Failed("different runner".into())).config_hash()
    );
    for field in 0..6 {
        for over in [false, true] {
            let mut l = limits();
            match field {
                0 => l.max_task_bytes = if over { 64 * 1024 + 1 } else { 0 },
                1 => l.max_profile_bytes = if over { 257 } else { 0 },
                2 => l.max_result_bytes = if over { 256 * 1024 + 1 } else { 0 },
                3 => l.max_in_flight = if over { 257 } else { 0 },
                4 => {
                    l.max_wait = if over {
                        Duration::from_secs(3601)
                    } else {
                        Duration::ZERO
                    }
                }
                _ => {
                    l.shutdown_grace = if over {
                        Duration::from_secs(301)
                    } else {
                        Duration::ZERO
                    }
                }
            }
            let err = DelegateTask::new(Options {
                runner_identity: "host-v1".into(),
                runner: runner(Response::Completed(String::new())),
                limits: l,
            })
            .err()
            .unwrap();
            assert_eq!(
                err.to_string(),
                "extension plan failed: extension configuration invalid: delegate-task-limits"
            );
        }
        let mut l = limits();
        match field {
            0 => l.max_task_bytes += 1,
            1 => l.max_profile_bytes += 1,
            2 => l.max_result_bytes += 1,
            3 => l.max_in_flight += 1,
            4 => l.max_wait += Duration::from_secs(1),
            _ => l.shutdown_grace += Duration::from_secs(1),
        }
        assert_ne!(
            base,
            configured(runner(Response::Completed(String::new())), l).config_hash()
        );
    }
    for identity in ["", " ", "x\n", &"x".repeat(257)] {
        let err = DelegateTask::new(Options {
            runner_identity: identity.into(),
            runner: runner(Response::Completed(String::new())),
            limits: limits(),
        })
        .err()
        .unwrap();
        assert!(err.to_string().ends_with("delegate-task-policy"));
    }
    let other = DelegateTask::new(Options {
        runner_identity: "host-v2".into(),
        runner: runner(Response::Completed(String::new())),
        limits: limits(),
    })
    .unwrap();
    assert_ne!(base, other.config_hash());
}

#[tokio::test]
async fn registered_tool_has_exact_contract_metadata_and_session_scope() {
    let registry = Registry::new();
    let session = SessionId::from("session");
    let handle = registry
        .mount(
            Arc::new(extension(Response::Completed(String::new()))),
            Scope::Session(session.clone()),
        )
        .await
        .unwrap();
    let plan = registry.acquire(&session);
    let info = &plan.tools[0].info;
    assert_eq!(info.name, TOOL_NAME);
    assert_eq!(
        info.description,
        "Delegate one bounded task under an opaque host profile. Returns completed, failed, rejected, unavailable, or timed_out."
    );
    assert!(!info.retry_safe);
    assert_eq!(info.required_permissions, [PERMISSION_DELEGATE]);
    assert_eq!(
        info.parameters,
        json!({"type":"object","additionalProperties":false,"required":["task","profile"],"properties":{"task":{"type":"string"},"profile":{"type":"string"}}})
    );
    assert!(registry.acquire(&SessionId::from("other")).tools.is_empty());
    drop(plan);
    handle.close().await.unwrap();
}
