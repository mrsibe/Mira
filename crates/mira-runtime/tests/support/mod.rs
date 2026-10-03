//! Offline test doubles: scripted provider, resolver, context provider and tools.
//!
//! Nothing here touches a network, a database or a keyring.
//!
//! Every integration test binary includes this module and uses a subset of it.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mira_runtime::mira_ai::{
    AiError, Api, AssistantContent, AssistantEmitter, AssistantEvent, AssistantMessage,
    AssistantSource, BlockKind, Model, Provider, StopReason, StreamHandle, StreamRequest,
    TextBlock, ToolCall, ToolDefinition, Usage,
};
use mira_runtime::{
    CancellationToken, ContextError, ContextItem, ContextProvider, ContextProviderFuture,
    ContextRequest, Credential, CredentialError, CredentialFuture, CredentialId, CredentialRequest,
    CredentialResolver, Message, ModelBinding, ModelRegistry, Session, Tool, ToolFuture,
    ToolRegistry, ToolResult,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Buffer of the fake provider's own event channel.
const PROVIDER_BUFFER: NonZeroUsize = NonZeroUsize::new(4).expect("4 is non-zero");

/// Model every test binding addresses unless it declares another.
pub fn test_model() -> Model {
    Model::new(Api::OpenAiCompletions, "test-provider", "test-model")
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
pub fn text_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        source: test_source(),
        content: vec![AssistantContent::Text(TextBlock::new(text))],
        stop_reason: StopReason::EndTurn,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// An assistant message with one text block and reported usage.
pub fn text_message_with_usage(text: &str, usage: Usage) -> AssistantMessage {
    let mut message = text_message(text);
    message.usage = Some(usage);
    message
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

/// An assistant message that requests the given calls.
pub fn call_message(text: &str, calls: Vec<ToolCall>) -> AssistantMessage {
    let mut content = vec![AssistantContent::Text(TextBlock::new(text))];
    content.extend(calls.into_iter().map(AssistantContent::ToolCall));
    AssistantMessage {
        source: test_source(),
        content,
        stop_reason: StopReason::ToolUse,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    }
}

/// What the scripted provider does for one request.
pub enum Script {
    /// Publish the full event sequence of the message and finish with it.
    Stream(AssistantMessage),
    /// Publish many deltas and then stall until the stream is cancelled.
    Flood(usize),
    /// Stall after `Start` until the stream is cancelled.
    Stall,
    /// Finish with a provider error.
    Failure(AiError),
    /// Fail synchronously, as a transport does before a request is sent.
    SetupFailure,
}

/// A provider that answers requests from a script, in order, and records them.
pub struct FakeProvider {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<StreamRequest>>,
}

impl FakeProvider {
    /// A provider with the given script, consumed one entry per request.
    pub fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Requests this provider received, in order.
    pub fn requests(&self) -> Vec<StreamRequest> {
        self.requests.lock().expect("lock").clone()
    }

    /// Contexts of the requests this provider received, in order.
    pub fn contexts(&self) -> Vec<mira_runtime::mira_ai::Context> {
        self.requests()
            .into_iter()
            .map(|request| request.context)
            .collect()
    }

    /// How many requests this provider received.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("lock").len()
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
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| Script::Stream(text_message("empty script")));
        self.requests.lock().expect("lock").push(request);

        match script {
            Script::SetupFailure => Err(AiError::InvalidRequest(
                "scripted setup failure".to_string(),
            )),
            Script::Failure(error) => {
                let (mut producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                producer.finish(Err(error));
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
            Script::Flood(deltas) => {
                let (producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
                tokio::spawn(async move {
                    if producer
                        .emit(AssistantEvent::Start {
                            source: source.clone(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if producer
                        .emit(AssistantEvent::BlockStart {
                            index: 0,
                            kind: BlockKind::Text,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    for _ in 0..deltas {
                        if producer
                            .emit(AssistantEvent::BlockDelta {
                                index: 0,
                                delta: "d".into(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    producer.cancellation().cancelled().await;
                });
                Ok(handle)
            }
            Script::Stall => {
                let (producer, handle) = AssistantEmitter::new(PROVIDER_BUFFER);
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

/// What the scripted resolver does for one run.
pub enum ResolveScript {
    /// Return this credential.
    Credential(&'static str),
    /// Return the constant failure.
    Failure,
    /// Stall until the run is cancelled.
    Stall,
    /// Panic while constructing the resolution future.
    Panic,
    /// Panic when the resolution future is polled.
    PolledPanic,
}

/// A resolver that answers runs from a script, in order, and records them.
pub struct FakeResolver {
    scripts: Mutex<VecDeque<ResolveScript>>,
    requests: Mutex<Vec<CredentialRequest>>,
    polled: Arc<AtomicUsize>,
    cancellation_seen: Arc<AtomicUsize>,
}

impl Default for FakeResolver {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl FakeResolver {
    /// A resolver with the given script, consumed one entry per run.
    pub fn new(scripts: Vec<ResolveScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
            polled: Arc::new(AtomicUsize::new(0)),
            cancellation_seen: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Resolutions this resolver was asked for, in order.
    pub fn requests(&self) -> Vec<CredentialRequest> {
        self.requests.lock().expect("lock").clone()
    }

    /// How often a stalled resolution future was first polled.
    pub fn polled_count(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.polled)
    }

    /// How often a stalled resolution observed cancellation.
    pub fn cancellation_count(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.cancellation_seen)
    }
}

impl CredentialResolver for FakeResolver {
    fn resolve<'a>(
        &'a self,
        request: &'a CredentialRequest,
        cancellation: CancellationToken,
    ) -> CredentialFuture<'a> {
        let script = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or(ResolveScript::Failure);
        self.requests.lock().expect("lock").push(request.clone());
        match script {
            ResolveScript::Panic => panic!("RESOLVER-PANIC-PRIVATE"),
            ResolveScript::PolledPanic => {
                Box::pin(async { panic!("RESOLVER-POLLED-PANIC-PRIVATE") })
            }
            ResolveScript::Credential(secret) => {
                Box::pin(async move { Ok(Credential::new(secret)) })
            }
            ResolveScript::Failure => Box::pin(async move { Err(CredentialError) }),
            ResolveScript::Stall => {
                let seen = Arc::clone(&self.cancellation_seen);
                let polled = Arc::clone(&self.polled);
                Box::pin(async move {
                    // A watcher task, so cancellation is observed even when the runtime drops the
                    // resolution future itself.
                    polled.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        cancellation.cancelled().await;
                        seen.fetch_add(1, Ordering::SeqCst);
                    });
                    std::future::pending::<Result<Credential, CredentialError>>().await
                })
            }
        }
    }
}

/// What the scripted context provider does for one request.
pub enum ProvideScript {
    /// Offer these items.
    Items(Vec<ContextItem>),
    /// Return the constant failure.
    Failure,
    /// Stall until the request is cancelled.
    Stall,
    /// Panic while constructing the context future.
    Panic,
    /// Panic when the context future is polled.
    PolledPanic,
}

/// A context provider that answers from a script, in order.
pub struct FakeContextProvider {
    scripts: Mutex<VecDeque<ProvideScript>>,
    requests: Mutex<Vec<ContextRequestRecord>>,
}

/// A recorded context request, without the borrowed references.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextRequestRecord {
    /// Model id of the request.
    pub model: String,
    /// Latest original user query of the request.
    pub query: Option<String>,
    /// Whether the canonical transcript contained an injected marker.
    pub transcript_contains_marker: bool,
}

impl FakeContextProvider {
    /// A provider with the given script, consumed one entry per request.
    pub fn new(scripts: Vec<ProvideScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Requests this provider received, in order.
    pub fn requests(&self) -> Vec<ContextRequestRecord> {
        self.requests.lock().expect("lock").clone()
    }

    /// How many requests this provider received.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("lock").len()
    }
}

/// The marker used to prove injected context never reaches the canonical transcript.
pub const MARKER: &str = "INJECTED-CONTEXT-MARKER";

/// Characters a snippet adds to the derived system prompt, label included.
///
/// Mirrors the documented rendering the runtime budgets: the provenance label, a newline and the
/// item text. The separator between snippets is not included.
pub fn rendered_chars(item: &ContextItem) -> usize {
    format!(
        "[context source={} id={}]\n{}",
        item.source, item.id, item.text
    )
    .chars()
    .count()
}

impl ContextProvider for FakeContextProvider {
    fn provide<'a>(
        &'a self,
        request: &'a ContextRequest<'a>,
        cancellation: CancellationToken,
    ) -> ContextProviderFuture<'a> {
        self.requests
            .lock()
            .expect("lock")
            .push(ContextRequestRecord {
                model: request.model.id.clone(),
                query: request.query.clone(),
                transcript_contains_marker: format!("{:?}", request.context.messages)
                    .contains(MARKER),
            });
        let script = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| ProvideScript::Items(Vec::new()));
        match script {
            ProvideScript::Items(items) => Box::pin(async move { Ok(items) }),
            ProvideScript::Failure => Box::pin(async move { Err(ContextError) }),
            ProvideScript::Panic => panic!("CONTEXT-PANIC-PRIVATE"),
            ProvideScript::PolledPanic => {
                Box::pin(async { panic!("CONTEXT-POLLED-PANIC-PRIVATE") })
            }
            ProvideScript::Stall => Box::pin(async move {
                cancellation.cancelled().await;
                Err(ContextError)
            }),
        }
    }
}

/// Build a registry from bindings.
pub fn registry(bindings: Vec<ModelBinding>) -> Arc<ModelRegistry> {
    Arc::new(ModelRegistry::new(bindings).expect("valid registry"))
}

/// A binding over a provider with the test model and the given credential identifier.
pub fn binding(key: &str, provider: Arc<dyn Provider>, credential_id: &str) -> ModelBinding {
    ModelBinding::new(
        key,
        test_model(),
        provider,
        CredentialId::new(credential_id),
    )
}

/// A session over one provider and one resolver.
pub fn session(
    registry: Arc<ModelRegistry>,
    binding_key: &str,
    credentials: Arc<dyn CredentialResolver>,
) -> Session {
    Session::new(mira_runtime::SessionConfig::new(
        registry,
        binding_key,
        credentials,
    ))
    .expect("valid session")
}

/// Drain a run's events and return its terminal outcome.
pub async fn finish(
    mut run: mira_runtime::SessionRun,
) -> (
    Vec<Arc<str>>,
    Result<mira_runtime::RunOutcome, mira_runtime::RuntimeError>,
) {
    let mut texts = Vec::new();
    while let Some(event) = run.recv().await {
        if let mira_runtime::AgentEvent::MessageUpdate {
            update: AssistantEvent::BlockDelta { delta, .. },
            ..
        } = &event
        {
            texts.push(Arc::clone(delta));
        }
    }
    (texts, run.outcome().await)
}

/// Wait, bounded, until a session is not busy.
pub async fn wait_idle(session: &Session) {
    for _ in 0..2000 {
        if !session.is_busy() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("session stayed busy");
}

/// Every tool call of a transcript with exactly one ordered result.
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

/// A tool that always succeeds and records its calls.
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
    /// A calculator with `a` and `b`.
    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "calculator",
                "Add two numbers.",
                json!({
                    "type": "object",
                    "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
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
            self.calls.lock().expect("lock").push(arguments.clone());
            let a = arguments
                .get("a")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let b = arguments
                .get("b")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            ToolResult::text(format!("{}", a + b))
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
    /// A stalling tool plus the signal that completes when cancellation was observed.
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
        let signal = self.signal.lock().expect("lock").take();
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

/// A tool registry holding the given tools.
pub fn tools(implementations: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
    implementations
}

/// Assert that a registry can be built, as a sanity check for tests.
pub fn tool_registry(implementations: Vec<Arc<dyn Tool>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in implementations {
        registry.insert(tool).expect("tool registers");
    }
    registry
}
