//! The injectable provider seam and the per-request options it consumes.

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use crate::context::Context;
use crate::error::AiError;
use crate::stream::StreamHandle;
use crate::types::Model;

/// Tokio deadline for a provider to answer with response headers. Includes connecting.
pub const DEFAULT_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(45);
/// Tokio deadline between two bytes of the response body.
pub const DEFAULT_STREAM_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(180);
/// Tokio deadline for one request from send to terminal result.
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Retries allowed for retryable setup failures. Two retries means at most three attempts.
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// Longest delay the transport waits between attempts, including a provider `Retry-After`.
pub const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// A bearer credential supplied explicitly for exactly one request.
///
/// Credentials are never read from the environment, a global registry or a database. The type
/// itself implements neither `Serialize` nor `Deserialize` and has a redacting `Debug`, so a
/// `Credential` value cannot be stored or logged directly. This is a type-level exclusion only:
/// [`Credential::expose_secret`] still returns the secret, so a caller must not copy it into a
/// message text, an error or a log line.
#[derive(Clone)]
pub struct Credential {
    secret: String,
}

impl Credential {
    /// Wrap a bearer credential for exactly one request.
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    /// The credential value. Call this only where the value is needed on the wire.
    pub fn expose_secret(&self) -> &str {
        &self.secret
    }

    /// Whether the credential is empty.
    pub fn is_empty(&self) -> bool {
        self.secret.is_empty()
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential([redacted])")
    }
}

/// Provider-neutral reasoning effort requested for one turn.
///
/// `Serialize`/`Deserialize` write the same `snake_case` value as [`ReasoningLevel::as_str`],
/// so a stored thinking level survives a reload unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningLevel {
    /// Smallest useful reasoning effort.
    Minimal,
    /// Low reasoning effort.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort.
    High,
}

impl ReasoningLevel {
    /// Wire value used when a configuration encodes effort as a string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Per-request settings. Timeouts default to documented baselines; tests and callers may
/// lower them.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestOptions {
    /// Sampling temperature. `None` sends no temperature field.
    pub temperature: Option<f32>,
    /// Explicit output token cap. `None` sends no cap: the transport never guesses a
    /// provider or model limit.
    pub max_output_tokens: Option<u32>,
    /// Requested reasoning effort. `None` sends no reasoning parameter at all.
    pub reasoning: Option<ReasoningLevel>,
    /// Deadline for response headers once the request is sent.
    pub response_header_timeout: Duration,
    /// Deadline between two bytes of the response body.
    pub stream_inactivity_timeout: Duration,
    /// Deadline for the whole request, including reading the body.
    pub total_timeout: Duration,
    /// Retries allowed for retryable setup failures, before any content is published.
    pub max_retries: u32,
    /// Upper bound for a single retry delay, including a provider `Retry-After`.
    pub max_retry_delay: Duration,
    /// Buffered events before the producer waits for the consumer.
    pub event_buffer: NonZeroUsize,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            temperature: None,
            max_output_tokens: None,
            reasoning: None,
            response_header_timeout: DEFAULT_RESPONSE_HEADER_TIMEOUT,
            stream_inactivity_timeout: DEFAULT_STREAM_INACTIVITY_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            max_retry_delay: DEFAULT_MAX_RETRY_DELAY,
            event_buffer: crate::stream::DEFAULT_EVENT_BUFFER,
        }
    }
}

impl RequestOptions {
    /// Documented baseline settings.
    pub fn new() -> Self {
        Self::default()
    }
}

/// One streamed completion request.
#[derive(Debug, Clone)]
pub struct StreamRequest {
    /// Model to address, with declared capabilities.
    pub model: Model,
    /// System prompt, conversation history and tool declarations.
    pub context: Context,
    /// Credential sent as `Authorization: Bearer`. Never logged or serialized.
    pub credential: Credential,
    /// Timeouts, sampling and transport options for this request.
    pub options: RequestOptions,
}

impl StreamRequest {
    /// A request with baseline options.
    pub fn new(model: Model, context: Context, credential: Credential) -> Self {
        Self {
            model,
            context,
            credential,
            options: RequestOptions::default(),
        }
    }
}

/// A source of streamed model responses.
///
/// Implementations are shared across threads (`Arc<dyn Provider>` works) and receive the
/// credential, model, context and options of each call explicitly.
pub trait Provider: Send + Sync {
    /// Start one streamed completion.
    ///
    /// The transport performs the request on the current Tokio runtime and returns a handle
    /// immediately; dropping the handle cancels the request. Request-building failures are
    /// returned here, while runtime failures arrive through the terminal result.
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_redacted_in_debug_output() {
        let credential = Credential::new("sk-do-not-print-me");
        assert_eq!(format!("{credential:?}"), "Credential([redacted])");
        let request = StreamRequest::new(
            Model::new(crate::types::Api::OpenAiCompletions, "test", "model"),
            Context::new(),
            credential,
        );
        assert!(!format!("{request:?}").contains("sk-do-not-print-me"));
    }
}
