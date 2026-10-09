use super::*;
#[tokio::test(flavor = "multi_thread")]
async fn registered_contract_is_exact() {
    let h = Harness::new(|_| {}).await;
    let plan = h.registry.acquire(&SessionId::from("session"));
    assert_eq!(plan.tools.len(), 4);
    for (name, permission, retry) in [
        (START_TOOL, PERMISSION_START, false),
        (STATUS_TOOL, PERMISSION_READ, true),
        (LIST_TOOL, PERMISSION_READ, true),
        (KILL_TOOL, PERMISSION_KILL, false),
    ] {
        let tool = plan.tools.iter().find(|t| t.info.name == name).unwrap();
        assert_eq!(tool.info.retry_safe, retry);
        assert_eq!(tool.info.required_permissions, vec![permission.to_string()]);
        assert_eq!(tool.info.parameters["additionalProperties"], false);
        assert_eq!(tool.info.parameters["type"], "object");
        assert!(tool.executor.execute(json!({})).await.is_err());
    }
    assert_eq!(h.ext.id(), "crabber-extensions/background-jobs");
    assert_eq!(h.ext.version(), env!("CARGO_PKG_VERSION"));
    assert_eq!(h.ext.config_hash().len(), 64);
    drop(plan);
    h.close().await;
}
#[test]
fn worst_case_helpers_cover_caps_and_saturate_safely() {
    let mut l = limits();
    l.max_tracked = 256;
    l.max_output_bytes_per_stream = 1048576;
    assert_eq!(l.worst_case_retained_bytes(), 512 * 1024 * 1024);
    assert!(l.worst_case_status_bytes() >= 12 * 1048576);
    assert!(l.worst_case_list_bytes() >= 256 * 200);
    l.max_tracked = usize::MAX;
    l.max_output_bytes_per_stream = usize::MAX;
    assert_eq!(l.worst_case_retained_bytes(), usize::MAX);
    assert_eq!(l.worst_case_status_bytes(), usize::MAX);
    assert_eq!(l.worst_case_list_bytes(), usize::MAX);
}
