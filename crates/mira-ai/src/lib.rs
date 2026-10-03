//! Portable inference protocol and an OpenAI-compatible Chat Completions transport for Mira.
//!
//! `mira-ai` is the bottom of the Mira library stack (`mira-runtime` → `mira-agent` →
//! `mira-ai`). It knows nothing about Tauri, SQLite, the keyring, the desktop application or
//! its prompts: every credential, model, context and option is an explicit caller input.
//!
//! ```no_run
//! use mira_ai::stream::DEFAULT_EVENT_BUFFER;
//! use mira_ai::{
//!     AiError, Api, AssistantEmitter, AssistantEvent, AssistantMessage, AssistantSource, BlockKind,
//!     Context, Credential, Model, StreamRequest,
//! };
//!
//! # async fn run() -> Result<(), AiError> {
//! let request = StreamRequest::new(
//!     Model::new(Api::OpenAiCompletions, "example-provider", "example-model"),
//!     Context::user_text("Hello"),
//!     Credential::new("sk-..."),
//! );
//! let (mut producer, mut stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
//! let source = AssistantSource {
//!     api: request.model.api,
//!     provider: request.model.provider.clone(),
//!     model: request.model.id.clone(),
//!     response_model: None,
//!     response_id: None,
//! };
//!
//! // A provider (real or fake) publishes immutable events for `request`.
//! producer
//!     .emit(AssistantEvent::Start {
//!         source: source.clone(),
//!     })
//!     .await
//!     .map_err(|_| AiError::Cancelled)?;
//! producer
//!     .emit(AssistantEvent::BlockStart {
//!         index: 0,
//!         kind: BlockKind::Text,
//!     })
//!     .await
//!     .map_err(|_| AiError::Cancelled)?;
//! producer
//!     .emit(AssistantEvent::BlockDelta {
//!         index: 0,
//!         delta: std::sync::Arc::from("hi"),
//!     })
//!     .await
//!     .map_err(|_| AiError::Cancelled)?;
//! // `finish` publishes the single terminal result and closes event delivery.
//! producer.finish(Ok(AssistantMessage {
//!     source,
//!     content: Vec::new(),
//!     stop_reason: mira_ai::StopReason::EndTurn,
//!     raw_stop_reason: None,
//!     usage: None,
//!     error_message: None,
//! }));
//!
//! while let Some(event) = stream.recv().await {
//!     let _ = event;
//! }
//! let message = stream.result().await?;
//! # let _ = message;
//! # Ok(())
//! # }
//! ```
//!
//! Cargo features: `openai-completions` (default) adds the HTTP transport, whose module
//! documentation shows a live request. Build with `default-features = false` to use only the
//! protocol types, the stream protocol and the [`Provider`] seam; this example compiles in that
//! configuration too.

#![deny(missing_docs)]

pub mod context;
pub mod error;
pub mod provider;
pub mod stream;
pub mod types;

#[cfg(feature = "openai-completions")]
pub mod api;

pub use context::{Context, ToolDefinition};
pub use error::{AiError, ProviderFailure, TimeoutStage};
pub use provider::{
    Credential, Provider, ReasoningLevel, RequestOptions, StreamRequest, DEFAULT_MAX_RETRIES,
    DEFAULT_MAX_RETRY_DELAY, DEFAULT_RESPONSE_HEADER_TIMEOUT, DEFAULT_STREAM_INACTIVITY_TIMEOUT,
    DEFAULT_TOTAL_TIMEOUT,
};
pub use stream::{
    AssistantEmitter, AssistantEvent, BlockKind, EmitError, StreamHandle, MAX_EVENT_BUFFER,
};
pub use types::{
    Api, AssistantContent, AssistantMessage, AssistantSource, ImageInput, InputContent, Message,
    Model, ModelCapabilities, StopReason, TextBlock, ThinkingBlock, ToolCall, ToolResultMessage,
    Usage, UserMessage,
};
