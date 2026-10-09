use super::{failure, manager::Policy};
use async_trait::async_trait;
use crabber::extension::{ExtensionError, ToolContext, ToolExecutor};
use serde_json::Value;
use std::sync::Arc;

macro_rules! executor {
    ($name:ident, $method:ident $(, $await:tt)?) => {
        pub(super) struct $name(pub(super) Arc<Policy>);
        #[async_trait]
        impl ToolExecutor for $name {
            async fn execute(&self, _arguments: Value) -> Result<Value, ExtensionError> {
                Err(failure("context"))
            }
            async fn execute_with_context(&self, context: ToolContext, arguments: Value) -> Result<Value, ExtensionError> {
                self.0.$method(context, arguments)$(.$await)?
            }
        }
    };
}
executor!(StartTool, start, await);
executor!(StatusTool, status);
executor!(ListTool, list);
executor!(KillTool, kill, await);
