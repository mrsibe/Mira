//! Sessions: one operational transcript, an explicit model registry and caller-owned credentials
//! and context.
//!
//! A [`Session`] owns exactly one `mira-agent` [`Agent`] as its canonical operational transcript.
//! It is not a second copy of the transcript: the agent commits, repairs and replays the messages,
//! and the session adds the pieces the agent intentionally leaves to its caller — which model and
//! provider to run, where the credential comes from, what context to inject, and one bounded total
//! time budget that covers credential preparation, context assembly, the model run, event
//! forwarding and backpressure.
//!
//! Nothing here is durable and nothing is queued. A session runs at most one prompt at a time; a
//! concurrent prompt is refused with [`RuntimeError::Busy`] from before credential preparation
//! rather than being queued. There is no mid-run model change, no scheduler and no storage.
//!
//! [`SessionRun`] is returned immediately, so a caller can cancel a run before credential
//! preparation has finished. It forwards the child agent's immutable `AgentEvent`s through a
//! bounded channel and delivers exactly one terminal
//! `Result<RunOutcome, RuntimeError>` on a separate channel, mirroring the child agent's contract.

use std::fmt;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use mira_agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentLimits, AgentRun, ContextTransform, RunLimit,
    RunOutcome, Tool, ToolRegistry,
};
use mira_ai::{Credential, Message, ReasoningLevel, RequestOptions, Usage, UserMessage};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::context::{
    lock_usage, new_usage_slot, read_usage, BoundContextTransform, ContextManager, ContextUsage,
};
use crate::credential::{CredentialRequest, CredentialResolver};
use crate::error::RuntimeError;
use crate::registry::{ModelBinding, ModelRegistry};

/// Default total time budget for one session run.
pub const DEFAULT_RUN_TIME: Duration = Duration::from_secs(300);
/// Longest total time a run may be given. A larger budget is refused with
/// [`RuntimeError::InvalidRunTime`] rather than overflowing the monotonic clock.
pub const MAXIMUM_RUN_TIME: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 100);
/// Default capacity of the bounded run event channel.
pub const DEFAULT_EVENT_BUFFER: NonZeroUsize = NonZeroUsize::new(64).expect("64 is non-zero");
/// Largest run event buffer the runtime supports, taken from the runtime itself.
///
/// A larger buffer is refused with [`RuntimeError::InvalidEventBuffer`] before the run slot is
/// taken, instead of panicking in the channel constructor.
pub const MAX_EVENT_BUFFER: usize = mira_agent::MAX_EVENT_BUFFER;

/// Finite budgets of one session run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLimits {
    /// Wall-clock budget for the whole run: credential preparation, context assembly, the model
    /// run, event forwarding and any time spent waiting for a slow consumer.
    pub total_time: Duration,
    /// Buffered run events before the run waits for its consumer. At most [`MAX_EVENT_BUFFER`].
    pub event_buffer: NonZeroUsize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            total_time: DEFAULT_RUN_TIME,
            event_buffer: DEFAULT_EVENT_BUFFER,
        }
    }
}

impl RuntimeLimits {
    /// The documented default budgets.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Everything a session needs, except the per-run credential outcome.
///
/// The registry and resolver are injected and caller-owned. `tools` are the portable tool
/// implementations the child agent validates and executes; `context` composes caller-provided
/// snippets into each request. A credential is never part of this configuration.
pub struct SessionConfig {
    /// Immutable registry the session selects a binding from.
    pub registry: Arc<ModelRegistry>,
    /// Key of the initially selected binding.
    pub binding: String,
    /// Tool implementations declared to the model and executed by the child agent.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Optional context composition, applied to each request context.
    pub context: Option<Arc<ContextManager>>,
    /// Per-request options for this session. `None` uses the selected binding's options.
    pub options: Option<RequestOptions>,
    /// Requested reasoning effort for this session, overriding the effective options.
    pub thinking: Option<ReasoningLevel>,
    /// Instructions rendered as the leading system message.
    pub system_prompt: Option<String>,
    /// Finite budgets of the child agent run.
    pub limits: AgentLimits,
    /// Finite budgets of the session run.
    pub runtime: RuntimeLimits,
    /// Initial canonical transcript, oldest first.
    pub history: Vec<Message>,
    /// Credential lookup for every run that does not supply an explicit credential.
    pub credentials: Arc<dyn CredentialResolver>,
}

impl SessionConfig {
    /// A configuration with baseline options, no tools, no context and no history.
    pub fn new(
        registry: Arc<ModelRegistry>,
        binding: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self {
            registry,
            binding: binding.into(),
            tools: Vec::new(),
            context: None,
            options: None,
            thinking: None,
            system_prompt: None,
            limits: AgentLimits::default(),
            runtime: RuntimeLimits::default(),
            history: Vec::new(),
            credentials,
        }
    }

    /// Set the tool implementations.
    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the context composition.
    pub fn with_context(mut self, context: Arc<ContextManager>) -> Self {
        self.context = Some(context);
        self
    }

    /// Replace the per-request options of the session.
    pub fn with_options(mut self, options: RequestOptions) -> Self {
        self.options = Some(options);
        self
    }

    /// Set the requested reasoning effort.
    pub fn with_thinking(mut self, thinking: ReasoningLevel) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Set the system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Set the child agent run budgets.
    pub fn with_limits(mut self, limits: AgentLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the session run budgets.
    pub fn with_runtime(mut self, runtime: RuntimeLimits) -> Self {
        self.runtime = runtime;
        self
    }

    /// Set the initial canonical transcript.
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }
}

/// The registry, the injected resolver and the system prompt are not printable, and the transcript
/// is sensitive, so `Debug` reports the selection and counts only.
impl fmt::Debug for SessionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionConfig")
            .field("registry", &self.registry)
            .field("binding", &self.binding)
            .field("tools", &self.tools.len())
            .field("context", &self.context.is_some())
            .field("options", &self.options)
            .field("thinking", &self.thinking)
            .field("system_prompt", &self.system_prompt.is_some())
            .field("limits", &self.limits)
            .field("runtime", &self.runtime)
            .field("history_len", &self.history.len())
            .field("credentials", &"<injected>")
            .finish()
    }
}

/// A shared, cloneable handle to one operational session.
///
/// Clones share one transcript, one binding and one run slot; separate sessions are fully
/// independent. The inner agent is never exposed, so its invariants cannot be bypassed from the
/// outside: the session is the only place that rebuilds it, and only while it is idle.
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    /// Every field is guarded by one mutex, so a run snapshot and a configuration change cannot
    /// interleave. Shared with the run guard, which releases the slot when the run ends.
    state: Arc<Mutex<SessionState>>,
}

struct SessionState {
    config: SessionParts,
    agent: Agent,
    /// True from before credential preparation until the run has finished cleaning up.
    active: bool,
}

/// The reusable session configuration. Kept separate from the agent so the agent can be rebuilt
/// while preserving the transcript.
#[derive(Clone)]
struct SessionParts {
    registry: Arc<ModelRegistry>,
    binding: String,
    tools: Vec<Arc<dyn Tool>>,
    context: Option<Arc<ContextManager>>,
    /// Composition counts of this session only. A `ContextManager` may be shared by several
    /// sessions; the counts are not, so metrics never leak between them.
    usage: Arc<Mutex<ContextUsage>>,
    options: Option<RequestOptions>,
    thinking: Option<ReasoningLevel>,
    system_prompt: Option<String>,
    limits: AgentLimits,
    runtime: RuntimeLimits,
    credentials: Arc<dyn CredentialResolver>,
}

/// A snapshot of a session's operational state.
///
/// Not durable and not a second source of truth: the caller persists its own transcript and
/// rebuilds a session from it. Holds no credential.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    /// Selected binding key.
    pub binding: String,
    /// Whether a run is in progress or the child agent is still finishing its cleanup.
    pub busy: bool,
    /// Canonical transcript the session replays, oldest first.
    pub messages: Vec<Message>,
}

impl Session {
    /// Build a session from a configuration.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::UnknownBinding`] when the selected key is not registered and
    /// [`RuntimeError::InvalidRunTime`] or [`RuntimeError::InvalidEventBuffer`] when the run
    /// budgets are outside the supported bounds.
    pub fn new(config: SessionConfig) -> Result<Self, RuntimeError> {
        validate_run_time(config.runtime.total_time)?;
        validate_event_buffer(config.runtime.event_buffer)?;
        let usage = new_usage_slot();
        sync_usage(&usage, config.context.as_ref());
        let parts = SessionParts {
            registry: config.registry,
            binding: config.binding,
            tools: config.tools,
            context: config.context,
            usage,
            options: config.options,
            thinking: config.thinking,
            system_prompt: config.system_prompt,
            limits: config.limits,
            runtime: config.runtime,
            credentials: config.credentials,
        };
        let agent = build_agent(&parts, config.history)?;
        Ok(Self {
            inner: Arc::new(SessionInner {
                state: Arc::new(Mutex::new(SessionState {
                    config: parts,
                    agent,
                    active: false,
                })),
            }),
        })
    }

    /// Start a run for a text prompt, resolving the credential through the configured resolver.
    ///
    /// Returns as soon as the run is scheduled: the returned [`SessionRun`] owns the run's events,
    /// cancellation and terminal outcome, so the caller can cancel before credential preparation
    /// has finished.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::NoRuntime`] outside a Tokio runtime, [`RuntimeError::Busy`] when a
    /// run is already in progress, and the configuration errors of [`Session::new`].
    pub fn prompt(&self, text: impl Into<String>) -> Result<SessionRun, RuntimeError> {
        self.launch(UserMessage::text(text), None)
    }

    /// Start a run for a text prompt with an explicit credential, bypassing the resolver.
    ///
    /// This is the convenient path for callers that already hold a credential, for example a
    /// background inference pass. The credential is used for this run only and is never stored.
    ///
    /// # Errors
    ///
    /// The same errors as [`Session::prompt`].
    pub fn prompt_with_credential(
        &self,
        text: impl Into<String>,
        credential: Credential,
    ) -> Result<SessionRun, RuntimeError> {
        self.launch(UserMessage::text(text), Some(credential))
    }

    /// Start a run for a user message that may carry images, resolving the credential.
    ///
    /// # Errors
    ///
    /// The same errors as [`Session::prompt`].
    pub fn start(&self, prompt: UserMessage) -> Result<SessionRun, RuntimeError> {
        self.launch(prompt, None)
    }

    /// Start a run for a user message that may carry images with an explicit credential.
    ///
    /// # Errors
    ///
    /// The same errors as [`Session::prompt`].
    pub fn start_with_credential(
        &self,
        prompt: UserMessage,
        credential: Credential,
    ) -> Result<SessionRun, RuntimeError> {
        self.launch(prompt, Some(credential))
    }

    /// Snapshot of the selected binding, the busy state and the canonical transcript.
    pub fn snapshot(&self) -> SessionSnapshot {
        let state = lock_state(&self.inner.state);
        let agent = state.agent.snapshot();
        SessionSnapshot {
            binding: state.config.binding.clone(),
            busy: state.active || agent.busy,
            messages: agent.messages,
        }
    }

    /// Whether a run is in progress.
    ///
    /// Also true while the child agent is still finishing its cleanup, so the session never
    /// reports idle before the transcript it replays has been repaired.
    pub fn is_busy(&self) -> bool {
        let state = lock_state(&self.inner.state);
        state.active || state.agent.is_busy()
    }

    /// Key of the currently selected binding.
    pub fn binding(&self) -> String {
        lock_state(&self.inner.state).config.binding.clone()
    }

    /// Counts reported about the last context composition of this session and the last reported
    /// model usage.
    pub fn context_usage(&self) -> ContextUsage {
        let state = lock_state(&self.inner.state);
        let mut usage = read_usage(&state.config.usage);
        usage.model_usage = last_reported_usage(&state.agent.snapshot().messages);
        usage
    }

    /// Replace the selected binding of future runs.
    ///
    /// The transcript is preserved: the child agent is rebuilt from the saved configuration and
    /// the current snapshot, because the agent owns its transcript and has no provider setter. Runs
    /// of the new binding resolve their own credential and use the new binding's options.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Busy`] while a run is in progress and
    /// [`RuntimeError::UnknownBinding`] when the key is not registered.
    pub fn set_model(&self, binding: impl Into<String>) -> Result<(), RuntimeError> {
        let mut state = lock_state(&self.inner.state);
        ensure_idle(&state)?;
        let mut parts = state.config.clone();
        parts.binding = binding.into();
        let history = state.agent.snapshot().messages;
        let agent = build_agent(&parts, history)?;
        state.config = parts;
        state.agent = agent;
        Ok(())
    }

    /// Set the requested reasoning effort of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Busy`] while a run is in progress.
    pub fn set_thinking(&self, thinking: Option<ReasoningLevel>) -> Result<(), RuntimeError> {
        let mut state = lock_state(&self.inner.state);
        ensure_idle(&state)?;
        let binding = selected_binding(&state.config)?;
        let options = effective_options(&state.config, thinking, &binding);
        state.agent.set_options(options).map_err(map_agent_error)?;
        state.config.thinking = thinking;
        Ok(())
    }

    /// Replace the canonical transcript of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Busy`] while a run is in progress.
    pub fn set_history(&self, history: Vec<Message>) -> Result<(), RuntimeError> {
        let state = lock_state(&self.inner.state);
        ensure_idle(&state)?;
        state.agent.set_history(history).map_err(map_agent_error)
    }

    /// Replace the context composition of future runs.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Busy`] while a run is in progress.
    pub fn set_context_manager(
        &self,
        context: Option<Arc<ContextManager>>,
    ) -> Result<(), RuntimeError> {
        let mut state = lock_state(&self.inner.state);
        ensure_idle(&state)?;
        let binding = selected_binding(&state.config)?;
        let transform = context
            .as_ref()
            .map(|manager| bound_transform(manager, &binding, &state.config.usage));
        state
            .agent
            .set_context_transform(transform)
            .map_err(map_agent_error)?;
        state.config.context = context;
        sync_usage(&state.config.usage, state.config.context.as_ref());
        Ok(())
    }

    /// Take the run slot and snapshot everything the run needs.
    fn launch(
        &self,
        prompt: UserMessage,
        credential: Option<Credential>,
    ) -> Result<SessionRun, RuntimeError> {
        // The child agent and the run driver both need a runtime; refuse before taking the slot.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::NoRuntime)?;
        let (guard, cancellation, inputs, buffer) = {
            let mut state = lock_state(&self.inner.state);
            ensure_idle(&state)?;
            let runtime_limits = state.config.runtime;
            validate_run_time(runtime_limits.total_time)?;
            validate_event_buffer(runtime_limits.event_buffer)?;
            let binding = selected_binding(&state.config)?;
            let now = tokio::time::Instant::now();
            let deadline = now
                .checked_add(runtime_limits.total_time)
                .unwrap_or(now + MAXIMUM_RUN_TIME);
            let cancellation = CancellationToken::new();
            // The slot is taken before credential preparation, so a concurrent prompt is refused
            // even while the first run is still resolving a credential.
            state.active = true;
            let guard = RunGuard {
                state: self.inner.state.clone(),
                cancellation: cancellation.clone(),
            };
            let inputs = RunInputs {
                agent: state.agent.clone(),
                resolver: state.config.credentials.clone(),
                credential_request: CredentialRequest {
                    binding: binding.key().to_string(),
                    model: binding.model().clone(),
                    credential_id: binding.credential_id().clone(),
                },
                credential,
                cancellation: cancellation.clone(),
                deadline,
            };
            (guard, cancellation, inputs, runtime_limits.event_buffer)
        };

        let (events, event_receiver) = mpsc::channel(buffer.get());
        let (terminal, outcome) = oneshot::channel();
        // The guard exists before the task is spawned, so a task that is dropped before it runs
        // still releases the run slot.
        runtime.spawn(async move {
            let result = drive(inputs, prompt, events).await;
            drop(guard);
            let _ = terminal.send(result);
        });
        Ok(SessionRun::new(event_receiver, outcome, cancellation))
    }
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock_state(&self.inner.state);
        let agent = state.agent.snapshot();
        formatter
            .debug_struct("Session")
            .field("binding", &state.config.binding)
            .field("busy", &(state.active || agent.busy))
            .field("messages", &agent.messages.len())
            .field("bindings", &state.config.registry.len())
            .field("context_providers", &context_provider_count(&state.config))
            .finish()
    }
}

/// Handle of one session run: its event stream, its cancellation and its terminal outcome.
///
/// Dropping this handle — including dropping a half-polled [`SessionRun::outcome`] future —
/// cancels the run and therefore the child agent run and any credential resolution still in
/// progress.
pub struct SessionRun {
    events: mpsc::Receiver<AgentEvent>,
    terminal: oneshot::Receiver<Result<RunOutcome, RuntimeError>>,
    cancellation: CancellationToken,
}

impl SessionRun {
    fn new(
        events: mpsc::Receiver<AgentEvent>,
        terminal: oneshot::Receiver<Result<RunOutcome, RuntimeError>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            events,
            terminal,
            cancellation,
        }
    }

    /// Wait for the next child agent event.
    ///
    /// Returns `None` when no further events will arrive. The terminal outcome is delivered
    /// through [`SessionRun::outcome`], not through this iterator.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }

    /// Cancel the run. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Whether the run's cancellation signal is set.
    ///
    /// True after [`SessionRun::cancel`], and true once the run has ended for any reason: the token
    /// is also the run's lifetime signal.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Cancellation handle of this run, for callers that need to observe it.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Consume the run and return its terminal outcome.
    ///
    /// Pending events are discarded, but they are drained first so the run can finish publishing
    /// and clean up. The outcome arrives even when the consumer never read an event.
    ///
    /// Dropping this future before it resolves drops the run and therefore cancels it.
    pub async fn outcome(mut self) -> Result<RunOutcome, RuntimeError> {
        loop {
            tokio::select! {
                biased;
                event = self.events.recv() => {
                    if event.is_none() {
                        break;
                    }
                }
                outcome = &mut self.terminal => {
                    return outcome.unwrap_or(Err(RuntimeError::Internal));
                }
            }
        }
        (&mut self.terminal)
            .await
            .unwrap_or(Err(RuntimeError::Internal))
    }
}

impl Drop for SessionRun {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl fmt::Debug for SessionRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRun")
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Releases the run slot when a run ends, for every exit path.
struct RunGuard {
    state: Arc<Mutex<SessionState>>,
    cancellation: CancellationToken,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Signal the run's lifetime end first, so work that watches the token stops.
        self.cancellation.cancel();
        let mut state = lock_state(&self.state);
        // Only the session slot is released here. Whether the child agent is still finishing its
        // own cleanup is reported by `SessionState::agent::is_busy`, so the session never claims
        // idle before the transcript it replays has been repaired.
        state.active = false;
    }
}

/// Everything one run needs, snapshotted before the task is spawned.
struct RunInputs {
    agent: Agent,
    resolver: Arc<dyn CredentialResolver>,
    credential_request: CredentialRequest,
    credential: Option<Credential>,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
}

/// Resolve the credential, run the child agent and forward its events to the consumer.
///
/// The credential preparation runs under the same cancellation and total deadline as the rest of
/// the run. The child agent is always driven to a terminal outcome — normally by reading its
/// outcome, and on every other exit by cancelling it and draining its outcome — so the run's
/// `AgentRun` is never abandoned and the next prompt of the same session is safe.
async fn drive(
    inputs: RunInputs,
    prompt: UserMessage,
    events: mpsc::Sender<AgentEvent>,
) -> Result<RunOutcome, RuntimeError> {
    let RunInputs {
        agent,
        resolver,
        credential_request,
        credential,
        cancellation,
        deadline,
    } = inputs;

    let credential = match credential {
        Some(credential) => credential,
        None => {
            let resolved = limited(
                &cancellation,
                deadline,
                // Gate synchronous preparation as well as polling the returned future.
                async {
                    resolver
                        .resolve(&credential_request, cancellation.clone())
                        .await
                },
            )
            .await?;
            resolved.map_err(|_| RuntimeError::Credential)?
        }
    };
    if cancellation.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(RuntimeError::Timeout);
    }

    let mut child = agent.start(prompt, credential).map_err(map_agent_error)?;
    match forward(&events, &mut child, &cancellation, deadline).await {
        // The child's event stream ended, so its terminal outcome is imminent. It is still read
        // under the run's cancellation and total deadline, so a stuck producer cannot outlive the
        // run; dropping the outcome future cancels the child, which cleans up its own transcript.
        Ok(()) => match limited(&cancellation, deadline, child.outcome()).await {
            Ok(result) => result.map_err(map_agent_error),
            Err(error) => Err(error),
        },
        Err(error) => {
            child.cancel();
            // Wait for the child's own cleanup, so the transcript is repaired before the run slot
            // is released. The cancellation token is already set, so this returns promptly.
            let _ = child.outcome().await;
            Err(error)
        }
    }
}

/// Forward the child agent's events under the run's cancellation and total deadline.
async fn forward(
    events: &mpsc::Sender<AgentEvent>,
    child: &mut AgentRun,
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<(), RuntimeError> {
    loop {
        let event = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
            () = tokio::time::sleep_until(deadline) => return Err(RuntimeError::Timeout),
            event = child.recv() => event,
        };
        let Some(event) = event else {
            return Ok(());
        };
        let sent = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
            () = tokio::time::sleep_until(deadline) => return Err(RuntimeError::Timeout),
            result = events.send(event) => result,
        };
        if sent.is_err() {
            // The consumer dropped its run handle.
            return Err(RuntimeError::Cancelled);
        }
    }
}

/// Await one step under the run's cancellation token and its total deadline.
async fn limited<T>(
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
    future: impl Future<Output = T>,
) -> Result<T, RuntimeError> {
    if cancellation.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    // Tokio timers have coarser resolution than Instant; an expired deadline
    // must refuse work even before sleep_until becomes ready.
    if tokio::time::Instant::now() >= deadline {
        return Err(RuntimeError::Timeout);
    }
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(RuntimeError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(RuntimeError::Timeout),
        value = future => Ok(value),
    }
}

/// Build the child agent of a session, preserving the transcript it is given.
fn build_agent(parts: &SessionParts, history: Vec<Message>) -> Result<Agent, RuntimeError> {
    let binding = selected_binding(parts)?;
    let mut tools = ToolRegistry::new();
    for tool in &parts.tools {
        tools.insert(tool.clone()).map_err(RuntimeError::Agent)?;
    }
    let options = effective_options(parts, parts.thinking, &binding);
    let mut config = AgentConfig::new(binding.provider().clone(), binding.model().clone())
        .with_tools(tools)
        .with_options(options)
        .with_limits(parts.limits)
        .with_event_buffer(parts.runtime.event_buffer)
        .with_history(history);
    if let Some(prompt) = parts.system_prompt.clone() {
        config = config.with_system_prompt(prompt);
    }
    if let Some(manager) = parts.context.as_ref() {
        config = config.with_context_transform(bound_transform(manager, &binding, &parts.usage));
    }
    Ok(Agent::new(config))
}

/// The context transform of a manager, bound to the model selected for the run and to this
/// session's own counts slot.
fn bound_transform(
    manager: &Arc<ContextManager>,
    binding: &Arc<ModelBinding>,
    usage: &Arc<Mutex<ContextUsage>>,
) -> Arc<dyn ContextTransform> {
    Arc::new(BoundContextTransform::new(
        manager.clone(),
        binding.model().clone(),
        usage.clone(),
    ))
}

/// Effective per-request options of one run: the session override or the binding defaults, with
/// the session's requested reasoning effort applied.
fn effective_options(
    parts: &SessionParts,
    thinking: Option<ReasoningLevel>,
    binding: &Arc<ModelBinding>,
) -> RequestOptions {
    let mut options = parts
        .options
        .clone()
        .unwrap_or_else(|| binding.options().clone());
    if let Some(level) = thinking {
        options.reasoning = Some(level);
    }
    options
}

/// Resolve the selected binding of the session configuration.
fn selected_binding(parts: &SessionParts) -> Result<Arc<ModelBinding>, RuntimeError> {
    parts
        .registry
        .get(&parts.binding)
        .cloned()
        .ok_or_else(|| RuntimeError::UnknownBinding(parts.binding.clone()))
}

/// Refuse a run or a configuration change while the session is occupied.
///
/// The child agent's own busy flag is part of the check: after a run task is dropped, the child
/// may still be repairing its transcript, and the session must not claim to be idle until that
/// cleanup settles.
fn ensure_idle(state: &SessionState) -> Result<(), RuntimeError> {
    if state.active || state.agent.is_busy() {
        return Err(RuntimeError::Busy);
    }
    Ok(())
}

/// The last usage a provider actually reported, if any.
fn last_reported_usage(messages: &[Message]) -> Option<Usage> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => assistant.usage,
        _ => None,
    })
}

fn context_provider_count(parts: &SessionParts) -> usize {
    parts
        .context
        .as_ref()
        .map(|manager| manager.providers().len())
        .unwrap_or(0)
}

/// Reset this session's composition counts to match a context configuration.
///
/// The counts describe the session, not the manager, so replacing or removing the composition
/// clears what the previous one reported and reports the new ceiling immediately.
fn sync_usage(slot: &Arc<Mutex<ContextUsage>>, context: Option<&Arc<ContextManager>>) {
    *lock_usage(slot) = ContextUsage {
        model_usage: None,
        injected_chars: 0,
        injected_items: 0,
        max_injected_chars: context
            .map(|manager| manager.max_injected_chars())
            .unwrap_or(0),
    };
}

fn validate_run_time(total_time: Duration) -> Result<(), RuntimeError> {
    if total_time > MAXIMUM_RUN_TIME {
        return Err(RuntimeError::InvalidRunTime {
            requested: total_time,
            maximum: MAXIMUM_RUN_TIME,
        });
    }
    Ok(())
}

fn validate_event_buffer(buffer: NonZeroUsize) -> Result<(), RuntimeError> {
    if buffer.get() > MAX_EVENT_BUFFER {
        return Err(RuntimeError::InvalidEventBuffer {
            requested: buffer.get(),
            maximum: MAX_EVENT_BUFFER,
        });
    }
    Ok(())
}

/// Map a child agent failure to a runtime category, keeping no provider prose.
fn map_agent_error(error: AgentError) -> RuntimeError {
    match error {
        AgentError::Cancelled => RuntimeError::Cancelled,
        AgentError::Busy => RuntimeError::Busy,
        AgentError::NoRuntime => RuntimeError::NoRuntime,
        AgentError::ContextTransform => RuntimeError::Context,
        AgentError::Limit(RunLimit::Time) => RuntimeError::Timeout,
        other => RuntimeError::Agent(other),
    }
}

/// Lock session state, ignoring poisoning: no lock is held across an await, so a panic cannot
/// leave this state half-updated.
fn lock_state(state: &Mutex<SessionState>) -> MutexGuard<'_, SessionState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
