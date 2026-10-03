//! The bounded agent loop: one provider request per turn, sequential tool execution, immutable
//! events.
//!
//! The loop is a translation of Pi's `packages/agent/src/agent-loop.ts` with Rust ownership and
//! explicit finite limits. It keeps the invariants that matter: a completed assistant turn is
//! committed before it is acted on, tool calls execute only after a `StopReason::ToolUse`
//! terminal, a truncated turn never executes a call, and every tool call is answered by a result
//! in the order the model requested it.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use mira_ai::{
    AiError, AssistantContent, AssistantEvent, AssistantMessage, Context, Credential, InputContent,
    Message, Model, Provider, RequestOptions, StopReason, StreamRequest, ToolCall,
    ToolResultMessage, UserMessage,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{lock_inner, AgentInner};
use crate::config::AgentLimits;
use crate::error::{AgentError, ProviderFailure, RunLimit};
use crate::event::AgentEvent;
use crate::run::RunOutcome;
use crate::state::PendingToolCall;
use crate::tool::{ArgumentCheck, ToolRegistry, ToolResult};
use crate::transform::ContextTransform;

/// Everything one run needs, snapshotted when it starts.
///
/// The snapshot is immutable for the whole run, so a configuration change can never affect a
/// running turn: setters are refused while the agent is busy.
pub(crate) struct RunContext {
    pub(crate) state: Arc<Mutex<AgentInner>>,
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) model: Model,
    pub(crate) system_prompt: Option<String>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) context_transform: Option<Arc<dyn ContextTransform>>,
    pub(crate) options: RequestOptions,
    pub(crate) limits: AgentLimits,
    /// Credential of this run only. It is never written to state, an event, an error or `Debug`.
    pub(crate) credential: Credential,
    pub(crate) cancellation: CancellationToken,
    pub(crate) deadline: tokio::time::Instant,
}

/// Run one prompt to its terminal outcome.
///
/// The terminal outcome is published exactly once, after run state is cleared and event delivery
/// has ended, so a consumer that sees the outcome also sees an idle agent.
///
/// `guard` is created by [`Agent::start`](crate::Agent::start) and moved here, so a task that is
/// dropped before it ever runs still clears the busy flag.
pub(crate) async fn run(
    context: RunContext,
    guard: RunGuard,
    history: Vec<Message>,
    prompt: UserMessage,
    events: mpsc::Sender<AgentEvent>,
    terminal: oneshot::Sender<Result<RunOutcome, AgentError>>,
) {
    let sink = EventSink {
        events,
        cancellation: context.cancellation.clone(),
        deadline: context.deadline,
    };
    let result = match AssertUnwindSafe(execute(&context, history, prompt, &sink))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        // A panic inside a provider, tool or transform is contained and reported as a category;
        // the payload (which can hold arbitrary content) is dropped.
        Err(_payload) => Err(AgentError::Internal),
    };
    drop(sink);
    drop(guard);
    let _ = terminal.send(result);
}

/// Clears run-owned state when the run ends, whatever the reason: normal completion, an error,
/// cancellation, the deadline, a contained panic, or a task that is dropped before it runs.
///
/// Cleanup signals the run's cancellation token first, so provider, tool and context-transform
/// work that watches it stops before the transcript is finalized, and finalizes the transcript
/// before the agent becomes idle.
pub(crate) struct RunGuard {
    state: Arc<Mutex<AgentInner>>,
    cancellation: CancellationToken,
    /// Length of the transcript when the run started: everything after it is this run's output.
    checkpoint: usize,
}

impl RunGuard {
    /// Take responsibility for finishing one run.
    pub(crate) fn new(
        state: Arc<Mutex<AgentInner>>,
        cancellation: CancellationToken,
        checkpoint: usize,
    ) -> Self {
        Self {
            state,
            cancellation,
            checkpoint,
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let mut inner = lock_inner(&self.state);
        finalize_transcript(&mut inner.messages, self.checkpoint);
        inner.partial = None;
        inner.pending_tool_calls.clear();
        inner.busy = false;
    }
}

/// Make the transcript this run appended replay-safe before the agent becomes idle.
///
/// A run can stop at any point — cancellation, deadline, budget, provider failure, panic or a
/// dropped task — and canonical history must never contain an assistant turn whose tool calls
/// have no results, because that turn would be replayed to the provider as an unanswered call.
/// Only the messages this run appended are repaired; imported history is the caller's to
/// validate.
///
/// A turn that cannot be replayed at all — duplicate or missing call identifiers, or a failed
/// turn whose calls a request encoder drops — is removed together with only the results that
/// answer its own calls. A replayable turn keeps every result it already has, and each call
/// without one gets a constant error result saying that its outcome is unknown. Nothing here
/// executes a tool: cleanup only repairs history.
fn finalize_transcript(messages: &mut Vec<Message>, checkpoint: usize) {
    if checkpoint >= messages.len() {
        return;
    }
    let mut index = checkpoint;
    while index < messages.len() {
        let Some((unreplayable, calls)) = turn_calls(&messages[index]) else {
            index += 1;
            continue;
        };
        index = if unreplayable {
            remove_turn(messages, index, &calls)
        } else {
            fill_missing_results(messages, index, &calls)
        };
    }
}

/// The calls of a committed assistant turn that may need repair.
///
/// Returns `None` for a turn that requests no calls, and `Some((unreplayable, calls))` otherwise.
/// `unreplayable` marks a turn that must not stay in replayable history at all.
fn turn_calls(message: &Message) -> Option<(bool, Vec<(String, String)>)> {
    let Message::Assistant(assistant) = message else {
        return None;
    };
    let calls: Vec<(String, String)> = assistant
        .tool_calls()
        .map(|call| (call.id.clone(), call.name.clone()))
        .collect();
    if calls.is_empty() {
        return None;
    }
    // A failed turn is dropped when a request is encoded, so its calls can never be paired and
    // synthesized results would be orphaned tool messages.
    let unreplayable = assistant.stop_reason == StopReason::Failed || !pairable_call_ids(&calls);
    Some((unreplayable, calls))
}

/// Whether every call of a turn has a distinct, non-empty identifier.
fn pairable_call_ids(calls: &[(String, String)]) -> bool {
    let mut seen: Vec<&str> = Vec::with_capacity(calls.len());
    for (id, _) in calls {
        if id.is_empty() || seen.contains(&id.as_str()) {
            return false;
        }
        seen.push(id);
    }
    true
}

/// Remove a turn that cannot be replayed, and only the results that answer its own calls.
fn remove_turn(messages: &mut Vec<Message>, index: usize, calls: &[(String, String)]) -> usize {
    messages.remove(index);
    let ids: Vec<&str> = calls.iter().map(|(id, _)| id.as_str()).collect();
    let mut cursor = index;
    while cursor < messages.len() {
        match &messages[cursor] {
            Message::ToolResult(result) if ids.contains(&result.tool_call_id.as_str()) => {
                messages.remove(cursor);
            }
            Message::ToolResult(_) => cursor += 1,
            _ => break,
        }
    }
    cursor
}

/// Add a constant error result for every call of a replayable turn that has no result yet.
///
/// Existing results keep their position, and the synthesized ones follow them in call order, so
/// every call ends up with exactly one ordered result.
fn fill_missing_results(
    messages: &mut Vec<Message>,
    index: usize,
    calls: &[(String, String)],
) -> usize {
    let mut end = index + 1;
    while end < messages.len() && matches!(messages[end], Message::ToolResult(_)) {
        end += 1;
    }
    let present: Vec<String> = messages[index + 1..end]
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let mut insert = end;
    for (id, name) in calls {
        if present.contains(id) {
            continue;
        }
        messages.insert(insert, Message::ToolResult(interruption_result(id, name)));
        insert += 1;
    }
    insert
}

/// Result text for a call whose outcome the run never observed.
///
/// It neither claims the call ran nor claims it did not: the run ended while the call was
/// pending, so repeating it may repeat a side effect.
fn interruption_result(call_id: &str, name: &str) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call_id.to_string(),
        tool_name: name.to_string(),
        content: vec![InputContent::text(format!(
            "Tool call \"{name}\" did not report a result: the run ended before this call completed, so whether it ran is unknown. Re-issue it only if repeating it is safe."
        ))],
        is_error: true,
    }
}

/// Publication half of the run's bounded event channel.
struct EventSink {
    events: mpsc::Sender<AgentEvent>,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
}

impl EventSink {
    /// Publish one event, waiting for buffer space.
    ///
    /// Every event is delivered (the channel is lossless), but the wait is bounded by the run's
    /// cancellation token and deadline, so a consumer that stops reading can never keep a run
    /// alive past its budget.
    async fn send(&self, event: AgentEvent) -> Result<(), AgentError> {
        let sent = limited(&self.cancellation, self.deadline, self.events.send(event)).await?;
        sent.map_err(|_| AgentError::Cancelled)
    }
}

/// Await one step of the run under its cancellation token and its total deadline.
async fn limited<T>(
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
    future: impl Future<Output = T>,
) -> Result<T, AgentError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(AgentError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(AgentError::Limit(RunLimit::Time)),
        value = future => Ok(value),
    }
}

/// Why a tool call was refused without executing it.
#[derive(Clone, Copy)]
enum Refusal {
    /// The response hit the model's output token limit, so arguments may be truncated.
    Truncated,
    /// The turn did not stop for tool use, so no call of it is authorized.
    NotAuthorized,
}

impl Refusal {
    fn describe(self, tool: &str) -> String {
        let reason = match self {
            Self::Truncated => {
                "the response hit the output token limit, so its arguments may be truncated"
            }
            Self::NotAuthorized => "the assistant turn did not stop for tool use",
        };
        format!("Tool call \"{tool}\" was not executed: {reason}. Re-issue the call with complete arguments.")
    }
}

async fn execute(
    context: &RunContext,
    history: Vec<Message>,
    prompt: UserMessage,
    sink: &EventSink,
) -> Result<RunOutcome, AgentError> {
    // The working transcript mirrors the agent's committed transcript. It grows only at commit
    // points, never per streamed delta.
    let mut transcript = history;
    let mut processed_calls: u32 = 0;
    let mut turn: u32 = 1;

    sink.send(AgentEvent::RunStart).await?;
    sink.send(AgentEvent::TurnStart { turn }).await?;
    commit_message(context, &mut transcript, Message::User(prompt), turn, sink).await?;

    loop {
        if turn > context.limits.max_turns || turn == u32::MAX {
            return Err(AgentError::Limit(RunLimit::Turns));
        }
        let message = request_turn(context, &mut transcript, turn, sink).await?;
        // `Some(outcome)` ends the run here; `None` asks for another turn.
        if let Some(outcome) = handle_turn(
            context,
            &mut transcript,
            &message,
            turn,
            &mut processed_calls,
            sink,
        )
        .await?
        {
            return Ok(outcome);
        }
        turn = turn.saturating_add(1);
        sink.send(AgentEvent::TurnStart { turn }).await?;
    }
}

/// Run one provider request, relay its streamed events, and commit its assistant message.
async fn request_turn(
    context: &RunContext,
    transcript: &mut Vec<Message>,
    turn: u32,
    sink: &EventSink,
) -> Result<AssistantMessage, AgentError> {
    let mut request_context = Context {
        system_prompt: context.system_prompt.clone(),
        messages: transcript.clone(),
        tools: context.tools.definitions(),
    };
    if let Some(transform) = context.context_transform.as_ref() {
        // The transform sees an immutable copy, so it cannot change the canonical transcript.
        let derived = limited(
            &context.cancellation,
            context.deadline,
            transform.transform(&request_context, context.cancellation.clone()),
        )
        .await?;
        request_context = derived.map_err(|_| AgentError::ContextTransform)?;
    }

    let request = StreamRequest {
        model: context.model.clone(),
        context: request_context,
        credential: context.credential.clone(),
        options: context.options.clone(),
    };
    // Provider setup runs inside the same cancellation and deadline gate as every other step, so
    // a run that is already cancelled — for example by a transform that cancelled it — never
    // starts a request.
    let stream = limited(&context.cancellation, context.deadline, async {
        context.provider.stream(request)
    })
    .await?;
    let mut stream = stream.map_err(|_| AgentError::Provider(ProviderFailure::Setup))?;

    let mut started = false;
    loop {
        let Some(update) = limited(&context.cancellation, context.deadline, stream.recv()).await?
        else {
            break;
        };
        if let AssistantEvent::Start { source } = &update {
            started = true;
            let partial = AssistantMessage {
                source: source.clone(),
                content: Vec::new(),
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: None,
                error_message: None,
            };
            set_partial(context, Some(partial.clone()));
            sink.send(AgentEvent::MessageStart {
                turn,
                message: Message::Assistant(partial),
            })
            .await?;
        } else {
            apply_update(context, &update);
            sink.send(AgentEvent::MessageUpdate { turn, update })
                .await?;
        }
    }

    // The terminal is authoritative even when the provider published no delta at all.
    let message = limited(&context.cancellation, context.deadline, stream.result())
        .await?
        .map_err(map_provider_error)?;

    set_partial(context, None);
    let committed = Message::Assistant(message.clone());
    commit(context, transcript, committed.clone());
    if !started {
        sink.send(AgentEvent::MessageStart {
            turn,
            message: committed.clone(),
        })
        .await?;
    }
    sink.send(AgentEvent::MessageEnd {
        turn,
        message: committed,
    })
    .await?;
    Ok(message)
}

/// Act on a committed assistant turn.
///
/// Returns the terminal outcome when the run ends here, or `None` when another turn follows.
///
/// Every call of a committed turn is processed: executed for an authorized tool-use turn, and
/// answered by a refusal result otherwise. The whole batch is therefore reserved against the
/// finite call budget before anything happens, so refusals cannot bypass the budget.
async fn handle_turn(
    context: &RunContext,
    transcript: &mut Vec<Message>,
    message: &AssistantMessage,
    turn: u32,
    processed_calls: &mut u32,
    sink: &EventSink,
) -> Result<Option<RunOutcome>, AgentError> {
    let calls: Vec<&ToolCall> = message.tool_calls().collect();
    if !calls.is_empty() {
        // Duplicate or missing identifiers cannot be paired with results, and no call from an
        // ambiguous turn may execute. The turn cannot stay in replayable history either; run
        // cleanup removes it.
        ensure_unique_call_ids(&calls)?;
    }
    if message.stop_reason == StopReason::Failed {
        // A failed turn is never replayed and never processed.
        return Err(AgentError::Provider(ProviderFailure::FailedTurn));
    }
    reserve_calls(context, calls.len(), processed_calls)?;

    match message.stop_reason {
        StopReason::ToolUse if calls.is_empty() => {
            end_turn(message, Vec::new(), turn, sink).await?;
            Ok(Some(RunOutcome::Completed {
                message: message.clone(),
            }))
        }
        StopReason::ToolUse => {
            execute_calls(context, transcript, message, &calls, turn, sink).await?;
            Ok(None)
        }
        StopReason::EndTurn if calls.is_empty() => {
            end_turn(message, Vec::new(), turn, sink).await?;
            Ok(Some(RunOutcome::Completed {
                message: message.clone(),
            }))
        }
        StopReason::EndTurn => {
            // The model declared its turn over while requesting calls. Fail closed: answer every
            // call with an error result so the transcript keeps ordered call/result pairs, then
            // let the bounded follow-up turn re-issue complete calls.
            let refusal = Refusal::NotAuthorized;
            let results = refuse_calls(context, transcript, &calls, refusal, turn, sink).await?;
            end_turn(message, results, turn, sink).await?;
            Ok(None)
        }
        StopReason::MaxTokens if calls.is_empty() => {
            end_turn(message, Vec::new(), turn, sink).await?;
            Ok(Some(RunOutcome::Truncated {
                message: message.clone(),
            }))
        }
        StopReason::MaxTokens => {
            // Arguments of a truncated turn are never salvaged or executed.
            let refusal = Refusal::Truncated;
            let results = refuse_calls(context, transcript, &calls, refusal, turn, sink).await?;
            end_turn(message, results, turn, sink).await?;
            Ok(None)
        }
        // Handled above, before the batch is reserved: a failed turn is neither replayed nor
        // processed.
        StopReason::Failed => Err(AgentError::Provider(ProviderFailure::FailedTurn)),
    }
}

/// Reserve a whole batch of calls against the finite call budget.
///
/// The budget covers every call the run processes, so a turn that refuses its calls consumes it
/// exactly like a turn that executes them. The batch is reserved in one step: a batch that does
/// not fit fails before any call of it is processed.
fn reserve_calls(
    context: &RunContext,
    count: usize,
    processed_calls: &mut u32,
) -> Result<(), AgentError> {
    if count == 0 {
        return Ok(());
    }
    let batch = u32::try_from(count).unwrap_or(u32::MAX);
    if processed_calls.saturating_add(batch) > context.limits.max_tool_calls {
        return Err(AgentError::Limit(RunLimit::ToolCalls));
    }
    *processed_calls = processed_calls.saturating_add(batch);
    Ok(())
}

/// Execute a batch of authorized tool calls sequentially, in the order they were requested.
async fn execute_calls(
    context: &RunContext,
    transcript: &mut Vec<Message>,
    message: &AssistantMessage,
    calls: &[&ToolCall],
    turn: u32,
    sink: &EventSink,
) -> Result<(), AgentError> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        push_pending(context, call);
        sink.send(AgentEvent::ToolStart {
            turn,
            call_id: call.id.clone(),
            tool: call.name.clone(),
        })
        .await?;
        let result = run_tool(context, call).await?;
        let result_message = ToolResultMessage {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: result.content,
            is_error: result.is_error,
        };
        // The call is over: commit the result that actually happened and clear the pending state
        // before the first publication await. A run interrupted while publishing must keep a
        // side effect that already occurred.
        pop_pending(context, &call.id);
        commit(
            context,
            transcript,
            Message::ToolResult(result_message.clone()),
        );
        sink.send(AgentEvent::ToolEnd {
            turn,
            call_id: call.id.clone(),
            tool: call.name.clone(),
            is_error: result_message.is_error,
        })
        .await?;
        publish_message(sink, turn, Message::ToolResult(result_message.clone())).await?;
        results.push(result_message);
    }
    end_turn(message, results, turn, sink).await
}

/// Answer calls that must not execute with error results, keeping the order of the calls.
async fn refuse_calls(
    context: &RunContext,
    transcript: &mut Vec<Message>,
    calls: &[&ToolCall],
    refusal: Refusal,
    turn: u32,
    sink: &EventSink,
) -> Result<Vec<ToolResultMessage>, AgentError> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        let result_message = ToolResultMessage {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: vec![InputContent::text(refusal.describe(&call.name))],
            is_error: true,
        };
        // The refusal is decided before anything is published, so it is committed first: an
        // interrupted run keeps the pair instead of losing it to a blocked publication.
        commit(
            context,
            transcript,
            Message::ToolResult(result_message.clone()),
        );
        sink.send(AgentEvent::ToolStart {
            turn,
            call_id: call.id.clone(),
            tool: call.name.clone(),
        })
        .await?;
        sink.send(AgentEvent::ToolEnd {
            turn,
            call_id: call.id.clone(),
            tool: call.name.clone(),
            is_error: true,
        })
        .await?;
        publish_message(sink, turn, Message::ToolResult(result_message.clone())).await?;
        results.push(result_message);
    }
    Ok(results)
}

/// Execute one validated call, containing panics and reporting refusals as failed results.
async fn run_tool(context: &RunContext, call: &ToolCall) -> Result<ToolResult, AgentError> {
    match context.tools.check(&call.name, call.arguments.as_ref()) {
        ArgumentCheck::Unknown => Ok(ToolResult::error(format!(
            "Tool call \"{}\" was not executed: no tool with that name is registered.",
            call.name
        ))),
        ArgumentCheck::Malformed => Ok(ToolResult::error(format!(
            "Tool call \"{}\" was not executed: its arguments are not a complete JSON object.",
            call.name
        ))),
        ArgumentCheck::SchemaMismatch => Ok(ToolResult::error(format!(
            "Tool call \"{}\" was not executed: its arguments do not match the tool schema.",
            call.name
        ))),
        ArgumentCheck::Valid(tool) => {
            let arguments = call.arguments.clone().unwrap_or(Value::Null);
            let name = call.name.clone();
            let cancellation = context.cancellation.clone();
            // The tool is invoked inside the contained future, so a panic in the method itself —
            // not only in the future it returns — becomes one generic paired error result. The
            // invocation also happens only after the limiter has checked established cancellation.
            let execution =
                AssertUnwindSafe(async move { tool.execute(arguments, cancellation).await })
                    .catch_unwind();
            match limited(&context.cancellation, context.deadline, execution).await? {
                Ok(result) => Ok(result),
                // The panic payload is arbitrary content and is deliberately not reported. It is
                // not suppressed either: the process panic hook still runs.
                Err(_panic) => Ok(ToolResult::error(format!("Tool call \"{name}\" failed."))),
            }
        }
    }
}

/// Commit a message to the working transcript and to agent state.
///
/// Both grow only here: streamed deltas never clone the transcript.
fn commit(context: &RunContext, transcript: &mut Vec<Message>, message: Message) {
    transcript.push(message.clone());
    lock_inner(&context.state).messages.push(message);
}

/// Commit a message, then publish its complete message lifecycle.
async fn commit_message(
    context: &RunContext,
    transcript: &mut Vec<Message>,
    message: Message,
    turn: u32,
    sink: &EventSink,
) -> Result<(), AgentError> {
    commit(context, transcript, message.clone());
    publish_message(sink, turn, message).await
}

/// Publish the complete message lifecycle of a message that is already committed.
async fn publish_message(sink: &EventSink, turn: u32, message: Message) -> Result<(), AgentError> {
    sink.send(AgentEvent::MessageStart {
        turn,
        message: message.clone(),
    })
    .await?;
    sink.send(AgentEvent::MessageEnd { turn, message }).await
}

/// Publish the end of a turn with its ordered tool results.
async fn end_turn(
    message: &AssistantMessage,
    results: Vec<ToolResultMessage>,
    turn: u32,
    sink: &EventSink,
) -> Result<(), AgentError> {
    sink.send(AgentEvent::TurnEnd {
        turn,
        message: message.clone(),
        tool_results: results,
    })
    .await
}

/// Reject a turn whose tool-call identifiers cannot be paired with results.
fn ensure_unique_call_ids(calls: &[&ToolCall]) -> Result<(), AgentError> {
    let owned: Vec<(String, String)> = calls
        .iter()
        .map(|call| (call.id.clone(), call.name.clone()))
        .collect();
    if pairable_call_ids(&owned) {
        Ok(())
    } else {
        Err(AgentError::AmbiguousToolCalls)
    }
}

/// Map a provider failure to a category, keeping no provider text.
fn map_provider_error(error: AiError) -> AgentError {
    match error {
        AiError::Cancelled => AgentError::Cancelled,
        AiError::Timeout(_) => AgentError::Provider(ProviderFailure::Timeout),
        _ => AgentError::Provider(ProviderFailure::Stream),
    }
}

/// Start the assistant turn being streamed, or clear it once the turn is committed.
fn set_partial(context: &RunContext, partial: Option<AssistantMessage>) {
    lock_inner(&context.state).partial = partial;
}

/// Fold one immutable provider update into the partial assistant turn.
///
/// The update is applied in place under a short lock, so the snapshot stays current without
/// cloning a growing message per delta. The partial is display state; the committed message is
/// the provider's terminal result.
fn apply_update(context: &RunContext, update: &AssistantEvent) {
    let mut inner = lock_inner(&context.state);
    let Some(partial) = inner.partial.as_mut() else {
        return;
    };
    match update {
        AssistantEvent::BlockStart { index, kind } => {
            if partial.content.len() == *index {
                partial.content.push(empty_block(*kind));
            }
        }
        AssistantEvent::BlockDelta { index, delta } => {
            if let Some(block) = partial.content.get_mut(*index) {
                match block {
                    AssistantContent::Text(text) => text.text.push_str(delta),
                    AssistantContent::Thinking(thinking) => thinking.thinking.push_str(delta),
                    AssistantContent::ToolCall(call) => call.arguments_raw.push_str(delta),
                    _ => {}
                }
            }
        }
        AssistantEvent::BlockEnd { index, block } => {
            if let Some(slot) = partial.content.get_mut(*index) {
                *slot = block.clone();
            }
        }
        _ => {}
    }
}

fn empty_block(kind: mira_ai::BlockKind) -> AssistantContent {
    match kind {
        mira_ai::BlockKind::Text => AssistantContent::Text(mira_ai::TextBlock::new("")),
        mira_ai::BlockKind::Thinking => AssistantContent::Thinking(mira_ai::ThinkingBlock {
            thinking: String::new(),
            signature: None,
            redacted: false,
        }),
        mira_ai::BlockKind::ToolCall => AssistantContent::ToolCall(ToolCall {
            id: String::new(),
            name: String::new(),
            arguments_raw: String::new(),
            arguments: None,
            signature: None,
        }),
    }
}

/// Record a call as executing.
fn push_pending(context: &RunContext, call: &ToolCall) {
    lock_inner(&context.state)
        .pending_tool_calls
        .push(PendingToolCall {
            call_id: call.id.clone(),
            name: call.name.clone(),
        });
}

/// Clear a call from the pending set.
fn pop_pending(context: &RunContext, call_id: &str) {
    lock_inner(&context.state)
        .pending_tool_calls
        .retain(|pending| pending.call_id != call_id);
}
