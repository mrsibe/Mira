//! The shared agent handle: operational state, configuration and run startup.

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use mira_ai::{Credential, Message, Model, Provider, RequestOptions, UserMessage};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{self, RunContext, RunGuard};
use crate::config::{AgentConfig, AgentLimits, MAXIMUM_RUN_TIME, MAX_EVENT_BUFFER};
use crate::error::AgentError;
use crate::run::AgentRun;
use crate::state::{AgentState, PendingToolCall};
use crate::tool::ToolRegistry;
use crate::transform::ContextTransform;

/// State one agent owns. Every field is guarded by the single agent mutex, so a run snapshot and
/// a configuration change cannot interleave.
pub(crate) struct AgentInner {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) model: Model,
    pub(crate) system_prompt: Option<String>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) context_transform: Option<Arc<dyn ContextTransform>>,
    pub(crate) options: RequestOptions,
    pub(crate) limits: AgentLimits,
    pub(crate) event_buffer: NonZeroUsize,
    pub(crate) messages: Vec<Message>,
    pub(crate) partial: Option<mira_ai::AssistantMessage>,
    pub(crate) pending_tool_calls: Vec<PendingToolCall>,
    pub(crate) busy: bool,
}

/// Lock agent state, ignoring poisoning: a panic cannot leave this state half-updated because no
/// lock is ever held across an await.
pub(crate) fn lock_inner(inner: &Mutex<AgentInner>) -> MutexGuard<'_, AgentInner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A shared, cloneable handle to one agent.
///
/// Clones share the same state: one run at a time, one transcript, one configuration. Separate
/// agents are fully independent.
///
/// The agent is operational state, not a durable source. It holds the transcript in memory and
/// appends committed turns to it; persisting and restoring a session is the caller's job.
pub struct Agent {
    inner: Arc<Mutex<AgentInner>>,
}

impl Agent {
    /// Create an agent from a [`AgentConfig`].
    pub fn new(config: AgentConfig) -> Self {
        let AgentConfig {
            provider,
            model,
            system_prompt,
            tools,
            context_transform,
            options,
            limits,
            event_buffer,
            history,
        } = config;
        Self {
            inner: Arc::new(Mutex::new(AgentInner {
                provider,
                model,
                system_prompt,
                tools: Arc::new(tools),
                context_transform,
                options,
                limits,
                event_buffer,
                messages: history,
                partial: None,
                pending_tool_calls: Vec::new(),
                busy: false,
            })),
        }
    }

    /// Start a run with a text prompt and an explicit credential.
    ///
    /// The credential is used for every request of this run only; it is never stored in agent
    /// state, in an event, in an error or in a `Debug` rendering.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NoRuntime`] outside a Tokio runtime and [`AgentError::Busy`] when a
    /// run is already in progress.
    pub fn prompt(
        &self,
        text: impl Into<String>,
        credential: Credential,
    ) -> Result<AgentRun, AgentError> {
        self.start(UserMessage::text(text), credential)
    }

    /// Start a run with a user message that may carry images.
    ///
    /// Returns as soon as the run is scheduled: the returned [`AgentRun`] owns the run's events,
    /// cancellation and terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::NoRuntime`] outside a Tokio runtime, [`AgentError::Busy`] when a run
    /// is already in progress, and [`AgentError::InvalidEventBuffer`] when the configured event
    /// buffer is larger than the runtime supports. Every fallible startup step happens before the
    /// run slot is taken, so a refused start leaves the agent idle and usable.
    pub fn start(
        &self,
        prompt: UserMessage,
        credential: Credential,
    ) -> Result<AgentRun, AgentError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| AgentError::NoRuntime)?;
        let (context, guard, history, buffer) = {
            let mut inner = lock_inner(&self.inner);
            if inner.busy {
                return Err(AgentError::Busy);
            }
            let buffer = inner.event_buffer;
            check_event_buffer(buffer)?;
            let now = tokio::time::Instant::now();
            let deadline = now
                .checked_add(inner.limits.total_time)
                .unwrap_or(now + MAXIMUM_RUN_TIME);
            let cancellation = CancellationToken::new();
            // The run appends to the transcript from here on; everything before is imported
            // history the caller owns, and everything after is finalized when the run ends.
            let checkpoint = inner.messages.len();
            inner.busy = true;
            let history = inner.messages.clone();
            let context = RunContext {
                state: self.inner.clone(),
                provider: inner.provider.clone(),
                model: inner.model.clone(),
                system_prompt: inner.system_prompt.clone(),
                tools: inner.tools.clone(),
                context_transform: inner.context_transform.clone(),
                options: inner.options.clone(),
                limits: inner.limits,
                credential,
                cancellation: cancellation.clone(),
                deadline,
            };
            let guard = RunGuard::new(self.inner.clone(), cancellation, checkpoint);
            (context, guard, history, buffer)
        };

        let (events, event_receiver) = mpsc::channel(buffer.get());
        let (terminal, outcome) = oneshot::channel();
        let cancellation = context.cancellation.clone();

        // The guard exists before the task is spawned, so the busy flag is cleared and the
        // transcript finalized even if the runtime drops the task before it is ever polled.
        runtime.spawn(async move {
            agent_loop::run(context, guard, history, prompt, events, terminal).await;
        });

        Ok(AgentRun::new(event_receiver, outcome, cancellation))
    }

    /// Snapshot of the current transcript, partial turn, busy flag and pending tool calls.
    pub fn snapshot(&self) -> AgentState {
        let inner = lock_inner(&self.inner);
        AgentState {
            messages: inner.messages.clone(),
            partial: inner.partial.clone(),
            busy: inner.busy,
            pending_tool_calls: inner.pending_tool_calls.clone(),
        }
    }

    /// Whether a run is in progress.
    pub fn is_busy(&self) -> bool {
        lock_inner(&self.inner).busy
    }

    /// Replace the model of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress; a run never swaps its model.
    pub fn set_model(&self, model: Model) -> Result<(), AgentError> {
        self.configure(|inner| inner.model = model)
    }

    /// Replace the system prompt of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn set_system_prompt(&self, prompt: Option<String>) -> Result<(), AgentError> {
        self.configure(|inner| inner.system_prompt = prompt)
    }

    /// Replace the executable tools of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress; a run keeps the registry it
    /// started with.
    pub fn set_tools(&self, tools: ToolRegistry) -> Result<(), AgentError> {
        self.configure(|inner| inner.tools = Arc::new(tools))
    }

    /// Replace the context transform of future requests.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn set_context_transform(
        &self,
        transform: Option<Arc<dyn ContextTransform>>,
    ) -> Result<(), AgentError> {
        self.configure(|inner| inner.context_transform = transform)
    }

    /// Replace the provider request options of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn set_options(&self, options: RequestOptions) -> Result<(), AgentError> {
        self.configure(|inner| inner.options = options)
    }

    /// Replace the run budgets of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn set_limits(&self, limits: AgentLimits) -> Result<(), AgentError> {
        self.configure(|inner| inner.limits = limits)
    }

    /// Replace the run event buffer of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress, and
    /// [`AgentError::InvalidEventBuffer`] when the buffer is larger than [`MAX_EVENT_BUFFER`].
    pub fn set_event_buffer(&self, buffer: NonZeroUsize) -> Result<(), AgentError> {
        check_event_buffer(buffer)?;
        self.configure(|inner| inner.event_buffer = buffer)
    }

    /// Append a message to the transcript, for example a tool result the caller produced itself.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn append_message(&self, message: Message) -> Result<(), AgentError> {
        self.configure(|inner| inner.messages.push(message))
    }

    /// Replace the whole transcript.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn set_history(&self, history: Vec<Message>) -> Result<(), AgentError> {
        self.configure(|inner| inner.messages = history)
    }

    /// Drop the transcript and start the next run from an empty history.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Busy`] while a run is in progress.
    pub fn clear_history(&self) -> Result<(), AgentError> {
        self.configure(|inner| inner.messages.clear())
    }

    /// Apply a configuration change, refusing it while a run is in progress.
    fn configure<T>(&self, change: impl FnOnce(&mut AgentInner) -> T) -> Result<T, AgentError> {
        let mut inner = lock_inner(&self.inner);
        if inner.busy {
            return Err(AgentError::Busy);
        }
        Ok(change(&mut inner))
    }
}

impl Clone for Agent {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Reject an event buffer the runtime cannot construct a channel for.
fn check_event_buffer(buffer: NonZeroUsize) -> Result<(), AgentError> {
    if buffer.get() > MAX_EVENT_BUFFER {
        return Err(AgentError::InvalidEventBuffer {
            requested: buffer.get(),
            maximum: MAX_EVENT_BUFFER,
        });
    }
    Ok(())
}

/// Reports the model identity and counts. Transcript contents and prompts are deliberately not
/// printed.
impl fmt::Debug for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = lock_inner(&self.inner);
        formatter
            .debug_struct("Agent")
            .field("model", &inner.model.id)
            .field("busy", &inner.busy)
            .field("messages", &inner.messages.len())
            .field("tools", &inner.tools.len())
            .field("pending_tool_calls", &inner.pending_tool_calls.len())
            .finish()
    }
}
