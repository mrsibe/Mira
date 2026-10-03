//! Failure surface of the runtime package.
//!
//! Every category is constant. Provider text, tool output, prompts, context snippets, the
//! request body, a credential and the resolver's own diagnostics never appear in an error value.

use std::time::Duration;

use mira_agent::AgentError;

/// Everything that can fail between starting a session run and its terminal outcome.
///
/// The categories are deliberately coarse: `Credential`, `Context` and `UnknownBinding` carry no
/// caller prose, so an error can never echo a credential, a provider response or a raw context
/// failure. The binding key in [`RuntimeError::UnknownBinding`] is caller-authored configuration,
/// not a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// A session run was started outside a Tokio runtime. Starting a run spawns the run driver
    /// and the child agent, both of which need a runtime.
    #[error("a Tokio runtime is required to start a session run")]
    NoRuntime,
    /// The session already has a run in progress. Concurrent prompts and configuration changes
    /// are refused instead of being queued, from before credential preparation.
    #[error("the session already has a run in progress")]
    Busy,
    /// The run was cancelled, either by [`SessionRun::cancel`](crate::SessionRun::cancel) or by
    /// dropping the run handle.
    #[error("the session run was cancelled")]
    Cancelled,
    /// The run exceeded its total time budget, including credential preparation, context
    /// assembly, the model run, forwarding and backpressure.
    #[error("the session run exceeded its total time budget")]
    Timeout,
    /// The [`CredentialResolver`](crate::CredentialResolver) did not produce a credential.
    #[error("the credential resolver did not provide a credential")]
    Credential,
    /// A [`ContextProvider`](crate::ContextProvider) failed.
    #[error("the context providers did not produce a context")]
    Context,
    /// The selected binding key is not registered.
    #[error("no model binding is registered under the key \"{0}\"")]
    UnknownBinding(String),
    /// Two bindings were registered under the same key.
    #[error("two model bindings were registered under the key \"{0}\"")]
    DuplicateBinding(String),
    /// A binding key or model identifier is empty.
    #[error("the model binding \"{0}\" is not usable")]
    InvalidBinding(String),
    /// The child agent run failed. The underlying [`AgentError`] carries the same constant
    /// categories as `mira-agent`.
    #[error("the agent run failed: {0}")]
    Agent(AgentError),
    /// The configured total run budget is larger than the runtime can schedule.
    #[error("the total run budget of {requested:?} exceeds the supported maximum of {maximum:?}")]
    InvalidRunTime {
        /// Budget that was requested.
        requested: Duration,
        /// Longest budget the runtime can represent on a monotonic clock.
        maximum: Duration,
    },
    /// The configured event buffer is larger than the runtime supports.
    #[error("the event buffer of {requested} exceeds the supported maximum of {maximum}")]
    InvalidEventBuffer {
        /// Buffer size that was requested.
        requested: usize,
        /// Largest buffer size the runtime supports.
        maximum: usize,
    },
    /// The run failed unexpectedly, including a run task that was dropped before it finished.
    #[error("the session run failed unexpectedly")]
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_as_categories() {
        assert_eq!(
            RuntimeError::Busy.to_string(),
            "the session already has a run in progress"
        );
        assert_eq!(
            RuntimeError::UnknownBinding("fast".to_string()).to_string(),
            "no model binding is registered under the key \"fast\""
        );
        assert_eq!(
            RuntimeError::InvalidEventBuffer {
                requested: 5,
                maximum: 4,
            }
            .to_string(),
            "the event buffer of 5 exceeds the supported maximum of 4"
        );
    }
}
