//! Bounded, host-mediated multiple-choice interaction.
//!
//! This trusted native extension supplies no UI and must not be used to collect
//! secrets. The responder is a drop-safe host future: deadline, cancellation,
//! and run interruption drop it and release capacity immediately. A responder
//! must not block while polled and owns work it spawns. Crabber's permission
//! policy, approver, and store see the model-authored question and options;
//! `required_permissions` is metadata and this extension makes no policy choice.
mod input;

use async_trait::async_trait;
use crabber::{
    core::{RunId, SessionId, ToolCallId, ToolInfo},
    extension::{
        Extension, ExtensionError, Registrar, ToolContext, ToolDefinition, ToolExecutor,
        WorkspaceContext,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};
use tokio::sync::Semaphore;

/// Model-visible name of the ask-user tool.
pub const TOOL_NAME: &str = "ask_user";
/// Permission metadata advertised by the ask-user tool.
pub const PERMISSION_ASK: &str = "interaction.ask";
/// Label of the automatic free-form response choice.
pub const CUSTOM_OPTION_LABEL: &str = "Other (write your own answer)";
/// Smallest permitted number of model-supplied choices.
pub const MIN_OPTIONS: usize = 2;
/// Largest permitted number of model-supplied choices.
pub const MAX_OPTIONS: usize = 5;

/// Finite limits enforced for each ask-user mount.
#[derive(Clone, Serialize)]
pub struct Limits {
    /// Maximum UTF-8 bytes in the question.
    pub max_question_bytes: usize,
    /// Maximum UTF-8 bytes in an option label.
    pub max_option_label_bytes: usize,
    /// Maximum UTF-8 bytes in an option description.
    pub max_option_description_bytes: usize,
    /// Maximum UTF-8 bytes in a custom response.
    pub max_custom_answer_bytes: usize,
    /// Maximum concurrent responder futures shared by the mount.
    pub max_in_flight: usize,
    /// Maximum time an invocation waits for the responder.
    pub max_wait: Duration,
}

/// One model-supplied multiple-choice option.
#[derive(Clone, Debug)]
pub struct AskOption {
    /// Option label, preserved exactly from model input.
    pub label: String,
    /// Optional explanatory description, with empty input normalized to absent.
    pub description: Option<String>,
}

/// Authoritative, bounded request delivered to a host responder.
#[derive(Clone)]
pub struct Request {
    session_id: SessionId,
    run_id: RunId,
    call_id: ToolCallId,
    workspace: WorkspaceContext,
    question: String,
    options: Vec<AskOption>,
    max_wait: Duration,
}
impl Request {
    /// Session identity supplied by Crabber, never model input.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    /// Run identity supplied by Crabber, never model input.
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
    /// Tool-call identity supplied by Crabber, never model input.
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }
    /// Persisted workspace routing context for this call.
    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }
    /// Bounded question text, preserved exactly from model input.
    pub fn question(&self) -> &str {
        &self.question
    }
    /// Bounded model-supplied choices.
    pub fn options(&self) -> &[AskOption] {
        &self.options
    }
    /// The automatic free-form option label.
    pub fn custom_label(&self) -> &str {
        CUSTOM_OPTION_LABEL
    }
    /// The maximum time this invocation may wait.
    pub fn max_wait(&self) -> Duration {
        self.max_wait
    }
}

/// A host reply to an ask-user request.
pub enum Response {
    /// Select a one-based index from the request options.
    Selected(usize),
    /// Supply a bounded non-blank free-form response.
    Custom(String),
    /// Dismiss the interaction without a response.
    Dismissed,
    /// Indicate that interaction is currently unavailable.
    Unavailable,
}

/// Host callback for an ask-user request.
///
/// Its future must be drop-safe and must not block during polling. It can be
/// invoked concurrently up to `Limits::max_in_flight`; dropping it is the only
/// cancellation signal, so work spawned by a host remains host-owned.
pub type Responder = Arc<
    dyn Fn(Request) -> Pin<Box<dyn Future<Output = Result<Response, ExtensionError>> + Send>>
        + Send
        + Sync,
>;
type ResponseFuture = Pin<Box<dyn Future<Output = Result<Response, ExtensionError>> + Send>>;

/// Construction options for [`AskUser`].
pub struct Options {
    /// Bounded, non-secret identity of host responder behavior.
    pub responder_identity: String,
    /// Host callback that mediates interaction.
    pub responder: Responder,
    /// Required finite bounds for requests and replies.
    pub limits: Limits,
}

/// A bounded native `ask_user` tool extension.
pub struct AskUser {
    policy: Arc<Policy>,
    hash: String,
}
struct Policy {
    responder: Responder,
    limits: Limits,
    capacity: Arc<Semaphore>,
}

impl AskUser {
    /// Validates configuration and constructs the extension.
    pub fn new(options: Options) -> Result<Self, ExtensionError> {
        let limits = &options.limits;
        if limits.max_question_bytes == 0
            || limits.max_question_bytes > 16 * 1024
            || limits.max_option_label_bytes == 0
            || limits.max_option_label_bytes > 1024
            || limits.max_option_description_bytes == 0
            || limits.max_option_description_bytes > 4 * 1024
            || limits.max_custom_answer_bytes == 0
            || limits.max_custom_answer_bytes > 16 * 1024
            || limits.max_in_flight == 0
            || limits.max_in_flight > 256
            || limits.max_wait.is_zero()
            || limits.max_wait > Duration::from_secs(10 * 60)
        {
            return Err(crate::config_error("ask-user-limits"));
        }
        if !crate::valid_identity(&options.responder_identity) {
            return Err(crate::config_error("ask-user-policy"));
        }
        let hash = crate::config_hash(&(
            "ask-user-v1",
            TOOL_NAME,
            PERMISSION_ASK,
            CUSTOM_OPTION_LABEL,
            &options.responder_identity,
            limits,
        ));
        let capacity = Arc::new(Semaphore::new(limits.max_in_flight));
        Ok(Self {
            policy: Arc::new(Policy {
                responder: options.responder,
                limits: options.limits,
                capacity,
            }),
            hash,
        })
    }
}

fn failure(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("ask user failed: {code}"))
}
fn invalid(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("ask user input invalid: {code}"))
}

impl Policy {
    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        if context.cancel.is_cancelled() {
            return Err(failure("cancelled"));
        }
        let (question, options) = input::normalize(arguments, &self.limits)?;
        let permit = match self.capacity.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(json!({"status": "unavailable"})),
        };
        let labels = options
            .iter()
            .map(|option| option.label.clone())
            .collect::<Vec<_>>();
        let request = Request {
            session_id: context.session_id.clone(),
            run_id: context.run_id.clone(),
            call_id: context.call_id.clone(),
            workspace: context.workspace().clone(),
            question,
            options,
            max_wait: self.limits.max_wait,
        };
        let responder = match catch_unwind(AssertUnwindSafe(|| (self.responder)(request))) {
            Ok(future) => SafeFuture::new(future),
            Err(_) => return Err(failure("responder")),
        };
        let mut responder = responder;
        let deadline = tokio::time::sleep(self.limits.max_wait);
        tokio::pin!(deadline);
        let result = tokio::select! {
            biased;
            _ = context.cancel.cancelled() => Err(failure("cancelled")),
            _ = &mut deadline => Ok(json!({"status": "timed_out"})),
            reply = responder.wait() => map_reply(reply, &labels, &self.limits),
        };
        drop(responder);
        drop(permit);
        result
    }
}

struct SafeFuture {
    future: Option<ResponseFuture>,
}
impl SafeFuture {
    fn new(future: ResponseFuture) -> Self {
        Self {
            future: Some(future),
        }
    }
    async fn wait(&mut self) -> Result<Response, ExtensionError> {
        poll_fn(|cx| {
            let Some(future) = self.future.as_mut() else {
                return Poll::Ready(Err(failure("responder")));
            };
            match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                Ok(poll) => poll,
                Err(_) => Poll::Ready(Err(failure("responder"))),
            }
        })
        .await
    }
}
impl Drop for SafeFuture {
    fn drop(&mut self) {
        if let Some(future) = self.future.take() {
            let _ = catch_unwind(AssertUnwindSafe(|| drop(future)));
        }
    }
}

fn map_reply(
    reply: Result<Response, ExtensionError>,
    labels: &[String],
    limits: &Limits,
) -> Result<Value, ExtensionError> {
    match reply.map_err(|_| failure("responder"))? {
        Response::Selected(index) if (1..=labels.len()).contains(&index) => Ok(json!({
            "status": "selected", "answer": labels[index - 1], "selected_option": index,
        })),
        Response::Selected(_) => Err(failure("responder")),
        Response::Custom(answer)
            if input::required_text(&answer, limits.max_custom_answer_bytes) =>
        {
            Ok(json!({"status": "custom", "answer": answer}))
        }
        Response::Custom(_) => Err(failure("responder")),
        Response::Dismissed => Ok(json!({"status": "dismissed"})),
        Response::Unavailable => Ok(json!({"status": "unavailable"})),
    }
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
        self.execute(context, arguments).await
    }
}

#[async_trait]
impl Extension for AskUser {
    fn id(&self) -> &str {
        "crabber-extensions/ask-user"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn config_hash(&self) -> String {
        self.hash.clone()
    }
    async fn install(&self, registrar: &mut Registrar) -> Result<(), ExtensionError> {
        registrar.tool(Arc::new(ToolDefinition {
            info: ToolInfo {
                name: TOOL_NAME.into(),
                description: "Ask one bounded multiple-choice question with an automatic free-form choice. The result may be selected, custom, dismissed, unavailable, or timed out.".into(),
                parameters: json!({"type":"object","additionalProperties":false,"required":["question","options"],"properties":{"question":{"type":"string"},"options":{"type":"array","minItems":2,"maxItems":5,"items":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"label":{"type":"string"},"description":{"type":"string"}}}}}}),
                retry_safe: false,
                required_permissions: vec![PERMISSION_ASK.into()],
            },
            executor: self.policy.clone(),
        }));
        Ok(())
    }
}
