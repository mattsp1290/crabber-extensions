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
    assert_eq!(id.len(), crabber_extensions::delegate_task::TASK_ID_BYTES);
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.background());
    assert_eq!(request.session_id(), &session);
    assert_eq!(request.call_id(), &call.id);
    assert_eq!(request.deadline(), limits().max_background_duration);
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
    let records = snapshot_tools(&store, &session).await;
    let wait = records.iter().find(|r| r.name == WAIT_TOOL).unwrap();
    assert_eq!(
        result_value(wait),
        json!({"status":"done","tasks":[{"task_id":id,"status":"completed","output":"synthetic result"}]})
    );
    assert!(format!("{:?}", provider.requests()[1]).contains("synthetic result"));
    assert_eq!(request.run_id(), &call.run_id);
    assert_eq!(request.workspace().workspace_id(), Some("workspace"));
    assert_eq!(request.workspace().directory(), Some("/workspace"));
    first.close_extensions().await.unwrap();
    second.close_extensions().await.unwrap();
}

#[tokio::test]
async fn status_and_list_report_running_then_terminal() {
    let mut host = PendingHost::new(false);
    let mut j = Journey::new(host.runner.clone(), limits());
    let id = j.start().await;
    host.entered(1).await;
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["status"],
        "running"
    );
    assert_eq!(
        result_value(&j.call(LIST_TOOL, json!({})).await)["tasks"],
        json!([{"task_id":id,"status":"running"}])
    );
    host.release.notify_one();
    j.wait(std::slice::from_ref(&id)).await;
    assert_eq!(
        result_value(&j.call(STATUS_TOOL, json!({"task_id":id})).await)["output"],
        "released"
    );
    assert_eq!(
        result_value(&j.call(LIST_TOOL, json!({})).await)["tasks"],
        json!([{"task_id":id,"status":"completed"}])
    );
}

// Every operation in this harness runs through Crabber and durable settlement.
pub(super) struct Journey {
    pub(super) ext: Arc<DelegateTask>,
    pub(super) store: Arc<MemoryStore>,
    pub(super) session: Option<SessionId>,
}
impl Journey {
    pub(super) fn new(runner: Runner, limits: Limits) -> Self {
        Self {
            ext: Arc::new(configured(runner, limits)),
            store: Arc::new(MemoryStore::new()),
            session: None,
        }
    }
    pub(super) fn other_owner(&self) -> Self {
        Self {
            ext: self.ext.clone(),
            store: Arc::new(MemoryStore::new()),
            session: None,
        }
    }
    pub(super) async fn call(&mut self, name: &str, args: Value) -> crabber::core::ToolCallRecord {
        let provider = Arc::new(FakeProvider::scripted(vec![tool_call(name, args), done()]));
        let a = agent(
            self.ext.clone(),
            self.store.clone(),
            provider.clone(),
            PermissionDecision::Allow,
        );
        let run = a
            .prompt(self.session.clone(), "synthetic journey")
            .await
            .unwrap();
        self.session = Some(run.session_id().clone());
        assert_eq!(run.done().await.unwrap().status, RunStatus::Completed);
        let records = snapshot_tools(&self.store, self.session.as_ref().unwrap()).await;
        let record = records.last().unwrap().clone();
        let text = format!("{:?}", record.result.as_ref().unwrap().content);
        assert!(!text.contains("HOST-PRIVATE-CANARY"));
        assert!(!format!("{:?}", provider.requests()).contains("HOST-PRIVATE-CANARY"));
        record
    }
    pub(super) async fn start(&mut self) -> String {
        read_task_id(&self.call(START_TOOL, arguments()).await)
    }
    pub(super) async fn wait(&mut self, ids: &[String]) -> Value {
        result_value(&self.call(WAIT_TOOL, json!({"task_ids":ids})).await)
    }
}

struct PollPanic;
impl Future for PollPanic {
    type Output = Result<Response, ExtensionError>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        panic!("HOST-PRIVATE-CANARY")
    }
}

#[path = "background/contract.rs"]
mod contract;

#[path = "background/output.rs"]
mod output;

#[path = "background/wait.rs"]
mod wait;
