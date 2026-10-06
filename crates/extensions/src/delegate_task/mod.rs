//! Bounded delegation to a trusted host runner.
//!
//! This native extension creates no child agents, sandbox, providers or
//! credentials. Hosts own provisioning, policy and resource limits. Input and
//! output travel through the durable tool-call path; hosts needing redaction
//! should mount the tool result redactor. Side effects are not transactional.
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
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Model-visible tool name.
pub const TOOL_NAME: &str = "delegate_task";
/// Permission metadata; the host permission policy decides access.
pub const PERMISSION_DELEGATE: &str = "session.subagent";
/// Hard byte cap for an opaque ASCII profile identifier.
pub const MAX_PROFILE_BYTES: usize = 256;
const MAX_TASK_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 256 * 1024;
const MAX_IN_FLIGHT: usize = 256;
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(5 * 60);

/// Finite bounds shared by all mounts of one instance.
///
/// Results are bounded per call. A parallel model turn can carry up to
/// `max_in_flight` results of `max_result_bytes` each plus JSON escaping;
/// hosts should size these bounds together.
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
    /// Maximum invocation wait (up to one hour).
    pub max_wait: Duration,
    /// Maximum shutdown join time (up to five minutes).
    pub shutdown_grace: Duration,
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
    /// Child token cancelled on interruption, deadline or any executor exit.
    /// Cancelling this token does not cancel the parent run.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
    /// Maximum time the parent invocation waits.
    pub fn max_wait(&self) -> Duration {
        self.max_wait
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
/// Direct callers must cancel executors before shutdown, which does not itself
/// cancel live requests or prevent subsequent calls.
pub struct DelegateTask {
    policy: Arc<Policy>,
    hash: String,
}
struct Policy {
    runner: Runner,
    limits: Limits,
    capacity: Arc<Semaphore>,
    cleanup: CleanupOwner,
}
impl DelegateTask {
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
            || limits.max_wait.is_zero()
            || limits.max_wait > MAX_WAIT
            || limits.shutdown_grace.is_zero()
            || limits.shutdown_grace > MAX_SHUTDOWN_GRACE
        {
            return Err(crate::config_error("delegate-task-limits"));
        }
        if !crate::valid_identity(&options.runner_identity) {
            return Err(crate::config_error("delegate-task-policy"));
        }
        let hash = crate::config_hash(&(
            "delegate-task-v1",
            TOOL_NAME,
            PERMISSION_DELEGATE,
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
        Ok(())
    }
    async fn shutdown(&self) {
        let _ = self
            .policy
            .cleanup
            .join(self.policy.limits.shutdown_grace)
            .await;
    }
}
