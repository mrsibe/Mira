//! Errors shared by the protocol and the transports.

use std::fmt;
use std::time::Duration;

/// Largest provider error body the transport reads before classifying it.
///
/// The body is only inspected for an allowlisted machine-readable code; none of its text is
/// retained.
#[cfg(feature = "openai-completions")]
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// The request stage that exceeded its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeoutStage {
    /// Waiting for response headers, which includes connecting and TLS setup.
    ResponseHeaders,
    /// Waiting for the next byte of the response body.
    StreamInactivity,
    /// The request exceeded its total deadline.
    Total,
}

impl fmt::Display for TimeoutStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResponseHeaders => "response headers",
            Self::StreamInactivity => "response stream inactivity",
            Self::Total => "the total request deadline",
        })
    }
}

/// A provider failure category the transport recognizes.
///
/// Categories come from the HTTP status and from a small allowlist of machine-readable provider
/// codes. Arbitrary provider prose is deliberately never stored in an error: a provider body
/// can echo the request prompt, the credential or unrelated HTML, so it is reduced to this
/// category and then discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderFailure {
    /// The provider rejected the request as malformed or unsupported.
    InvalidRequest,
    /// The credential is missing, invalid or not permitted.
    Unauthorized,
    /// The provider does not serve the requested model or endpoint.
    NotFound,
    /// The provider is rate limiting or out of quota.
    RateLimited,
    /// The request exceeds the model context window.
    ContextOverflow,
    /// The provider failed internally or is temporarily unavailable.
    Unavailable,
    /// The provider redirected the request; redirects are refused.
    Redirect,
    /// The provider reported an error inside the response stream.
    StreamError,
    /// The provider failed without an allowlisted category.
    Unrecognized,
}

impl ProviderFailure {
    /// Stable machine-readable identifier, suitable for matching and logs that carry no prose.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not-found",
            Self::RateLimited => "rate-limited",
            Self::ContextOverflow => "context-overflow",
            Self::Unavailable => "unavailable",
            Self::Redirect => "redirect",
            Self::StreamError => "stream-error",
            Self::Unrecognized => "unrecognized",
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "the provider rejected the request",
            Self::Unauthorized => "the provider rejected the credential",
            Self::NotFound => "the provider does not serve this model",
            Self::RateLimited => "the provider is rate limiting this request",
            Self::ContextOverflow => "the request exceeds the model context window",
            Self::Unavailable => "the provider is unavailable",
            Self::Redirect => "the provider redirected the request",
            Self::StreamError => "the provider reported an error while streaming",
            Self::Unrecognized => "the provider reported an unrecognized failure",
        })
    }
}

/// Everything that can go wrong between a caller's request and a terminal assistant message.
///
/// Errors never contain provider prose, the request URL, the request body, prompt text or a
/// credential. Provider responses are reduced to constants and to [`ProviderFailure`]
/// categories.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AiError {
    /// The request could not be built from the supplied model, context or options.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The configured API base URL cannot address a Chat Completions endpoint.
    #[error("invalid API base URL: {0}")]
    InvalidBaseUrl(String),
    /// The provider answered with a non-success HTTP status.
    #[error("provider returned HTTP {status} ({failure})")]
    Provider {
        /// HTTP status returned by the provider.
        status: u16,
        /// Allowlisted category, never provider prose.
        failure: ProviderFailure,
    },
    /// The provider ended the response stream with an error payload.
    #[error("provider reported an error in the response stream ({failure})")]
    ProviderStream {
        /// Allowlisted category, never provider prose.
        failure: ProviderFailure,
    },
    /// The provider requested a retry cooldown this request may not wait for.
    ///
    /// The transport never shortens a provider-requested cooldown and never waits longer than
    /// the request allows. `requested` is [`Duration::MAX`] when the provider asked for a delay
    /// too large to represent at all.
    #[error(
        "provider requested a retry delay of {requested:?}, which exceeds the allowed {allowed:?}"
    )]
    RetryDelayExceeded {
        /// Cooldown the provider asked for.
        requested: Duration,
        /// Longest delay this request accepts: the configured retry budget or the remaining
        /// total deadline, whichever is smaller.
        allowed: Duration,
    },
    /// The connection or the response body failed below the protocol layer.
    #[error("transport failure: {0}")]
    Transport(String),
    /// A request stage exceeded its deadline.
    #[error("request timed out waiting for {0}")]
    Timeout(TimeoutStage),
    /// The caller cancelled the request.
    #[error("request cancelled")]
    Cancelled,
    /// The response stream ended without a provider finish reason.
    #[error("provider stream ended without a finish reason")]
    IncompleteStream,
    /// The provider response violated the streaming protocol.
    #[error("provider stream protocol error: {0}")]
    Protocol(String),
    /// A requested option is not supported by the selected provider configuration.
    #[error("unsupported option: {0}")]
    Unsupported(String),
    /// `Provider::stream` was called outside a Tokio runtime.
    #[error("no Tokio runtime is available to run the request")]
    NoRuntime,
}

/// Map an HTTP status and an optional provider code to a failure category.
#[cfg(feature = "openai-completions")]
pub(crate) fn provider_failure(status: Option<u16>, code: Option<&str>) -> ProviderFailure {
    if let Some(failure) = code.and_then(allowlisted_code) {
        return failure;
    }
    match status {
        Some(400 | 422) => ProviderFailure::InvalidRequest,
        Some(401 | 403) => ProviderFailure::Unauthorized,
        Some(404 | 405) => ProviderFailure::NotFound,
        Some(429) => ProviderFailure::RateLimited,
        Some(300..=399) => ProviderFailure::Redirect,
        Some(500..=599) => ProviderFailure::Unavailable,
        Some(_) => ProviderFailure::Unrecognized,
        None => ProviderFailure::StreamError,
    }
}

/// Map a provider error body to a category, retaining none of its text.
#[cfg(feature = "openai-completions")]
pub(crate) fn provider_failure_from_body(body: &str) -> Option<ProviderFailure> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    code_from_value(error).and_then(allowlisted_code)
}

/// Map a provider error payload inside the response stream to a category.
#[cfg(feature = "openai-completions")]
pub(crate) fn provider_failure_from_value(value: &serde_json::Value) -> ProviderFailure {
    provider_failure(None, code_from_value(value))
}

/// Read the machine-readable `code`/`type` field of a provider error object.
#[cfg(feature = "openai-completions")]
fn code_from_value(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    object
        .get("code")
        .and_then(serde_json::Value::as_str)
        .or_else(|| object.get("type").and_then(serde_json::Value::as_str))
}

/// The allowlist of provider codes the transport understands. Unknown codes are ignored and the
/// HTTP status decides the category instead.
#[cfg(feature = "openai-completions")]
fn allowlisted_code(code: &str) -> Option<ProviderFailure> {
    let normalized = code.trim().to_ascii_lowercase();
    Some(match normalized.as_str() {
        "invalid_request_error" | "invalid_request" => ProviderFailure::InvalidRequest,
        "authentication_error"
        | "invalid_api_key"
        | "permission_denied"
        | "insufficient_permissions" => ProviderFailure::Unauthorized,
        "rate_limit_error" | "rate_limit_exceeded" => ProviderFailure::RateLimited,
        "context_length_exceeded" | "context_overflow" | "string_above_max_length" => {
            ProviderFailure::ContextOverflow
        }
        "server_error" | "overloaded_error" | "service_unavailable" => ProviderFailure::Unavailable,
        _ => return None,
    })
}

#[cfg(all(test, feature = "openai-completions"))]
mod tests {
    use super::*;

    fn from_body(body: &str) -> Option<ProviderFailure> {
        provider_failure_from_body(body)
    }

    #[test]
    fn classifies_allowlisted_provider_codes() {
        let cases = [
            (
                r#"{"error":{"message":"x","code":"context_length_exceeded"}}"#,
                ProviderFailure::ContextOverflow,
            ),
            (
                r#"{"error":{"type":"rate_limit_error","message":"x"}}"#,
                ProviderFailure::RateLimited,
            ),
            (
                r#"{"error":{"type":"invalid_api_key"}}"#,
                ProviderFailure::Unauthorized,
            ),
            (
                r#"{"code":"overloaded_error"}"#,
                ProviderFailure::Unavailable,
            ),
            (
                r#"{"error":{"type":"CONTEXT_OVERFLOW"}}"#,
                ProviderFailure::ContextOverflow,
            ),
        ];
        for (body, expected) in cases {
            assert_eq!(from_body(body), Some(expected), "body: {body}");
        }
    }

    #[test]
    fn falls_back_to_the_http_status() {
        assert_eq!(
            provider_failure(Some(400), None),
            ProviderFailure::InvalidRequest
        );
        assert_eq!(
            provider_failure(Some(401), None),
            ProviderFailure::Unauthorized
        );
        assert_eq!(
            provider_failure(Some(403), None),
            ProviderFailure::Unauthorized
        );
        assert_eq!(provider_failure(Some(404), None), ProviderFailure::NotFound);
        assert_eq!(
            provider_failure(Some(429), None),
            ProviderFailure::RateLimited
        );
        assert_eq!(provider_failure(Some(302), None), ProviderFailure::Redirect);
        assert_eq!(
            provider_failure(Some(500), None),
            ProviderFailure::Unavailable
        );
        assert_eq!(
            provider_failure(Some(503), None),
            ProviderFailure::Unavailable
        );
        assert_eq!(
            provider_failure(Some(418), None),
            ProviderFailure::Unrecognized
        );
        assert_eq!(provider_failure(None, None), ProviderFailure::StreamError);
    }

    #[test]
    fn an_unknown_code_leaves_the_status_in_charge() {
        assert_eq!(
            provider_failure(Some(429), Some("weird_code")),
            ProviderFailure::RateLimited
        );
        assert_eq!(from_body(r#"{"error":{"code":"weird_code"}}"#), None);
    }

    #[test]
    fn ignores_bodies_without_a_structured_code() {
        assert_eq!(from_body("<html>502 Bad Gateway</html>"), None);
        assert_eq!(from_body("not json"), None);
        assert_eq!(from_body("{}"), None);
        assert_eq!(from_body(r#"{"error":{"message":"why"}}"#), None);
    }

    #[test]
    fn provider_prose_never_reaches_an_error() {
        const PROMPT: &str = "PRIVATE-PROMPT-TEXT";
        const KEY: &str = "sk-secret-value-1234567890";
        const CUSTOM: &str = "CUSTOM-HEADER-SECRET";

        let body = format!(
            r#"{{"error":{{"message":"rejected prompt: {PROMPT}; bearer {KEY}; x-api-key: {CUSTOM}","code":"context_length_exceeded"}}}}"#
        );
        assert_eq!(from_body(&body), Some(ProviderFailure::ContextOverflow));

        let stream_error: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"message":"{PROMPT} {KEY} {CUSTOM}","type":"server_error"}}"#
        ))
        .expect("json");
        let error = AiError::ProviderStream {
            failure: provider_failure_from_value(&stream_error),
        };
        assert_eq!(
            error,
            AiError::ProviderStream {
                failure: ProviderFailure::Unavailable
            }
        );

        let rendered = format!(
            "{error} {} {}",
            AiError::Provider {
                status: 400,
                failure: ProviderFailure::InvalidRequest,
            },
            AiError::RetryDelayExceeded {
                requested: Duration::from_secs(3600),
                allowed: Duration::from_secs(5),
            }
        );
        for needle in [PROMPT, KEY, CUSTOM, "rejected prompt"] {
            assert!(
                !rendered.contains(needle),
                "'{needle}' leaked into: {rendered}"
            );
        }
        assert!(rendered.contains("context-overflow") || rendered.contains("unavailable"));
    }

    #[test]
    fn failure_categories_have_stable_identifiers() {
        assert_eq!(ProviderFailure::RateLimited.as_str(), "rate-limited");
        assert_eq!(
            ProviderFailure::Redirect.to_string(),
            "the provider redirected the request"
        );
    }
}
