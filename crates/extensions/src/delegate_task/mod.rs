//! Bounded delegation to a trusted host runner.
//!
//! This native extension creates no child agents, sandbox, providers or
//! credentials. Hosts own provisioning, policy and resource limits. Input and
//! output travel through the durable tool-call path; hosts needing redaction
//! should mount the tool result redactor. Side effects are not transactional.
mod background;
mod input;

use crate::safe_future::SafeFuture;
use async_trait::async_trait;
use crabber::{
    core::{RunId, SessionId, ToolCallId, ToolInfo},
    extension::{
        CleanupOwner, Extension, ExtensionError, Registrar, ToolContext, ToolDefinition,
        ToolExecutor, WorkspaceContext,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

/// Model-visible tool name.
pub const TOOL_NAME: &str = "delegate_task";
/// Permission metadata; the host permission policy decides access.
pub const PERMISSION_DELEGATE: &str = "session.subagent";
/// Hard byte cap for an opaque ASCII profile identifier.
pub const MAX_PROFILE_BYTES: usize = 256;
/// Start a background task.
pub const START_TOOL: &str = "delegate_task_start";
/// Read a retained task.
pub const STATUS_TOOL: &str = "delegate_task_status";
/// List owned tasks.
pub const LIST_TOOL: &str = "delegate_task_list";
/// Wait for named tasks.
pub const WAIT_TOOL: &str = "delegate_task_wait";
/// Permission metadata for background reads.
pub const PERMISSION_DELEGATE_READ: &str = "session.subagent.read";
const TASK_ID_PREFIX: &str = "task_";
const TASK_ID_EPOCH_BYTES: usize = 32;
const TASK_ID_COUNTER_BYTES: usize = 16;
const TASK_ID_SEPARATOR: usize = TASK_ID_PREFIX.len() + TASK_ID_EPOCH_BYTES;
/// Fixed byte length of a process-local background task ID.
pub const TASK_ID_BYTES: usize = TASK_ID_SEPARATOR + 1 + TASK_ID_COUNTER_BYTES;
const MAX_TASK_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 256 * 1024;
const MAX_IN_FLIGHT: usize = 256;
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(5 * 60);

/// Finite bounds shared by all mounts of one instance.
///
/// Results are bounded per call before JSON escaping. `max_in_flight` bounds
/// simultaneous live runners, not results accumulated in a turn or conversation.
/// Completed calls release slots for further calls in the same turn. Hosts size
/// result bounds together with their tool-call and context budgets.
#[derive(Clone, Serialize)]
pub struct Limits {
    /// Maximum UTF-8 bytes in the task (up to 64 KiB).
    pub max_task_bytes: usize,
    /// Maximum bytes in the profile (up to 256).
    pub max_profile_bytes: usize,
    /// Maximum UTF-8 output bytes (up to 256 KiB), before JSON escaping.
    pub max_result_bytes: usize,
    /// Maximum live runner futures (up to 256).
    pub max_in_flight: usize,
    /// Inline deadline and default/max read wait (one second to one hour).
    /// Explicit read timeouts use whole seconds up to floor(max_wait); omitted
    /// timeouts preserve the full configured duration, including fractions.
    pub max_wait: Duration,
    /// Maximum shutdown join time (up to five minutes).
    pub shutdown_grace: Duration,
    /// Retained records per instance (up to 256).
    pub max_tracked: usize,
    /// Background runner deadline (up to 24 hours).
    pub max_background_duration: Duration,
    /// IDs per wait (up to 64).
    pub max_wait_ids: usize,
    /// Output bytes per wait before escaping (up to 1 MiB). Outputs larger
    /// than this are always omitted by wait and must be read with status.
    pub max_wait_reply_bytes: usize,
}

impl Limits {
    /// Saturating bound for output bytes in tracked records, before escaping.
    /// Excludes record metadata and evicted records pinned by in-flight waits.
    pub fn worst_case_retained_bytes(&self) -> usize {
        self.max_tracked.saturating_mul(self.max_result_bytes)
    }

    /// Saturating bound for JSON-escaped wait output and row envelopes.
    pub fn worst_case_wait_bytes(&self) -> usize {
        self.max_wait_reply_bytes
            .saturating_mul(6)
            .saturating_add(self.max_wait_ids.saturating_mul(128))
            .saturating_add(64)
    }
}

/// Authoritative request; no public constructor permits forged context.
/// Hosts can test runners through a mounted tool and `ToolContext::new`.
#[derive(Clone)]
pub struct Request {
    session_id: SessionId,
    run_id: RunId,
    call_id: ToolCallId,
    workspace: WorkspaceContext,
    task: String,
    profile: String,
    cancellation: CancellationToken,
    max_wait: Duration,
    background: bool,
}
impl Request {
    /// Parent session identity supplied by Crabber.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    /// Parent run identity supplied by Crabber.
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    /// Parent tool-call identity supplied by Crabber.
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }
    /// Persisted workspace routing data, not a filesystem capability.
    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }
    /// Bounded task preserved exactly from model input.
    pub fn task(&self) -> &str {
        &self.task
    }
    /// Opaque host profile preserved exactly from model input.
    pub fn profile(&self) -> &str {
        &self.profile
    }
    /// Inline tokens cancel on interruption, deadline or executor exit.
    /// Background tokens cancel on their deadline or extension shutdown only.
    /// Cancelling this token does not cancel the parent run.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
    /// Whether this request runs independently of its parent invocation.
    pub fn background(&self) -> bool {
        self.background
    }
    /// Runner deadline duration: inline max_wait or max_background_duration.
    pub fn deadline(&self) -> Duration {
        self.max_wait
    }
    /// Alias for [`Self::deadline`].
    pub fn max_wait(&self) -> Duration {
        self.deadline()
    }
}

/// Host-trusted output, stored verbatim within the configured byte bound.
pub enum Response {
    /// Task completed successfully.
    Completed(String),
    /// Task failed with host-visible explanation.
    Failed(String),
    /// Host rejected the task or profile.
    Rejected(String),
    /// Host runner is unavailable.
    Unavailable(String),
}

/// Host callback running on an extension-owned tracked task.
///
/// In background mode the token fires on the background deadline or extension
/// shutdown, never on parent call or run cancellation.
/// Invoked concurrently up to `Limits::max_in_flight`. The future must not
/// block while polled, must observe `Request::cancellation`, and return promptly
/// after cancellation. Its capacity slot stays held until it exits. The token
/// may already be cancelled at first poll: check it before starting side effects.
/// The host owns any child agents, sandbox, model/tool policy, credentials and
/// resource limits; `profile` is opaque routing data. Inputs and outputs are
/// durable and must not contain secrets. Side effects are not transactional.
/// Any `Err` payload is discarded and replaced by a fixed failure; use
/// `Response::Failed` or `Response::Rejected` for host-visible failure text.
/// Valid response text is host-trusted and stored unredacted within its bound.
pub type Runner = Arc<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Result<Response, ExtensionError>> + Send>>
        + Send
        + Sync,
>;

/// Construction options for [`DelegateTask`].
pub struct Options {
    /// Bounded, non-secret identity of host runner behavior.
    pub runner_identity: String,
    /// Host-owned runner callback.
    pub runner: Runner,
    /// Required finite request, response and lifecycle limits.
    pub limits: Limits,
}

/// Bounded native delegation tool.
///
/// Use one instance per registry or tenant for isolated capacity and close.
/// Shared instances share capacity and a tracker: shutdown of any mount waits
/// for every mount's runners. Shutdown joins at most `shutdown_grace`, then
/// silently detaches stragglers, which retain slots until they exit. Mount close
/// can take its registry drain bound plus this grace; the grace timeout is not
/// reported as a mount-close error. Hosts interrupt active runs before closing.
/// Direct callers must cancel inline executors before shutdown. Shutdown does
/// not cancel inline requests or prevent later inline calls. Closing any mount
/// permanently refuses background starts on every shared mount and cancels
/// running background tasks. Reads remain available. In-flight waits hold plan
/// leases: interrupt runs before closing to avoid a mount drain timeout.
/// Dropping an unclosed instance does not cancel its background runners: hosts
/// must explicitly close and retry a timed-out close. Otherwise work can remain
/// live until max_background_duration.
pub struct DelegateTask {
    policy: Arc<Policy>,
    hash: String,
}
struct Policy {
    runner: Runner,
    limits: Limits,
    capacity: Arc<Semaphore>,
    cleanup: CleanupOwner,
    tasks: Mutex<background::Tasks>,
    settled: watch::Sender<u64>,
}
impl DelegateTask {
    /// Shared capacity currently held by inline or background runners.
    /// Includes detached runners until their futures exit. This is a snapshot;
    /// concurrent starts and completions may change it immediately.
    pub fn live_runners(&self) -> usize {
        self.policy.limits.max_in_flight - self.policy.capacity.available_permits()
    }

    /// Validates all bounds and the runner identity before construction.
    pub fn new(options: Options) -> Result<Self, ExtensionError> {
        let limits = &options.limits;
        if limits.max_task_bytes == 0
            || limits.max_task_bytes > MAX_TASK_BYTES
            || limits.max_profile_bytes == 0
            || limits.max_profile_bytes > MAX_PROFILE_BYTES
            || limits.max_result_bytes == 0
            || limits.max_result_bytes > MAX_RESULT_BYTES
            || limits.max_in_flight == 0
            || limits.max_in_flight > MAX_IN_FLIGHT
            || limits.max_wait < Duration::from_secs(1)
            || limits.max_wait > MAX_WAIT
            || !(1..=256).contains(&limits.max_tracked)
            || limits.max_background_duration.is_zero()
            || limits.max_background_duration > Duration::from_secs(24 * 3600)
            || !(1..=64).contains(&limits.max_wait_ids)
            || !(1..=1024 * 1024).contains(&limits.max_wait_reply_bytes)
            || limits.shutdown_grace.is_zero()
            || limits.shutdown_grace > MAX_SHUTDOWN_GRACE
        {
            return Err(crate::config_error("delegate-task-limits"));
        }
        if !crate::valid_identity(&options.runner_identity) {
            return Err(crate::config_error("delegate-task-policy"));
        }
        let hash = crate::config_hash(&(
            "delegate-task-v2",
            [TOOL_NAME, START_TOOL, STATUS_TOOL, LIST_TOOL, WAIT_TOOL],
            [PERMISSION_DELEGATE, PERMISSION_DELEGATE_READ],
            &options.runner_identity,
            limits,
        ));
        let capacity = Arc::new(Semaphore::new(limits.max_in_flight));
        Ok(Self {
            policy: Arc::new(Policy {
                runner: options.runner,
                limits: options.limits,
                capacity,
                cleanup: CleanupOwner::new(),
                tasks: Mutex::new(background::Tasks::new()),
                settled: watch::channel(0).0,
            }),
            hash,
        })
    }
}
fn failure(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("delegate task failed: {code}"))
}
fn invalid(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("delegate task input invalid: {code}"))
}

#[async_trait]
impl ToolExecutor for Policy {
    async fn execute(&self, _: Value) -> Result<Value, ExtensionError> {
        Err(failure("context"))
    }
    async fn execute_with_context(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        if context.cancel.is_cancelled() {
            return Err(failure("cancelled"));
        }
        let (task, profile) = input::normalize(arguments, &self.limits)?;
        let permit = match self.capacity.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(json!({"status":"unavailable"})),
        };
        let token = context.cancel.child_token();
        let _guard = token.clone().drop_guard();
        let request = Request {
            session_id: context.session_id.clone(),
            run_id: context.run_id.clone(),
            call_id: context.call_id.clone(),
            workspace: context.workspace().clone(),
            task,
            profile,
            cancellation: token,
            max_wait: self.limits.max_wait,
            background: false,
        };
        let deadline = tokio::time::sleep(self.limits.max_wait);
        tokio::pin!(deadline);
        let runner = self.runner.clone();
        let mut task = self.cleanup.tracker().spawn(async move {
            let _permit = permit;
            let future = catch_unwind(AssertUnwindSafe(|| (runner)(request)))
                .map_err(|_| failure("runner"))?;
            SafeFuture::new(future, || failure("runner")).wait().await
        });
        tokio::select! {
            biased;
            _ = context.cancel.cancelled() => Err(failure("cancelled")),
            _ = &mut deadline => Ok(json!({"status":"timed_out"})),
            reply = &mut task => match reply {
                Ok(Ok(response)) => map_reply(response, &self.limits),
                _ => Err(failure("runner")),
            },
        }
    }
}
fn map_reply(response: Response, limits: &Limits) -> Result<Value, ExtensionError> {
    let (status, output) = match response {
        Response::Completed(out) => ("completed", out),
        Response::Failed(out) => ("failed", out),
        Response::Rejected(out) => ("rejected", out),
        Response::Unavailable(out) => ("unavailable", out),
    };
    if output.len() > limits.max_result_bytes || output.contains('\0') {
        return Err(failure("runner"));
    }
    let mut value = json!({"status":status});
    if !output.is_empty() {
        value["output"] = Value::String(output);
    }
    Ok(value)
}
#[async_trait]
impl Extension for DelegateTask {
    fn id(&self) -> &str {
        "crabber-extensions/delegate-task"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn config_hash(&self) -> String {
        self.hash.clone()
    }
    async fn install(&self, registrar: &mut Registrar) -> Result<(), ExtensionError> {
        registrar.tool(Arc::new(ToolDefinition {
            info: ToolInfo { name: TOOL_NAME.into(), description: "Delegate one bounded task under an opaque host profile. Returns completed, failed, rejected, unavailable, or timed_out.".into(), parameters: json!({"type":"object","additionalProperties":false,"required":["task","profile"],"properties":{"task":{"type":"string"},"profile":{"type":"string"}}}), retry_safe: false, required_permissions: vec![PERMISSION_DELEGATE.into()] }, executor: self.policy.clone(),
        }));
        background::install(&self.policy, registrar);
        Ok(())
    }
    async fn shutdown(&self) {
        self.policy.close_tasks();
        let _ = self
            .policy
            .cleanup
            .join(self.policy.limits.shutdown_grace)
            .await;
    }
}
