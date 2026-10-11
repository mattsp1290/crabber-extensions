//! Process-local owner registry and background tool lifecycle.
use super::*;
use std::{
    collections::{BTreeMap, hash_map::RandomState},
    hash::{BuildHasher, Hasher},
    sync::{MutexGuard, PoisonError},
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
#[derive(PartialEq, Eq)]
struct Owner {
    session: SessionId,
    workspace: String,
}
impl Owner {
    fn from_context(context: &ToolContext) -> Result<Self, ExtensionError> {
        if context.cancel.is_cancelled() {
            return Err(failure("cancelled"));
        }
        Ok(Self {
            session: context.session_id.clone(),
            workspace: context
                .workspace()
                .workspace_id()
                .ok_or_else(|| failure("owner"))?
                .into(),
        })
    }
}
// Terminal states are single assignment; only Running may transition.
enum State {
    Running,
    Completed(String),
    Failed(String),
    Rejected(String),
    Unavailable(String),
    TimedOut,
    Cancelled,
    Error,
}
impl State {
    fn reply(reply: Result<Response, ExtensionError>, limits: &Limits) -> Self {
        let Ok(response) = reply else {
            return Self::Error;
        };
        let output = match &response {
            Response::Completed(out)
            | Response::Failed(out)
            | Response::Rejected(out)
            | Response::Unavailable(out) => out,
        };
        if output.len() > limits.max_result_bytes || output.contains('\0') {
            return Self::Error;
        }
        match response {
            Response::Completed(out) => Self::Completed(out),
            Response::Failed(out) => Self::Failed(out),
            Response::Rejected(out) => Self::Rejected(out),
            Response::Unavailable(out) => Self::Unavailable(out),
        }
    }
    fn status_output(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Running => ("running", None),
            Self::Completed(out) => ("completed", Some(out)),
            Self::Failed(out) => ("failed", Some(out)),
            Self::Rejected(out) => ("rejected", Some(out)),
            Self::Unavailable(out) => ("unavailable", Some(out)),
            Self::TimedOut => ("timed_out", None),
            Self::Cancelled => ("cancelled", None),
            Self::Error => ("error", None),
        }
    }
}
struct TaskRecord {
    id: String,
    owner: Owner,
    state: Mutex<State>,
    token: CancellationToken,
}
impl TaskRecord {
    fn row(&self, output: bool) -> Value {
        let mut unlimited = usize::MAX;
        self.budgeted_row(if output { Some(&mut unlimited) } else { None })
    }
    fn budgeted_row(&self, budget: Option<&mut usize>) -> Value {
        let state = lock(&self.state);
        let (status, text) = state.status_output();
        let mut row = json!({"task_id":self.id,"status":status});
        if let (Some(text), Some(remaining)) = (text.filter(|s| !s.is_empty()), budget) {
            if text.len() > *remaining {
                *remaining = 0;
                row["output_omitted"] = true.into();
            } else {
                *remaining -= text.len();
                row["output"] = text.into();
            }
        }
        row
    }
    fn terminal(&self) -> bool {
        !matches!(*lock(&self.state), State::Running)
    }
}
// Own the permit through every exit, including unexpected task unwinding.
// Host futures are declared after this guard and drop before it on unwinding.
struct RunnerLifecycle {
    policy: Arc<Policy>,
    record: Arc<TaskRecord>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl RunnerLifecycle {
    fn finish(mut self, value: State) {
        drop(self.permit.take());
        self.policy.settle(&self.record, value);
    }
}
impl Drop for RunnerLifecycle {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.policy.settle(&self.record, State::Error);
    }
}
pub(super) struct Tasks {
    records: BTreeMap<String, Arc<TaskRecord>>,
    counter: u64,
    epoch: String,
    closing: bool,
}
impl Tasks {
    pub(super) fn new() -> Self {
        let epoch = (0..2)
            .map(|_| {
                let mut hash = RandomState::new().build_hasher();
                hash.write(b"delegate-task-epoch");
                format!(
                    "{:0width$x}",
                    hash.finish(),
                    width = TASK_ID_EPOCH_BYTES / 2
                )
            })
            .collect();
        Self {
            records: BTreeMap::new(),
            counter: 0,
            epoch,
            closing: false,
        }
    }
    fn find(&self, owner: &Owner, id: &str) -> Result<Arc<TaskRecord>, ExtensionError> {
        self.records
            .get(id)
            .filter(|r| &r.owner == owner)
            .cloned()
            .ok_or_else(|| invalid("task_id"))
    }
}
impl Policy {
    fn settle(&self, record: &TaskRecord, value: State) {
        let mut state = lock(&record.state);
        if matches!(*state, State::Running) {
            *state = value;
            drop(state);
            self.settled.send_modify(|n| *n = n.wrapping_add(1));
        }
    }
    pub(super) fn close_tasks(&self) {
        let records = {
            let mut tasks = lock(&self.tasks);
            tasks.closing = true;
            tasks
                .records
                .values()
                .filter(|r| !r.terminal())
                .cloned()
                .collect::<Vec<_>>()
        };
        for record in records {
            self.settle(&record, State::Cancelled);
            record.token.cancel();
        }
    }
    fn start(
        self: &Arc<Self>,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        if context.cancel.is_cancelled() {
            return Err(failure("cancelled"));
        }
        let (task, profile) = input::normalize(arguments, &self.limits)?;
        let owner = Owner::from_context(&context)?;
        let Ok(permit) = self.capacity.clone().try_acquire_owned() else {
            return Ok(json!({"status":"unavailable"}));
        };
        let mut tasks = lock(&self.tasks);
        if tasks.closing {
            return Ok(json!({"status":"unavailable"}));
        }
        let counter = tasks
            .counter
            .checked_add(1)
            .ok_or_else(|| failure("identity-exhausted"))?;
        while tasks.records.len() >= self.limits.max_tracked {
            let own = tasks
                .records
                .values()
                .find(|r| r.owner == owner && r.terminal());
            let candidate = own
                .or_else(|| tasks.records.values().find(|r| r.terminal()))
                .map(|r| r.id.clone());
            let Some(id) = candidate else {
                return Ok(json!({"status":"unavailable"}));
            };
            tasks.records.remove(&id);
        }
        tasks.counter = counter;
        let id = format!(
            "{TASK_ID_PREFIX}{}_{counter:0width$x}",
            tasks.epoch,
            width = TASK_ID_COUNTER_BYTES
        );
        let token = CancellationToken::new();
        let record = Arc::new(TaskRecord {
            id: id.clone(),
            owner,
            state: Mutex::new(State::Running),
            token: token.clone(),
        });
        let request = Request {
            session_id: context.session_id.clone(),
            run_id: context.run_id.clone(),
            call_id: context.call_id.clone(),
            workspace: context.workspace().clone(),
            task,
            profile,
            cancellation: token,
            max_wait: self.limits.max_background_duration,
            background: true,
        };
        tasks.records.insert(id.clone(), record.clone());
        let policy = self.clone();
        let lifecycle = RunnerLifecycle {
            policy: policy.clone(),
            record: record.clone(),
            permit: Some(permit),
        };
        self.cleanup.tracker().spawn(async move {
            let lifecycle = lifecycle;
            if record.token.is_cancelled() {
                lifecycle.finish(State::Cancelled);
                return;
            }
            let reply = match catch_unwind(AssertUnwindSafe(|| (policy.runner)(request))) {
                Err(_) => Err(failure("runner")),
                Ok(host) => {
                    let mut safe = SafeFuture::new(host, || failure("runner"));
                    let reply = {
                        let wait = safe.wait();
                        tokio::pin!(wait);
                        tokio::select! { biased;
                            reply = &mut wait => reply,
                            _ = tokio::time::sleep(policy.limits.max_background_duration) => {
                                policy.settle(&record, State::TimedOut);
                                record.token.cancel();
                                wait.await
                            }
                        }
                    };
                    drop(safe);
                    reply
                }
            };
            let value = State::reply(reply, &policy.limits);
            lifecycle.finish(value);
        });
        Ok(json!({"status":"started","task_id":id}))
    }
    async fn wait_tasks(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        let owner = Owner::from_context(&context)?;
        let (ids, timeout) = input::wait(arguments, &self.limits)?;
        let records = {
            let tasks = lock(&self.tasks);
            ids.iter()
                .map(|id| tasks.find(&owner, id))
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut settled = self.settled.subscribe();
        settled.borrow_and_update();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            if records.iter().all(|r| r.terminal()) {
                break;
            }
            tokio::select! { biased;
                _ = context.cancel.cancelled() => return Err(failure("cancelled")),
                _ = &mut deadline => break,
                _ = settled.changed() => {}
            }
        }
        let mut remaining = self.limits.max_wait_reply_bytes;
        let rows: Vec<_> = records
            .iter()
            .map(|r| r.budgeted_row(Some(&mut remaining)))
            .collect();
        let done = rows.iter().all(|r| r["status"] != "running");
        Ok(json!({"status":if done {"done"} else {"timed_out"},"tasks":rows}))
    }
}
struct Executor {
    policy: Arc<Policy>,
    name: &'static str,
}
#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(&self, _: Value) -> Result<Value, ExtensionError> {
        Err(failure("context"))
    }
    async fn execute_with_context(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        match self.name {
            START_TOOL => self.policy.start(context, arguments),
            WAIT_TOOL => self.policy.wait_tasks(context, arguments).await,
            _ => {
                let owner = Owner::from_context(&context)?;
                let tasks = lock(&self.policy.tasks);
                if self.name == STATUS_TOOL {
                    return Ok(tasks.find(&owner, input::task_id(&arguments)?)?.row(true));
                }
                if arguments != json!({}) {
                    return Err(invalid("shape"));
                }
                Ok(
                    json!({"tasks":tasks.records.values().filter(|r| r.owner == owner).map(|r| r.row(false)).collect::<Vec<_>>()}),
                )
            }
        }
    }
}
pub(super) fn install(policy: &Arc<Policy>, registrar: &mut Registrar) {
    let statuses = "running, completed, failed, rejected, unavailable, timed_out, cancelled, error";
    for (name, description, parameters) in [
        (
            START_TOOL,
            "Start a background task; returns started with task_id or unavailable. Collect output with delegate_task_wait or delegate_task_status.".to_owned(),
            json!({"type":"object","additionalProperties":false,"required":["task","profile"],"properties":{"task":{"type":"string"},"profile":{"type":"string"}}}),
        ),
        (
            STATUS_TOOL,
            format!("Read retained output repeatedly until pruned. Statuses: {statuses}."),
            json!({"type":"object","additionalProperties":false,"required":["task_id"],"properties":{"task_id":{"type":"string"}}}),
        ),
        (
            LIST_TOOL,
            format!("List owned retained tasks by ID. Statuses: {statuses}."),
            json!({"type":"object","additionalProperties":false,"properties":{}}),
        ),
        (
            WAIT_TOOL,
            format!(
                "Wait for 1..{} unique retained task IDs; explicit timeout_seconds is 1..{} whole seconds. Returns done or timed_out. Task statuses: {statuses}. Outputs persist until pruned. Read output_omitted rows individually with delegate_task_status.", policy.limits.max_wait_ids, policy.limits.max_wait.as_secs()
            ),
            json!({"type":"object","additionalProperties":false,"required":["task_ids"],"properties":{"task_ids":{"type":"array","minItems":1,"items":{"type":"string"}},"timeout_seconds":{"type":"integer","minimum":1}}}),
        ),
    ] {
        registrar.tool(Arc::new(ToolDefinition {
            info: ToolInfo {
                name: name.into(),
                description,
                parameters,
                retry_safe: name != START_TOOL,
                required_permissions: vec![
                    if name == START_TOOL {
                        PERMISSION_DELEGATE
                    } else {
                        PERMISSION_DELEGATE_READ
                    }
                    .into(),
                ],
            },
            executor: Arc::new(Executor {
                policy: policy.clone(),
                name,
            }),
        }));
    }
}
