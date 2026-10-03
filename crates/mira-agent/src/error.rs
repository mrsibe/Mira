//! Failure surface of an agent run and of agent configuration.
//!
//! Errors are categories, never prose. Provider text, tool output, prompts, the request body and
//! credentials never appear in an error value.

use std::fmt;

/// The finite run budget that was exceeded.
///
/// The limits are independent: an agent checks the turn budget before every provider request,
/// the tool-call budget for every complete tool-call batch (whether the calls execute or are
/// answered by a refusal result), and the time budget on every wait of the run (provider, context
/// transform, tool execution and event publication).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunLimit {
    /// Provider requests allowed for one run.
    Turns,
    /// Tool calls one run may process, executed or refused.
    ToolCalls,
    /// Wall-clock budget for the whole run.
    Time,
}

impl RunLimit {
    /// Stable machine-readable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turns => "turns",
            Self::ToolCalls => "tool-calls",
            Self::Time => "time",
        }
    }
}

impl fmt::Display for RunLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Coarse category of a provider failure observed during a run.
///
/// The underlying [`mira_ai::AiError`](mira_ai::AiError) is deliberately reduced to a category
/// here so that a run error stays comparable and can never carry provider text, a URL or a
/// credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderFailure {
    /// The provider refused to start a streamed request.
    Setup,
    /// The provider stream failed after it started, including a stream that ended without a
    /// terminal result.
    Stream,
    /// The request could not be served in time.
    Timeout,
    /// The provider ended the turn with `StopReason::Failed`, for example a safety filter.
    FailedTurn,
}

impl ProviderFailure {
    /// Stable machine-readable identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Stream => "stream",
            Self::Timeout => "timeout",
            Self::FailedTurn => "failed-turn",
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Setup => "the provider rejected the request",
            Self::Stream => "the provider stream failed",
            Self::Timeout => "the provider request timed out",
            Self::FailedTurn => "the provider ended the turn without usable output",
        })
    }
}

/// Failure reported by a [`ContextTransform`](crate::ContextTransform).
///
/// The error carries no prose: a transform is caller code and may report detail through its own
/// channel. The run only records that the transform failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the context transform failed")]
pub struct ContextTransformError;

/// Everything that can fail between starting a run and its terminal outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// [`Agent::start`](crate::Agent::start) was called outside a Tokio runtime.
    #[error("a Tokio runtime is required to start a run")]
    NoRuntime,
    /// The agent already has a run in progress. Concurrent starts and configuration changes are
    /// refused instead of being queued.
    #[error("the agent already has a run in progress")]
    Busy,
    /// The caller cancelled the run, or the consumer dropped its [`AgentRun`](crate::AgentRun).
    #[error("the run was cancelled")]
    Cancelled,
    /// A finite run budget was exceeded.
    #[error("the run exceeded its {0} limit")]
    Limit(RunLimit),
    /// The provider failed.
    #[error("the model provider failed: {0}")]
    Provider(ProviderFailure),
    /// The assistant turn asked for tool calls that cannot be paired with results, for example
    /// duplicate or missing tool-call identifiers. No call from such a turn is executed.
    #[error("the assistant turn requested tool calls with duplicate or missing identifiers")]
    AmbiguousToolCalls,
    /// A [`ContextTransform`](crate::ContextTransform) failed.
    #[error("the context transform failed")]
    ContextTransform,
    /// Two tools were registered under the same name.
    #[error("duplicate tool name: {0}")]
    DuplicateTool(String),
    /// A tool's parameter schema is not a JSON Schema object, does not compile, or needs a
    /// reference that is not available offline.
    #[error("invalid tool parameter schema: {0}")]
    InvalidToolSchema(String),
    /// The requested run event buffer is larger than the runtime supports. The run is refused
    /// instead of panicking in the channel constructor.
    #[error("the run event buffer of {requested} exceeds the supported maximum of {maximum}")]
    InvalidEventBuffer {
        /// Buffer size that was requested.
        requested: usize,
        /// Largest buffer size the runtime supports.
        maximum: usize,
    },
    /// The run failed unexpectedly, including a producer that panicked. The panic payload is
    /// never returned, but it is not suppressed either: `catch_unwind` does not replace Rust's
    /// panic hook, so a caller that needs silence must install its own hook. This crate never
    /// installs one.
    #[error("the run failed unexpectedly")]
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_provider_categories_have_stable_identifiers() {
        assert_eq!(RunLimit::Turns.as_str(), "turns");
        assert_eq!(RunLimit::ToolCalls.as_str(), "tool-calls");
        assert_eq!(RunLimit::Time.as_str(), "time");
        assert_eq!(ProviderFailure::Setup.as_str(), "setup");
        assert_eq!(ProviderFailure::Stream.as_str(), "stream");
        assert_eq!(ProviderFailure::Timeout.as_str(), "timeout");
        assert_eq!(ProviderFailure::FailedTurn.as_str(), "failed-turn");
    }

    #[test]
    fn errors_render_as_categories() {
        assert_eq!(
            AgentError::Limit(RunLimit::ToolCalls).to_string(),
            "the run exceeded its tool-calls limit"
        );
        assert_eq!(
            AgentError::Provider(ProviderFailure::Stream).to_string(),
            "the model provider failed: the provider stream failed"
        );
    }
}
