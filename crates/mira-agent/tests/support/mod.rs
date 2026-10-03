//! Offline test doubles: a scripted provider, scripted tools and event helpers.
//!
//! Nothing here touches a network, a database or a keyring.
//!
//! Every integration test binary includes this module and uses a subset of it.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mira_agent::mira_ai::{
    AiError, Api, AssistantContent, AssistantEmitter, AssistantEvent, AssistantMessage,
    AssistantSource, BlockKind, Model, Provider, StopReason, StreamHandle, StreamRequest,
    TextBlock, ToolCall, ToolDefinition,
};
use mira_agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentRun, CancellationToken, Message, RunOutcome,
    Tool, ToolFuture, ToolRegistry, ToolResult,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Buffer of the fake provider's own event channel.
const PROVIDER_BUFFER: NonZeroUsize = NonZeroUsize::new(4).unwrap();

/// Model every test run addresses.
pub fn test_model() -> Model {
    Model::new(Api::OpenAiCompletions, "test-provider", "test-model")
}

/// An agent with a system prompt and the given tools.
pub fn agent(provider: Arc<FakeProvider>, tools: ToolRegistry) -> Agent {
    Agent::new(
        AgentConfig::new(provider, test_model())
            .with_system_prompt("be brief")
            .with_tools(tools),
    )
}

/// A registry holding every given tool.
pub fn registry(tools: Vec<Arc<dyn Tool>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.insert(tool).expect("tool registers");
    }
    registry
}

/// Source of a scripted assistant message.
pub fn test_source() -> AssistantSource {
    AssistantSource {
        api: Api::OpenAiCompletions,
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        response_model: None,
        response_id: None,
    }
}

/// An assistant message with one text block.
pub fn text_message(text: &str, stop: StopReason) -> AssistantMessage {
    AssistantMessage {
        source: test_source(),
        content: vec![AssistantContent::Text(TextBlock::new(text))],
        stop_reason: stop,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// A tool call whose arguments parsed into an object.
pub fn parsed_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments_raw: arguments.to_string(),
        arguments: Some(arguments),
        signature: None,
    }
}

/// A tool call whose arguments never parsed, for example because the provider cut it off.
pub fn unparsed_call(id: &str, name: &str, arguments_raw: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments_raw: arguments_raw.to_string(),
        arguments: None,
        signature: None,
    }
}

/// An assistant message that requests the given calls.
pub fn call_message(calls: Vec<ToolCall>, stop: StopReason) -> AssistantMessage {
    AssistantMessage {
        source: test_source(),
        content: calls.into_iter().map(AssistantContent::ToolCall).collect(),
        stop_reason: stop,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// An assistant message with one text block and the given calls.
pub fn mixed_message(text: &str, calls: Vec<ToolCall>, stop: StopReason) -> AssistantMessage {
    let mut content = vec![AssistantContent::Text(TextBlock::new(text))];
    content.extend(calls.into_iter().map(AssistantContent::ToolCall));
    AssistantMessage {
        source: test_source(),
        content,
        stop_reason: stop,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// What the scripted provider does for one request.
pub enum Script {
    /// Publish the full event sequence of the message and finish with it.
    Stream(AssistantMessage),
    /// Publish the terminal result only: no start event and no deltas.
    Silent(AssistantMessage),
    /// Publish `Start`, one text block with one delta, then stall until the stream is dropped.
    Partial(AssistantMessage),
    /// Publish `Start` and stall until the stream is dropped.
    Stall,
    /// Fail synchronously, as a transport does before a request is sent.
    SetupFailure,
    /// Finish with a provider error.
    Failure(AiError),
    /// Drop the producer without finishing, so the consumer sees an ended stream and no terminal.
    Abandon,
    /// Panic while the request is being set up, as a broken provider implementation would.
    Panic,
}

/// A provider that answers requests from a script, in order.
pub struct FakeProvider {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<StreamRequest>>,
    stall_signals: Mutex<Vec<oneshot::Receiver<()>>>,
}

impl FakeProvider {
    /// A provider with the given script, consumed one entry per request.
    pub fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
            stall_signals: Mutex::new(Vec::new()),
        }
    }

    /// Requests this provider received, in order.
    pub fn requests(&self) -> Vec<StreamRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// How many requests this provider received.
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Take the next signal that completes when a stalled stream observes cancellation.
    pub fn take_stall_signal(&self) -> Option<oneshot::Receiver<()>> {
        self.stall_signals.lock().unwrap().pop()
    }
}

impl Provider for FakeProvider {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
        let source = AssistantSource {
            api: request.model.api,
            provider: request.model.provider.clone(),
            model: request.model.id.clone(),
            response_model: None,
            response_id: None,
        };
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Script::Stream(text_message("", StopReason::EndTurn)));
        self.requests.lock().unwrap().push(request);

        match script {
            Script::Panic => panic!("scripted provider panic"),
            Script::SetupFailure => Err(AiError::InvalidRequest(
                "scripted setup failure".to_string(),
            )),
            Script::Failure(error) => {
                let (mut producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                producer.finish(Err(error));
                Ok(handle)
            }
            Script::Abandon => {
                let (producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                drop(producer);
                Ok(handle)
            }
            Script::Silent(message) => {
                let (mut producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                producer.finish(Ok(message));
                Ok(handle)
            }
            Script::Stream(message) => {
                let (mut producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                tokio::spawn(async move {
                    for event in block_events(&message) {
                        if producer.emit(event).await.is_err() {
                            return;
                        }
                    }
                    producer.finish(Ok(message));
                });
                Ok(handle)
            }
            Script::Partial(message) => {
                let (producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                let (sender, receiver) = oneshot::channel();
                self.stall_signals.lock().unwrap().push(receiver);
                let first = message
                    .content
                    .iter()
                    .find_map(|block| match block {
                        AssistantContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                tokio::spawn(async move {
                    let started = producer
                        .emit(AssistantEvent::Start {
                            source: source.clone(),
                        })
                        .await
                        .is_ok()
                        && producer
                            .emit(AssistantEvent::BlockStart {
                                index: 0,
                                kind: BlockKind::Text,
                            })
                            .await
                            .is_ok()
                        && producer
                            .emit(AssistantEvent::BlockDelta {
                                index: 0,
                                delta: first.into(),
                            })
                            .await
                            .is_ok();
                    if started {
                        producer.cancellation().cancelled().await;
                    }
                    drop(producer);
                    let _ = sender.send(());
                });
                Ok(handle)
            }
            Script::Stall => {
                let (producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                let (sender, receiver) = oneshot::channel();
                self.stall_signals.lock().unwrap().push(receiver);
                tokio::spawn(async move {
                    if producer
                        .emit(AssistantEvent::Start {
                            source: source.clone(),
                        })
                        .await
                        .is_ok()
                    {
                        producer.cancellation().cancelled().await;
                    }
                    drop(producer);
                    let _ = sender.send(());
                });
                Ok(handle)
            }
        }
    }
}

/// The immutable events a well-behaved provider publishes for a message.
pub fn block_events(message: &AssistantMessage) -> Vec<AssistantEvent> {
    let mut events = vec![AssistantEvent::Start {
        source: message.source.clone(),
    }];
    for (index, block) in message.content.iter().enumerate() {
        let (kind, delta) = match block {
            AssistantContent::Text(text) => (BlockKind::Text, text.text.clone()),
            AssistantContent::Thinking(thinking) => {
                (BlockKind::Thinking, thinking.thinking.clone())
            }
            AssistantContent::ToolCall(call) => (BlockKind::ToolCall, call.arguments_raw.clone()),
            _ => continue,
        };
        events.push(AssistantEvent::BlockStart { index, kind });
        if !delta.is_empty() {
            events.push(AssistantEvent::BlockDelta {
                index,
                delta: delta.into(),
            });
        }
        events.push(AssistantEvent::BlockEnd {
            index,
            block: block.clone(),
        });
    }
    events
}

/// Drain a run's events and return its terminal outcome.
///
/// Reading events to the end also proves that event delivery ends no later than the outcome.
pub async fn finish(mut run: AgentRun) -> (Vec<AgentEvent>, Result<RunOutcome, AgentError>) {
    let mut events = Vec::new();
    while let Some(event) = run.recv().await {
        events.push(event);
    }
    let outcome = run.outcome().await;
    (events, outcome)
}

/// Unwrap a completed outcome.
pub fn expect_completed(outcome: Result<RunOutcome, AgentError>) -> AssistantMessage {
    match outcome {
        Ok(RunOutcome::Completed { message }) => message,
        other => panic!("expected a completed run, got {other:?}"),
    }
}

/// Unwrap a truncated outcome.
pub fn expect_truncated(outcome: Result<RunOutcome, AgentError>) -> AssistantMessage {
    match outcome {
        Ok(RunOutcome::Truncated { message }) => message,
        other => panic!("expected a truncated run, got {other:?}"),
    }
}

/// Unwrap a failed outcome.
pub fn expect_error(outcome: Result<RunOutcome, AgentError>) -> AgentError {
    match outcome {
        Err(error) => error,
        Ok(outcome) => panic!("expected a failed run, got {outcome:?}"),
    }
}

/// Compact event names, for asserting a lifecycle sequence.
pub fn kinds(events: &[AgentEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            AgentEvent::RunStart => "run-start",
            AgentEvent::TurnStart { .. } => "turn-start",
            AgentEvent::MessageStart {
                message: Message::User(_),
                ..
            } => "user-start",
            AgentEvent::MessageStart {
                message: Message::Assistant(_),
                ..
            } => "assistant-start",
            AgentEvent::MessageStart {
                message: Message::ToolResult(_),
                ..
            } => "tool-result-start",
            AgentEvent::MessageUpdate { .. } => "update",
            AgentEvent::MessageEnd {
                message: Message::User(_),
                ..
            } => "user-end",
            AgentEvent::MessageEnd {
                message: Message::Assistant(_),
                ..
            } => "assistant-end",
            AgentEvent::MessageEnd {
                message: Message::ToolResult(_),
                ..
            } => "tool-result-end",
            AgentEvent::ToolStart { .. } => "tool-start",
            AgentEvent::ToolEnd { .. } => "tool-end",
            AgentEvent::TurnEnd { .. } => "turn-end",
            _ => "other",
        })
        .collect()
}

/// The committed tool results of every turn of a run, in order.
pub fn tool_results(events: &[AgentEvent]) -> Vec<mira_agent::mira_ai::ToolResultMessage> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd {
                message: Message::ToolResult(result),
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .collect()
}

/// The requests a provider received, as contexts.
pub fn contexts(provider: &FakeProvider) -> Vec<mira_agent::mira_ai::Context> {
    provider
        .requests()
        .into_iter()
        .map(|request| request.context)
        .collect()
}

/// The text of a failed tool result.
pub fn result_text(result: &mira_agent::mira_ai::ToolResultMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|content| match content {
            mira_agent::mira_ai::InputContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// A tool that records every validated call and computes `a op b`.
pub struct Calculator {
    definition: ToolDefinition,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl Default for Calculator {
    fn default() -> Self {
        Self::new()
    }
}

impl Calculator {
    /// A calculator with `a`, `b` and an optional `op` of `add` or `sub`.
    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "calculator",
                "Add or subtract two numbers.",
                json!({
                    "type": "object",
                    "properties": {
                        "a": { "type": "number" },
                        "b": { "type": "number" },
                        "op": { "type": "string", "enum": ["add", "sub"] }
                    },
                    "required": ["a", "b"],
                    "additionalProperties": false
                }),
            ),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Arguments of every executed call, in execution order.
    pub fn calls(&self) -> Arc<Mutex<Vec<Value>>> {
        Arc::clone(&self.calls)
    }
}

impl Tool for Calculator {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(&'a self, arguments: Value, _cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(arguments.clone());
            let a = arguments
                .get("a")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let b = arguments
                .get("b")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let value = if arguments.get("op").and_then(Value::as_str) == Some("sub") {
                a - b
            } else {
                a + b
            };
            ToolResult::text(format!("{value}"))
        })
    }
}

/// A tool that reports failure without panicking.
pub struct FailingTool {
    definition: ToolDefinition,
}

impl Default for FailingTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FailingTool {
    /// A tool that always returns an error result.
    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "failing",
                "Always fails.",
                json!({ "type": "object", "properties": {} }),
            ),
        }
    }
}

impl Tool for FailingTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(
        &'a self,
        _arguments: Value,
        _cancellation: CancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { ToolResult::error("scripted tool failure") })
    }
}

/// A tool that panics while executing.
pub struct PanickingTool {
    definition: ToolDefinition,
}

impl Default for PanickingTool {
    fn default() -> Self {
        Self::new()
    }
}

impl PanickingTool {
    /// The text of the panic, which must never reach a result, an event or an error.
    pub const PANIC_TEXT: &'static str = "PANIC-PAYLOAD-PRIVATE";

    /// A tool that panics on every call.
    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "panicking",
                "Panics.",
                json!({ "type": "object", "properties": {} }),
            ),
        }
    }
}

impl Tool for PanickingTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(
        &'a self,
        _arguments: Value,
        _cancellation: CancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            panic!("{}", Self::PANIC_TEXT);
        })
    }
}

/// A tool that never finishes and reports when its cancellation was observed.
pub struct StallingTool {
    definition: ToolDefinition,
    signal: Mutex<Option<oneshot::Sender<()>>>,
    invocations: Arc<AtomicUsize>,
}

impl StallingTool {
    /// A tool that stalls, plus the signal to await its cancellation.
    pub fn new() -> (Self, oneshot::Receiver<()>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self {
                definition: ToolDefinition::new(
                    "stalling",
                    "Never finishes.",
                    json!({ "type": "object", "properties": {} }),
                ),
                signal: Mutex::new(Some(sender)),
                invocations: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    /// How often this tool was invoked.
    pub fn invocations(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.invocations)
    }
}

impl Tool for StallingTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(&'a self, _arguments: Value, cancellation: CancellationToken) -> ToolFuture<'a> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let signal = self.signal.lock().unwrap().take();
        Box::pin(async move {
            if let Some(sender) = signal {
                tokio::spawn(async move {
                    cancellation.cancelled().await;
                    let _ = sender.send(());
                });
            }
            std::future::pending::<ToolResult>().await
        })
    }
}

/// A tool whose [`Tool::execute`] method itself panics, before it returns a future.
pub struct SyncPanickingTool {
    definition: ToolDefinition,
}

impl Default for SyncPanickingTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncPanickingTool {
    /// The text of the panic, which must never reach a result, an event or an error.
    pub const PANIC_TEXT: &'static str = "SYNC-PANIC-PAYLOAD-PRIVATE";

    /// A tool that panics in the method body.
    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "sync-panicking",
                "Panics while being invoked.",
                json!({ "type": "object", "properties": {} }),
            ),
        }
    }
}

impl Tool for SyncPanickingTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(
        &'a self,
        _arguments: Value,
        _cancellation: CancellationToken,
    ) -> ToolFuture<'a> {
        panic!("{}", Self::PANIC_TEXT);
    }
}

/// A tool whose declaration is supplied by the test and whose call always succeeds.
pub struct SchemaTool {
    definition: ToolDefinition,
}

impl SchemaTool {
    /// A tool declared by `definition` that returns the text `ok`.
    pub fn new(definition: ToolDefinition) -> Self {
        Self { definition }
    }
}

impl Tool for SchemaTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(
        &'a self,
        _arguments: Value,
        _cancellation: CancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { ToolResult::text("ok") })
    }
}

/// A context transform that records the canonical context of every request and returns a derived
/// context without touching the input.
pub struct RecordingTransform {
    calls: Arc<Mutex<Vec<mira_agent::Context>>>,
    derive: Box<dyn Fn(&mira_agent::Context) -> mira_agent::Context + Send + Sync>,
}

impl RecordingTransform {
    /// A transform that derives a context from the canonical one.
    pub fn new(
        derive: impl Fn(&mira_agent::Context) -> mira_agent::Context + Send + Sync + 'static,
    ) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            derive: Box::new(derive),
        }
    }

    /// The canonical contexts this transform was asked to derive from, in order.
    pub fn calls(&self) -> Arc<Mutex<Vec<mira_agent::Context>>> {
        Arc::clone(&self.calls)
    }
}

impl mira_agent::ContextTransform for RecordingTransform {
    fn transform<'a>(
        &'a self,
        context: &'a mira_agent::Context,
        cancellation: CancellationToken,
    ) -> mira_agent::ContextTransformFuture<'a> {
        self.calls.lock().unwrap().push(context.clone());
        let derived = (self.derive)(context);
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(mira_agent::ContextTransformError);
            }
            Ok(derived)
        })
    }
}

/// A context transform that never finishes.
pub struct StallingTransform;

impl mira_agent::ContextTransform for StallingTransform {
    fn transform<'a>(
        &'a self,
        _context: &'a mira_agent::Context,
        _cancellation: CancellationToken,
    ) -> mira_agent::ContextTransformFuture<'a> {
        Box::pin(std::future::pending())
    }
}

/// A context transform that always fails.
pub struct FailingTransform;

impl mira_agent::ContextTransform for FailingTransform {
    fn transform<'a>(
        &'a self,
        _context: &'a mira_agent::Context,
        _cancellation: CancellationToken,
    ) -> mira_agent::ContextTransformFuture<'a> {
        Box::pin(async { Err(mira_agent::ContextTransformError) })
    }
}

/// A context transform that cancels its own run and still returns a context.
pub struct SelfCancellingTransform;

impl mira_agent::ContextTransform for SelfCancellingTransform {
    fn transform<'a>(
        &'a self,
        context: &'a mira_agent::Context,
        cancellation: CancellationToken,
    ) -> mira_agent::ContextTransformFuture<'a> {
        cancellation.cancel();
        let derived = context.clone();
        Box::pin(async move { Ok(derived) })
    }
}

/// A context transform that panics on its `panic_on`th invocation, after earlier turns succeeded.
pub struct PanickingTransform {
    calls: AtomicUsize,
    panic_on: usize,
}

impl PanickingTransform {
    /// A transform that panics once it reaches `panic_on` calls.
    pub fn new(panic_on: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            panic_on,
        }
    }
}

impl mira_agent::ContextTransform for PanickingTransform {
    fn transform<'a>(
        &'a self,
        context: &'a mira_agent::Context,
        _cancellation: CancellationToken,
    ) -> mira_agent::ContextTransformFuture<'a> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call >= self.panic_on {
            panic!("TRANSFORM-PANIC-PAYLOAD-PRIVATE");
        }
        let derived = context.clone();
        Box::pin(async move { Ok(derived) })
    }
}

/// Whether every assistant turn that requests calls is followed by exactly the ordered results of
/// those calls: one result per call, in the order the calls were made.
pub fn calls_are_paired_in_order(messages: &[Message]) -> bool {
    let mut index = 0;
    while index < messages.len() {
        let Message::Assistant(assistant) = &messages[index] else {
            index += 1;
            continue;
        };
        let calls: Vec<&str> = assistant
            .tool_calls()
            .map(|call| call.id.as_str())
            .collect();
        if calls.is_empty() {
            index += 1;
            continue;
        }
        let results: Vec<&str> = messages[index + 1..]
            .iter()
            .take_while(|message| matches!(message, Message::ToolResult(_)))
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        if results != calls {
            return false;
        }
        index += 1 + results.len();
    }
    true
}

/// Every tool call of a transcript with the number of results that answer it, in call order.
pub fn result_counts(messages: &[Message]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for message in messages {
        if let Message::Assistant(assistant) = message {
            counts.extend(assistant.tool_calls().map(|call| (call.id.clone(), 0)));
        }
    }
    for message in messages {
        if let Message::ToolResult(result) = message {
            if let Some(entry) = counts.iter_mut().find(|(id, _)| id == &result.tool_call_id) {
                entry.1 += 1;
            }
        }
    }
    counts
}
