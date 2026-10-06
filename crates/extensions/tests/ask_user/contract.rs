use super::*;

#[test]
fn validates_limits_identity_and_hashes_only_policy() {
    let callback: Responder = Arc::new(|_| Box::pin(async { Ok(Response::Dismissed) }));
    let first = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder: callback.clone(),
        limits: limits(),
    })
    .unwrap();
    let mut changed = limits();
    changed.max_wait = Duration::from_secs(4);
    let second = AskUser::new(Options {
        responder_identity: "host-v1".into(),
        responder: callback.clone(),
        limits: changed,
    })
    .unwrap();
    assert_ne!(first.config_hash(), second.config_hash());
    for bad_identity in ["", " ", "x\n", &"x".repeat(257)] {
        assert!(
            AskUser::new(Options {
                responder_identity: bad_identity.into(),
                responder: callback.clone(),
                limits: limits()
            })
            .is_err()
        );
    }
    let mut bad = limits();
    bad.max_in_flight = 0;
    assert!(
        AskUser::new(Options {
            responder_identity: "host-v1".into(),
            responder: callback,
            limits: bad
        })
        .is_err()
    );
}

#[test]
fn rejects_each_limit_at_zero_and_above_its_cap() {
    let callback: Responder = Arc::new(|_| Box::pin(async { Ok(Response::Dismissed) }));
    let invalid = [
        (0, 50, 50, 100, 1, Duration::from_secs(1)),
        (100, 0, 50, 100, 1, Duration::from_secs(1)),
        (100, 50, 0, 100, 1, Duration::from_secs(1)),
        (100, 50, 50, 0, 1, Duration::from_secs(1)),
        (100, 50, 50, 100, 0, Duration::from_secs(1)),
        (100, 50, 50, 100, 1, Duration::ZERO),
        (16 * 1024 + 1, 50, 50, 100, 1, Duration::from_secs(1)),
        (100, 1025, 50, 100, 1, Duration::from_secs(1)),
        (100, 50, 4 * 1024 + 1, 100, 1, Duration::from_secs(1)),
        (100, 50, 50, 16 * 1024 + 1, 1, Duration::from_secs(1)),
        (100, 50, 50, 100, 257, Duration::from_secs(1)),
        (100, 50, 50, 100, 1, Duration::from_secs(601)),
    ];
    for (question, label, description, custom, in_flight, wait) in invalid {
        assert!(
            AskUser::new(Options {
                responder_identity: "host-v1".into(),
                responder: callback.clone(),
                limits: Limits {
                    max_question_bytes: question,
                    max_option_label_bytes: label,
                    max_option_description_bytes: description,
                    max_custom_answer_bytes: custom,
                    max_in_flight: in_flight,
                    max_wait: wait
                },
            })
            .is_err()
        );
    }
}

#[tokio::test]
async fn registered_tool_has_contract_metadata() {
    let registry = Registry::new();
    let handle = registry
        .mount(Arc::new(extension(Response::Dismissed)), Scope::Global)
        .await
        .unwrap();
    let plan = registry.acquire(&SessionId::from("session"));
    let info = &plan.tools[0].info;
    assert_eq!(info.name, TOOL_NAME);
    assert!(!info.retry_safe);
    assert_eq!(info.required_permissions, [PERMISSION_ASK]);
    assert_eq!(info.parameters["required"], json!(["question", "options"]));
    assert_eq!(info.parameters["additionalProperties"], false);
    assert_eq!(info.parameters["properties"]["options"]["minItems"], 2);
    assert_eq!(info.parameters["properties"]["options"]["maxItems"], 5);
    drop(plan);
    handle.close().await.unwrap();
}
