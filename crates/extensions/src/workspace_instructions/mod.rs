//! Fresh, bounded instruction files from host-admitted directory capabilities.
mod files;
mod workspace;
pub use workspace::TrustedWorkspace;

use async_trait::async_trait;
use crabber::extension::{Extension, ExtensionError, PromptAttemptContext, Registrar};
use serde::Serialize;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, oneshot};

pub const PROMPT_NAME: &str = "workspace/instructions";
pub const DEFAULT_ORDER: i32 = 100;

/// Return None to withhold instructions. Workspace identity is routing data;
/// this trusted host callback makes the authorization decision each attempt.
pub type Resolver = Arc<
    dyn Fn(
            PromptAttemptContext,
        ) -> Pin<
            Box<dyn Future<Output = Result<Option<Arc<TrustedWorkspace>>, ExtensionError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone, Serialize)]
pub struct Limits {
    pub max_file_names: usize,
    pub max_chain_depth: usize,
    pub max_file_bytes: usize,
    pub max_section_bytes: usize,
    pub max_in_flight: usize,
    pub max_wait: Duration,
}

pub struct Options {
    pub order: i32,
    /// Ordered basenames. Use vec!["AGENTS.md".into()] for the reference default.
    pub file_names: Vec<String>,
    /// Bounded non-secret identity; rotate when resolver behavior changes.
    pub resolver_identity: String,
    pub resolver: Resolver,
    pub limits: Limits,
}

pub struct WorkspaceInstructions {
    policy: Arc<Policy>,
    hash: String,
}
struct Policy {
    order: i32,
    names: Vec<String>,
    resolver: Resolver,
    limits: Limits,
    capacity: Arc<Semaphore>,
}

impl WorkspaceInstructions {
    pub fn new(options: Options) -> Result<Self, ExtensionError> {
        let l = &options.limits;
        if l.max_file_names == 0
            || l.max_file_names > 16
            || l.max_chain_depth == 0
            || l.max_chain_depth > 64
            || l.max_file_bytes == 0
            || l.max_file_bytes > 16 * 1024
            || l.max_section_bytes < 256
            || l.max_section_bytes > 32 * 1024
            || l.max_in_flight == 0
            || l.max_in_flight > 256
            || l.max_wait.is_zero()
            || l.max_wait > Duration::from_secs(5)
        {
            return Err(crate::config_error("instructions-limits"));
        }
        let mut names = std::collections::HashSet::new();
        if options.file_names.is_empty()
            || options.file_names.len() > l.max_file_names
            || options
                .file_names
                .iter()
                .any(|name| !valid_file_name(name) || !names.insert(name))
            || !crate::valid_identity(&options.resolver_identity)
        {
            return Err(crate::config_error("instructions-policy"));
        }
        let hash = crate::config_hash(&(
            "workspace-instructions-v1",
            options.order,
            &options.file_names,
            &options.resolver_identity,
            l,
        ));
        Ok(Self {
            hash,
            policy: Arc::new(Policy {
                order: options.order,
                names: options.file_names,
                resolver: options.resolver,
                capacity: Arc::new(Semaphore::new(l.max_in_flight)),
                limits: options.limits,
            }),
        })
    }
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && !name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
}
fn failure(code: &'static str) -> ExtensionError {
    ExtensionError::Tool(format!("workspace instructions failed: {code}"))
}

impl Policy {
    async fn provide(
        self: Arc<Self>,
        context: PromptAttemptContext,
    ) -> Result<Option<String>, ExtensionError> {
        let token = context.cancellation().clone();
        if token.is_cancelled() {
            return Err(failure("cancelled"));
        }
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure("capacity"))?;
        let (send, recv) = oneshot::channel();
        let max_wait = self.limits.max_wait;
        let cleanup = context.cleanup().clone();
        let wait_token = token.clone();
        // The tracked task owns both resolver work and the blocking read join.
        // Dropping the callback/receiver cannot release its permit early.
        cleanup.spawn(async move {
            let result = async {
                let workspace = (self.resolver)(context.clone())
                    .await
                    .map_err(|_| failure("resolver"))?;
                let Some(workspace) = workspace else {
                    return Ok(None);
                };
                if context.workspace().directory().map(std::path::Path::new)
                    != Some(workspace.root())
                {
                    return Err(failure("workspace-mismatch"));
                }
                if token.is_cancelled() {
                    return Err(failure("cancelled"));
                }
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    files::render(&workspace, &self.names, &self.limits, || {
                        token.is_cancelled()
                    })
                })
                .await
                .map_err(|_| failure("reader"))?
            }
            .await;
            let _ = send.send(result);
        });
        match tokio::time::timeout(max_wait, recv).await {
            Ok(reply) => reply.map_err(|_| failure("worker"))?,
            Err(_) => {
                wait_token.cancel();
                Err(failure("deadline"))
            }
        }
    }
}

#[async_trait]
impl Extension for WorkspaceInstructions {
    fn id(&self) -> &str {
        "crabber-extensions/workspace-instructions"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn config_hash(&self) -> String {
        self.hash.clone()
    }
    async fn install(&self, registrar: &mut Registrar) -> Result<(), ExtensionError> {
        let policy = self.policy.clone();
        registrar.prompt_contributor(
            policy.order,
            PROMPT_NAME,
            Arc::new(move |context| Box::pin(policy.clone().provide(context))),
        );
        Ok(())
    }
}
