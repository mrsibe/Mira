//! Agent configuration: the injected provider, model, tools, limits and request options.

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use mira_ai::{Message, Model, Provider, RequestOptions};

use crate::tool::ToolRegistry;
use crate::transform::ContextTransform;

/// Default number of buffered run events before the run waits for the consumer.
pub const DEFAULT_EVENT_BUFFER: NonZeroUsize = NonZeroUsize::new(64).unwrap();
/// Largest run event buffer the runtime supports, taken from the runtime itself.
///
/// A larger buffer is rejected by [`Agent::start`](crate::Agent::start) and
/// [`Agent::set_event_buffer`](crate::Agent::set_event_buffer) with
/// [`AgentError::InvalidEventBuffer`](crate::AgentError::InvalidEventBuffer).
pub const MAX_EVENT_BUFFER: usize = tokio::sync::Semaphore::MAX_PERMITS;
/// Default number of provider requests one run may make.
pub const DEFAULT_MAX_TURNS: u32 = 16;
/// Default number of tool calls one run may execute.
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 64;
/// Default wall-clock budget for one run, including provider, tool, transform and publication
/// waits.
pub const DEFAULT_RUN_TIME: Duration = Duration::from_secs(300);
/// Longest total time a run may be given when [`AgentLimits::total_time`] is too large for the
/// platform clock. A larger budget is clamped to this bound rather than overflowing the clock.
pub const MAXIMUM_RUN_TIME: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 100);

/// Finite budgets of one run.
///
/// A limit of zero is legal and makes the corresponding budget fail immediately; it is a way to
/// assert that a run does not start, not a way to disable a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLimits {
    /// Provider requests allowed for one run, counted before each request.
    pub max_turns: u32,
    /// Tool calls one run may process, either by executing them or by answering them with a
    /// refusal result. Checked for a whole batch before any call of it is processed.
    pub max_tool_calls: u32,
    /// Wall-clock budget for the whole run. A value too large for the platform clock is clamped
    /// to [`MAXIMUM_RUN_TIME`] so the budget stays finite.
    pub total_time: Duration,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            total_time: DEFAULT_RUN_TIME,
        }
    }
}

impl AgentLimits {
    /// The documented default budgets.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Everything an agent needs to run, except the per-run credential.
///
/// The provider is injected and owns its own connection pool, endpoint and compatibility
/// settings; the agent never reads environment variables, files or a global registry. A
/// credential is supplied per run and is never part of this configuration.
pub struct AgentConfig {
    /// Provider that serves every request of this agent.
    pub provider: Arc<dyn Provider>,
    /// Model addressed by every request of this agent.
    pub model: Model,
    /// Instructions rendered as the leading system message. `None` sends no system prompt.
    pub system_prompt: Option<String>,
    /// Tools the run may execute. An empty registry declares no tools.
    pub tools: ToolRegistry,
    /// Optional derivation applied to a copy of the transcript before every request.
    pub context_transform: Option<Arc<dyn ContextTransform>>,
    /// Per-request provider options: sampling, deadlines, retries and the provider event buffer.
    pub options: RequestOptions,
    /// Finite budgets of every run.
    pub limits: AgentLimits,
    /// Buffered run events before a run waits for its consumer. At most [`MAX_EVENT_BUFFER`].
    pub event_buffer: NonZeroUsize,
    /// Initial transcript, oldest first. Saved runs append to it.
    pub history: Vec<Message>,
}

impl AgentConfig {
    /// A configuration with baseline options, no tools and no history.
    pub fn new(provider: Arc<dyn Provider>, model: Model) -> Self {
        Self {
            provider,
            model,
            system_prompt: None,
            tools: ToolRegistry::new(),
            context_transform: None,
            options: RequestOptions::default(),
            limits: AgentLimits::default(),
            event_buffer: DEFAULT_EVENT_BUFFER,
            history: Vec::new(),
        }
    }

    /// Set the system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Set the executable tools.
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Set the context transform.
    pub fn with_context_transform(mut self, transform: Arc<dyn ContextTransform>) -> Self {
        self.context_transform = Some(transform);
        self
    }

    /// Set the provider request options.
    pub fn with_options(mut self, options: RequestOptions) -> Self {
        self.options = options;
        self
    }

    /// Set the run budgets.
    pub fn with_limits(mut self, limits: AgentLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the run event buffer.
    pub fn with_event_buffer(mut self, buffer: NonZeroUsize) -> Self {
        self.event_buffer = buffer;
        self
    }

    /// Set the initial transcript.
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }
}

/// The provider is not printable, and transcripts and prompts are sensitive, so `Debug` reports
/// counts and the model identity only.
impl fmt::Debug for AgentConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentConfig")
            .field("provider", &"<injected>")
            .field("model", &self.model.id)
            .field("system_prompt", &self.system_prompt.is_some())
            .field("tools", &self.tools)
            .field("context_transform", &self.context_transform.is_some())
            .field("options", &self.options)
            .field("limits", &self.limits)
            .field("event_buffer", &self.event_buffer)
            .field("history_len", &self.history.len())
            .finish()
    }
}
