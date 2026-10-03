//! OpenAI-compatible Chat Completions provider.
//!
//! The provider sends `POST {base_url}/chat/completions` with `stream: true` and decodes the
//! server-sent event body into the portable protocol. Compatibility is explicit: nothing is
//! inferred from the provider name or URL, so a custom endpoint never receives guessed
//! parameters.
//!
//! Two policies are deliberately strict:
//!
//! - Redirects are refused, so a provider cannot forward the conversation or the caller's
//!   custom credential headers to another origin.
//! - Provider responses are reduced to [`ProviderFailure`](crate::ProviderFailure) categories and
//!   constants; arbitrary provider prose never reaches an error.
//!
//! ```no_run
//! use mira_ai::api::openai_completions::{OpenAiCompletionsConfig, OpenAiCompletionsProvider};
//! use mira_ai::{Context, Credential, Model, Provider, StreamRequest};
//!
//! # async fn run() -> Result<(), mira_ai::AiError> {
//! let provider = OpenAiCompletionsProvider::new(OpenAiCompletionsConfig::new(
//!     "https://api.deepseek.com/v1",
//! )?)?;
//! let request = StreamRequest::new(
//!     Model::new(mira_ai::Api::OpenAiCompletions, "deepseek", "deepseek-chat"),
//!     Context::user_text("Hello"),
//!     Credential::new("sk-..."),
//! );
//! let mut stream = provider.stream(request)?;
//! while let Some(event) = stream.recv().await {
//!     let _ = event;
//! }
//! let message = stream.result().await?;
//! # let _ = message;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::error::AiError;
use crate::provider::{Provider, StreamRequest};
use crate::stream::StreamHandle;

mod decode;
mod encode;
mod sse;
mod transport;
mod url;
mod wire;

/// Default timeout for opening a connection to the provider.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Default maximum size of one SSE event payload.
pub const DEFAULT_MAX_SSE_EVENT_BYTES: usize = sse::DEFAULT_MAX_SSE_EVENT_BYTES;
/// Default maximum number of content blocks accepted in one response.
pub const DEFAULT_MAX_CONTENT_BLOCKS: usize = 512;

/// How a requested reasoning level is encoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEncoding {
    /// Send the top-level `reasoning_effort` field.
    ReasoningEffort,
    /// Send DeepSeek's `thinking: { "type": "enabled" }`, optionally with `reasoning_effort`.
    DeepSeekThinking,
}

/// Request field used for an explicit output token cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensField {
    /// Current OpenAI field.
    MaxCompletionTokens,
    /// Deprecated OpenAI field that several compatible providers require.
    MaxTokens,
}

/// Explicit compatibility switches for one OpenAI-compatible endpoint.
///
/// Every field is a caller decision. The transport never derives compatibility from a
/// provider name or base URL, and it never sends a parameter a caller did not ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenAiCompletionsCompat {
    /// How a requested reasoning level is encoded. `None` means the endpoint is never sent a
    /// reasoning parameter, and requesting one becomes an error.
    pub reasoning: Option<ReasoningEncoding>,
    /// Whether `reasoning_effort` is accepted alongside
    /// [`ReasoningEncoding::DeepSeekThinking`].
    pub supports_reasoning_effort: bool,
    /// Whether the endpoint accepts `stream_options: { "include_usage": true }`.
    pub supports_usage_in_streaming: bool,
    /// Whether assistant reasoning is replayed as `reasoning_content` when the message came
    /// from the same model.
    pub replay_reasoning_content: bool,
    /// Request field used when a caller sets an explicit output token cap.
    pub max_tokens_field: MaxTokensField,
}

impl Default for OpenAiCompletionsCompat {
    fn default() -> Self {
        Self {
            reasoning: None,
            supports_reasoning_effort: false,
            supports_usage_in_streaming: true,
            replay_reasoning_content: false,
            max_tokens_field: MaxTokensField::MaxCompletionTokens,
        }
    }
}

impl OpenAiCompletionsCompat {
    /// DeepSeek profile: `thinking` plus `reasoning_effort` and the `max_tokens` field.
    ///
    /// This mirrors the DeepSeek request shape Mira already sends today. Reasoning is not
    /// replayed, matching DeepSeek's guidance not to send previous `reasoning_content` back.
    pub fn deepseek() -> Self {
        Self {
            reasoning: Some(ReasoningEncoding::DeepSeekThinking),
            supports_reasoning_effort: true,
            supports_usage_in_streaming: true,
            replay_reasoning_content: false,
            max_tokens_field: MaxTokensField::MaxTokens,
        }
    }
}

/// Configuration of one OpenAI-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiCompletionsConfig {
    /// API base URL, for example `https://api.deepseek.com/v1`. The transport appends
    /// `/chat/completions`.
    pub base_url: String,
    /// Explicit compatibility switches.
    pub compat: OpenAiCompletionsCompat,
    /// Extra headers sent with every request. `authorization` and hop-by-hop headers are
    /// rejected because credentials are supplied per request.
    pub extra_headers: Vec<(String, String)>,
    /// Timeout for opening a connection. Shared by every request through the HTTP client.
    pub connect_timeout: Duration,
    /// Largest SSE event payload accepted from the provider.
    pub max_sse_event_bytes: usize,
    /// Largest number of content blocks accepted in one response.
    pub max_content_blocks: usize,
}

impl OpenAiCompletionsConfig {
    /// Configuration with validated base URL and baseline limits.
    pub fn new(base_url: impl Into<String>) -> Result<Self, AiError> {
        let base_url = base_url.into();
        url::validate_base_url(&base_url)?;
        Ok(Self {
            base_url,
            compat: OpenAiCompletionsCompat::default(),
            extra_headers: Vec::new(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_sse_event_bytes: DEFAULT_MAX_SSE_EVENT_BYTES,
            max_content_blocks: DEFAULT_MAX_CONTENT_BLOCKS,
        })
    }
}

impl fmt::Debug for OpenAiCompletionsConfig {
    /// Header values are never printed: a caller may use a custom header to carry a key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_names: Vec<&str> = self
            .extra_headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        formatter
            .debug_struct("OpenAiCompletionsConfig")
            .field("base_url", &self.base_url)
            .field("compat", &self.compat)
            .field("extra_headers", &header_names)
            .field("connect_timeout", &self.connect_timeout)
            .field("max_sse_event_bytes", &self.max_sse_event_bytes)
            .field("max_content_blocks", &self.max_content_blocks)
            .finish()
    }
}

/// Streamed Chat Completions provider.
///
/// A provider owns the shared HTTP connection pool; authorization, model, context and options
/// are supplied per request. Clone it to share one pool between callers.
#[derive(Clone)]
pub struct OpenAiCompletionsProvider {
    transport: Arc<transport::Transport>,
}

impl OpenAiCompletionsProvider {
    /// Build a provider and its HTTP client from a validated configuration.
    pub fn new(config: OpenAiCompletionsConfig) -> Result<Self, AiError> {
        Ok(Self {
            transport: Arc::new(transport::Transport::new(config)?),
        })
    }
}

impl fmt::Debug for OpenAiCompletionsProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompletionsProvider")
            .field("base_url", &self.transport.config.base_url)
            .finish_non_exhaustive()
    }
}

impl Provider for OpenAiCompletionsProvider {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
        transport::spawn(self.transport.clone(), request)
    }
}
