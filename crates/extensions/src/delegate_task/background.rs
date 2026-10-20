//! Process-local owner registry and background tool lifecycle.
use super::*;
use crabber::JsonToolExecutor;
use crabber::core::{InputFingerprint, SessionOwnerId, SessionOwnerOperationId};
use crabber::extension::{
    ResourceFailure, SessionInvocationAdmission, SessionInvocationContext, SessionInvocationError,
    SessionInvocationRequest,
};
mod lifecycle;
use lifecycle::RunnerLifecycle;
pub(super) const COMPONENT_ID: &str = "crabber-extensions/delegate-task.background";
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
    Admitted,
    Running,
    Finished(Response),
    TimedOut,
    Cancelled,
    Error,
}
impl State {
    fn reply(reply: Result<Response, ExtensionError>, limits: &Limits) -> Self {
        reply
            .and_then(|response| response.checked(limits))
            .map_or(Self::Error, Self::Finished)
    }
    fn status_output(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Admitted | Self::Running => ("running", None),
            Self::Finished(response) => {
                let (status, output) = response.status_output();
                (status, Some(output))
            }
            Self::TimedOut => ("timed_out", None),
            Self::Cancelled => ("cancelled", None),
            Self::Error => ("error", None),
        }
    }
}
enum Output<'a> {
    Hidden,
    Full,
    Budgeted(&'a mut usize),
}
struct TaskRecord {
    id: String,
    owner: Owner,
    state: Mutex<State>,
    token: CancellationToken,
    invocation: SessionInvocationRequest,
}
impl TaskRecord {
    fn row(&self, output: Output<'_>) -> (Value, bool) {
        let state = lock(&self.state);
        let (status, text) = state.status_output();
        let mut row = json!({"task_id":self.id,"status":status,"detached":true});
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            match output {
                Output::Hidden => {}
                Output::Full => row["output"] = text.into(),
                Output::Budgeted(remaining) if text.len() <= *remaining => {
                    *remaining -= text.len();
                    row["output"] = text.into();
                }
                Output::Budgeted(remaining) => {
                    *remaining = 0;
                    row["output_omitted"] = true.into();
                }
            }
        }
        (row, !matches!(*state, State::Admitted | State::Running))
    }
    fn terminal(&self) -> bool {
        !matches!(*lock(&self.state), State::Admitted | State::Running)
    }
}
// Std-only randomness supplies process-local uniqueness without a dependency.
// Ownership checks, rather than ID secrecy, gate access.
fn process_epoch() -> String {
    (0..2)
        .map(|_| {
            let mut hash = RandomState::new().build_hasher();
            hash.write(b"delegate-task-epoch");
            format!(
                "{:0width$x}",
                hash.finish(),
                width = TASK_ID_EPOCH_BYTES / 2
            )
        })
        .collect()
}
pub(super) struct Tasks {
    records: BTreeMap<String, Arc<TaskRecord>>,
    counter: u64,
    epoch: String,
    closing: bool,
    unresolved: Vec<(SessionInvocationRequest, tokio::sync::OwnedSemaphorePermit)>,
}
impl Tasks {
    pub(super) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            counter: 0,
            epoch: process_epoch(),
            closing: false,
            unresolved: Vec::new(),
        }
    }
    // Refusal consumes neither an ID nor a record; keep checks before mutation.
    fn admit(
        &mut self,
        owner: Owner,
        max_tracked: usize,
        invocation: SessionInvocationRequest,
    ) -> Result<Option<Arc<TaskRecord>>, ExtensionError> {
        if self.closing {
            return Ok(None);
        }
        let counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| failure("identity-exhausted"))?;
        if self.records.len() >= max_tracked {
            let own = self
                .records
                .values()
                .find(|r| r.owner == owner && r.terminal());
            let candidate = own
                .or_else(|| self.records.values().find(|r| r.terminal()))
                .map(|r| r.id.clone());
            let Some(id) = candidate else { return Ok(None) };
            self.records.remove(&id);
        }
        self.counter = counter;
        let record = Arc::new(TaskRecord {
            id: format!(
                "{TASK_ID_PREFIX}{}_{counter:0width$x}",
                self.epoch,
                width = TASK_ID_COUNTER_BYTES
            ),
            owner,
            state: Mutex::new(State::Admitted),
            invocation,
            token: CancellationToken::new(),
        });
        self.records.insert(record.id.clone(), record.clone());
        Ok(Some(record))
    }
    pub(super) fn unresolved_count(&self) -> usize {
        self.unresolved.len()
    }
    pub(super) fn counts(&self) -> (usize, bool) {
        (self.records.len(), self.closing)
    }
    fn close(&mut self) -> Vec<Arc<TaskRecord>> {
        self.closing = true;
        self.records
            .values()
            .filter(|r| !r.terminal())
            .cloned()
            .collect()
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
        if matches!(*state, State::Admitted | State::Running) {
            *state = value;
            drop(state);
            self.settled.send_modify(|n| *n = n.wrapping_add(1));
        }
    }
    pub(super) fn close_tasks(&self) {
        let records = lock(&self.tasks).close();
        for record in records {
            self.settle(&record, State::Cancelled);
            record.token.cancel();
        }
    }
    async fn start(
        self: &Arc<Self>,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        if context.cancel.is_cancelled() {
            return Err(failure("cancelled"));
        }
        let (task, profile) = input::normalize(arguments, &self.limits)?;
        let owner = Owner::from_context(&context)?;
        // Reserve reconciliation space before registration, including caller-drop
        // uncertainty. Every unknown acknowledgement keeps its exact request.
        let Ok(slot) = self.reconciliation.clone().try_acquire_owned() else {
            return Ok(json!({"status":"unavailable"}));
        };
        let shared_permit = match self.shared.as_ref().map(|pool| pool.acquire()).transpose() {
            Ok(permit) => permit,
            Err(()) => return Ok(json!({"status":"unavailable"})),
        };
        let Ok(permit) = self.capacity.clone().try_acquire_owned() else {
            return Ok(json!({"status":"unavailable"}));
        };
        let invocation = invocation_request(&context, &self.hash)?;
        let record = {
            let mut tasks = lock(&self.tasks);
            let Some(record) = tasks.admit(owner, self.limits.max_tracked, invocation.clone())?
            else {
                return Ok(json!({"status":"unavailable"}));
            };
            record
        };
        let mut registration = Registration {
            policy: self.clone(),
            request: invocation.clone(),
            slot: Some(slot),
        };
        let id = record.id.clone();
        let request = Request::new(
            &context,
            (task, profile),
            record.token.child_token(),
            self.limits.max_background_duration,
            true,
        );
        let lifecycle = RunnerLifecycle::new(self.clone(), record, permit, shared_permit);
        let admitted = context
            .session_invocations()
            .spawn(invocation.clone(), move |inv| lifecycle.run(inv, request))
            .await;
        if !matches!(admitted, Err(SessionInvocationError::UnknownOutcome)) {
            registration.known();
        }
        match admitted {
            Ok(SessionInvocationAdmission::Started) => Ok(json!({"status":"started","task_id":id})),
            outcome => {
                let existing = {
                    let mut tasks = lock(&self.tasks);
                    tasks.records.remove(&id);
                    tasks
                        .records
                        .values()
                        .find(|r| r.invocation.id == invocation.id)
                        .map(|r| r.id.clone())
                };
                match outcome {
                    Ok(SessionInvocationAdmission::Replayed) => existing
                        .map(|id| json!({"status":"started","task_id":id}))
                        .ok_or_else(|| failure("session-jobs")),
                    Err(SessionInvocationError::Capacity | SessionInvocationError::Rejected) => {
                        Ok(json!({"status":"unavailable"}))
                    }
                    _ => Err(failure("session-jobs")),
                }
            }
        }
    }
    fn status(&self, context: &ToolContext, arguments: &Value) -> Result<Value, ExtensionError> {
        let owner = Owner::from_context(context)?;
        Ok(lock(&self.tasks)
            .find(&owner, input::task_id(arguments)?)?
            .row(Output::Full)
            .0)
    }
    fn list(&self, context: &ToolContext, arguments: &Value) -> Result<Value, ExtensionError> {
        let owner = Owner::from_context(context)?;
        if *arguments != json!({}) {
            return Err(invalid("shape"));
        }
        let tasks = lock(&self.tasks);
        let rows: Vec<_> = tasks
            .records
            .values()
            .filter(|r| r.owner == owner)
            .map(|r| r.row(Output::Hidden).0)
            .collect();
        Ok(json!({"tasks":rows}))
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
        let mut done = true;
        let rows: Vec<_> = records
            .iter()
            .map(|r| {
                let (row, terminal) = r.row(Output::Budgeted(&mut remaining));
                done &= terminal;
                row
            })
            .collect();
        Ok(json!({"status":if done {"done"} else {"timed_out"},"tasks":rows}))
    }
}
#[derive(Clone, Copy)]
enum Tool {
    Start,
    Status,
    List,
    Wait,
}
impl Tool {
    const ALL: [Self; 4] = [Self::Start, Self::Status, Self::List, Self::Wait];
    fn info(self, limits: &Limits) -> ToolInfo {
        let statuses =
            "running, completed, failed, rejected, unavailable, timed_out, cancelled, error";
        let (name, description, parameters) = match self {
            Self::Start => (
                START_TOOL,
                "Start a background task; returns started with task_id or unavailable. Collect output with delegate_task_wait or delegate_task_status.".to_owned(),
                task_schema(),
            ),
            Self::Status => (
                STATUS_TOOL,
                format!("Read retained output repeatedly until pruned. Statuses: {statuses}."),
                json!({"type":"object","additionalProperties":false,"required":["task_id"],"properties":{"task_id":{"type":"string"}}}),
            ),
            Self::List => (
                LIST_TOOL,
                format!("List owned retained tasks by ID. Statuses: {statuses}."),
                json!({"type":"object","additionalProperties":false,"properties":{}}),
            ),
            Self::Wait => (
                WAIT_TOOL,
                format!("Wait for 1..{} unique retained task IDs; explicit timeout_seconds is 1..{} whole seconds. Returns done or timed_out. Task statuses: {statuses}. Outputs persist until pruned. Read output_omitted rows individually with delegate_task_status.", limits.max_wait_ids, limits.max_wait.as_secs()),
                json!({"type":"object","additionalProperties":false,"required":["task_ids"],"properties":{"task_ids":{"type":"array","minItems":1,"items":{"type":"string"}},"timeout_seconds":{"type":"integer","minimum":1}}}),
            ),
        };
        let starts_work = matches!(self, Self::Start);
        ToolInfo {
            name: name.into(),
            description,
            parameters,
            retry_safe: !starts_work,
            required_permissions: vec![
                if starts_work {
                    PERMISSION_DELEGATE
                } else {
                    PERMISSION_DELEGATE_READ
                }
                .into(),
            ],
        }
    }
}
struct Executor {
    policy: Arc<Policy>,
    tool: Tool,
}
#[async_trait]
impl JsonToolExecutor for Executor {
    async fn execute(&self, _: Value) -> Result<Value, ExtensionError> {
        Err(failure("context"))
    }
    async fn execute_with_context(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        if self
            .policy
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.matches(&context))
        {
            return Err(failure("owner"));
        }
        match self.tool {
            Tool::Start => self.policy.start(context, arguments).await,
            Tool::Status => self.policy.status(&context, &arguments),
            Tool::List => self.policy.list(&context, &arguments),
            Tool::Wait => self.policy.wait_tasks(context, arguments).await,
        }
    }
}
pub(super) fn install(policy: &Arc<Policy>, registrar: &mut Registrar) {
    for tool in Tool::ALL {
        registrar.tool(Arc::new(ToolDefinition {
            info: tool.info(&policy.limits),
            executor: Arc::new(Executor {
                policy: policy.clone(),
                tool,
            }),
        }));
    }
}

fn invocation_request(
    context: &ToolContext,
    hash: &str,
) -> Result<SessionInvocationRequest, ExtensionError> {
    let key = crate::config_hash(&(&context.session_id, &context.run_id, &context.call_id));
    let key = format!("delegate-task.{}", &key[..32]);
    Ok(SessionInvocationRequest {
        id: SessionOwnerId::from(key.clone()),
        reserve_id: SessionOwnerOperationId::from(format!("{key}.reserve")),
        begin_id: SessionOwnerOperationId::from(format!("{key}.begin")),
        join_id: SessionOwnerOperationId::from(format!("{key}.join")),
        component_id: COMPONENT_ID.into(),
        behavior_fingerprint: InputFingerprint::new(hash).map_err(|_| failure("session-jobs"))?,
    })
}
struct Registration {
    policy: Arc<Policy>,
    request: SessionInvocationRequest,
    slot: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Registration {
    fn known(&mut self) {
        self.slot.take();
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            let mut tasks = lock(&self.policy.tasks);
            if !tasks
                .unresolved
                .iter()
                .any(|(request, _)| request.id == self.request.id)
            {
                tasks.unresolved.push((self.request.clone(), slot));
            }
        }
    }
}
