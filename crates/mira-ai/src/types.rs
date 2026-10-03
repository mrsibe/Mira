//! Portable inference protocol types shared by every Mira provider adapter.
//!
//! These types describe one model request and its streamed response. They carry no HTTP,
//! storage or application concerns, so `mira-agent` and `mira-runtime` can depend on them
//! directly. See [ADR 0006](../../../docs/adr/0006-rust-native-runtime.md).

use std::fmt;

use serde_json::Value;

/// Transport family that serves a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Api {
    /// OpenAI-compatible Chat Completions, served at `POST {base_url}/chat/completions`.
    OpenAiCompletions,
}

impl Api {
    /// Stable identifier used by provider configuration and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
        }
    }
}

impl fmt::Display for Api {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A model as addressed by a provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// Identifier sent to the provider in the `model` request field.
    pub id: String,
    /// Transport that serves this model.
    pub api: Api,
    /// Registry-level provider identifier used for attribution, for example `"deepseek"`.
    pub provider: String,
    /// What the model accepts and can produce.
    pub capabilities: ModelCapabilities,
}

impl Model {
    /// A text-only model; set [`Model::capabilities`] for images, reasoning or tools.
    pub fn new(api: Api, provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            api,
            provider: provider.into(),
            capabilities: ModelCapabilities::default(),
        }
    }

    /// The same model with explicit capabilities.
    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// Declared model abilities. The application decides these; the protocol never guesses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCapabilities {
    /// The model accepts image input parts.
    pub images: bool,
    /// The model can produce thinking/reasoning output.
    pub reasoning: bool,
    /// The model can be asked to call tools. Informational for a transport: the caller
    /// declares tools through [`Context::tools`](crate::Context::tools).
    pub tools: bool,
}

/// Text content block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    /// Text content.
    pub text: String,
}

impl TextBlock {
    /// A text block.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Base64 image input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInput {
    /// Media type of the payload, for example `image/png`.
    pub media_type: String,
    /// Base64 payload without a `data:` URL prefix.
    pub base64_data: String,
}

impl ImageInput {
    /// An image part with its media type.
    pub fn new(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self {
        Self {
            media_type: media_type.into(),
            base64_data: base64_data.into(),
        }
    }
}

/// Thinking/reasoning content block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingBlock {
    /// Reasoning text. Empty when the provider redacted it.
    pub thinking: String,
    /// Opaque provider signature for replay, for example the response field the reasoning
    /// arrived in. Consumers store and re-send it but must not interpret it.
    pub signature: Option<String>,
    /// True when provider safety filters replaced the reasoning text with opaque content.
    pub redacted: bool,
}

/// Content accepted in user messages and tool results.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputContent {
    /// Text part.
    Text(TextBlock),
    /// Image part.
    Image(ImageInput),
}

impl InputContent {
    /// A text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextBlock::new(text))
    }

    /// An image part.
    pub fn image(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self {
        Self::Image(ImageInput::new(media_type, base64_data))
    }
}

/// A tool call the model asked the consumer to execute.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider tool call identifier, used to pair the call with its result.
    pub id: String,
    /// Tool name as declared in [`Context::tools`](crate::Context::tools).
    pub name: String,
    /// Argument JSON exactly as the provider streamed it. Never repaired or salvaged.
    pub arguments_raw: String,
    /// Strictly parsed arguments. `None` when `arguments_raw` is not complete, valid JSON
    /// object text, for example after the provider stopped at its output token limit.
    /// A call with `None` arguments must never be executed.
    pub arguments: Option<Value>,
    /// Opaque provider signature attached to the call, for example a thought signature.
    /// The OpenAI-compatible transport does not produce or consume one.
    pub signature: Option<String>,
}

/// Content a model can produce.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AssistantContent {
    /// Visible text.
    Text(TextBlock),
    /// Reasoning text.
    Thinking(ThinkingBlock),
    /// Tool call requested by the model.
    ToolCall(ToolCall),
}

/// User turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    /// Text and image parts, in order. An empty list is valid input and is skipped when encoding.
    pub content: Vec<InputContent>,
}

impl UserMessage {
    /// A user turn with a single text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![InputContent::text(text)],
        }
    }
}

/// Tool result turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultMessage {
    /// Identifier of the [`ToolCall`] this result answers.
    pub tool_call_id: String,
    /// Name of the tool that produced the result.
    pub tool_name: String,
    /// Text and image parts of the result.
    pub content: Vec<InputContent>,
    /// True when the tool failed.
    pub is_error: bool,
}

/// Assistant turn.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantMessage {
    /// Who produced the message.
    pub source: AssistantSource,
    /// Produced content blocks, in order.
    pub content: Vec<AssistantContent>,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// Provider finish reason as data, for example `content_filter`, bounded to printable
    /// characters and truncated. It is never used as a human-readable description.
    /// This is untrusted and not redacted: like message content, it may contain sensitive
    /// data and must not be logged or treated as a safe diagnostic.
    pub raw_stop_reason: Option<String>,
    /// Reported token usage, or `None` when the provider did not report any.
    pub usage: Option<Usage>,
    /// Constant explanation for [`StopReason::Failed`]. Provider prose is never copied here.
    pub error_message: Option<String>,
}

impl AssistantMessage {
    /// Concatenated text blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Tool calls in the order the model produced them.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|block| match block {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
    }
}

/// Which provider, transport and model produced an assistant message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantSource {
    /// Transport that produced the message.
    pub api: Api,
    /// Registry-level provider identifier from the requested [`Model`].
    pub provider: String,
    /// Model identifier the caller requested.
    pub model: String,
    /// Concrete model the provider reported when it differs from the requested one.
    pub response_model: Option<String>,
    /// Provider response identifier when the stream reported one.
    pub response_id: Option<String>,
}

/// Why the model stopped producing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished its turn (`stop`).
    EndTurn,
    /// The model reached the output token limit (`length`). Tool arguments in this response
    /// may be truncated and must not be executed.
    MaxTokens,
    /// The model finished the turn to request tool calls (`tool_calls`).
    ToolUse,
    /// The provider ended the turn without usable output, for example a safety filter or an
    /// unsupported finish reason. `error_message` explains and `raw_stop_reason` preserves
    /// the provider value.
    Failed,
}

/// Token usage exactly as the provider reported it.
///
/// Usage is present only when the provider reported it; the transport never fabricates
/// zero counts. Cache and reasoning counts are optional because providers report them
/// inconsistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// `prompt_tokens` as reported.
    pub input_tokens: u64,
    /// `completion_tokens` as reported. Includes [`Usage::reasoning_tokens`].
    pub output_tokens: u64,
    /// Reported cache-read input tokens, a subset of [`Usage::input_tokens`].
    pub cached_input_tokens: Option<u64>,
    /// Reported reasoning tokens, a subset of [`Usage::output_tokens`].
    pub reasoning_tokens: Option<u64>,
}

/// One turn in a conversation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Message {
    /// User turn.
    User(UserMessage),
    /// Assistant turn, as produced by a provider or restored by the caller.
    Assistant(AssistantMessage),
    /// Result of a tool call.
    ToolResult(ToolResultMessage),
}

impl From<UserMessage> for Message {
    fn from(message: UserMessage) -> Self {
        Self::User(message)
    }
}

impl From<AssistantMessage> for Message {
    fn from(message: AssistantMessage) -> Self {
        Self::Assistant(message)
    }
}

impl From<ToolResultMessage> for Message {
    fn from(message: ToolResultMessage) -> Self {
        Self::ToolResult(message)
    }
}
