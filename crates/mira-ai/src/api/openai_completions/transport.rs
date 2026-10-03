//! HTTP transport: request setup, retries, deadlines, cancellation and body streaming.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use tokio::time::{sleep, sleep_until, timeout_at, Instant};

use crate::api::openai_completions::decode::{Decoder, PayloadOutcome};
use crate::api::openai_completions::encode;
use crate::api::openai_completions::sse::SseParser;
use crate::api::openai_completions::OpenAiCompletionsConfig;
use crate::error::{
    provider_failure, provider_failure_from_body, AiError, TimeoutStage, MAX_ERROR_BODY_BYTES,
};
use crate::provider::{RequestOptions, StreamRequest};
use crate::stream::{AssistantEmitter, AssistantEvent, EmitError, StreamHandle};
use crate::types::{AssistantMessage, AssistantSource};

/// Headers a caller may not override, because the transport sets them per request.
const RESERVED_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
];

/// Shared HTTP state of one provider.
pub(crate) struct Transport {
    pub(crate) client: reqwest::Client,
    pub(crate) endpoint: reqwest::Url,
    pub(crate) headers: HeaderMap,
    pub(crate) config: OpenAiCompletionsConfig,
}

impl Transport {
    pub(crate) fn new(config: OpenAiCompletionsConfig) -> Result<Self, AiError> {
        let endpoint = super::url::endpoint_url(&config.base_url)?;
        let headers = build_headers(&config.extra_headers)?;
        validate_deadline("connect_timeout", config.connect_timeout)?;
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            // Redirects are refused: a provider redirect could forward the conversation and the
            // caller's custom credential headers to another origin.
            .redirect(Policy::none())
            // The total deadline is enforced per request so streaming is not cut short by a
            // client-wide timeout.
            .build()
            .map_err(|_| AiError::Transport("could not build the HTTP client".to_string()))?;
        Ok(Self {
            client,
            endpoint,
            headers,
            config,
        })
    }
}

fn build_headers(extra: &[(String, String)]) -> Result<HeaderMap, AiError> {
    let mut headers = HeaderMap::new();
    for (name, value) in extra {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(AiError::InvalidRequest(
                "header names must not be empty".to_string(),
            ));
        }
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(AiError::InvalidRequest(format!(
                "header '{name}' is reserved; credentials are supplied per request"
            )));
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| AiError::InvalidRequest(format!("'{name}' is not a valid header name")))?;
        let header_value = HeaderValue::from_str(value).map_err(|_| {
            AiError::InvalidRequest(format!("header '{name}' has an invalid value"))
        })?;
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

/// Start one request and hand back the consumer half of its stream.
pub(crate) fn spawn(
    transport: Arc<Transport>,
    request: StreamRequest,
) -> Result<StreamHandle, AiError> {
    validate_timeouts(&request.options)?;
    let body = encode::build_request_body(&transport.config, &request)?;
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| AiError::NoRuntime)?;
    let (mut emitter, stream) = AssistantEmitter::try_new(request.options.event_buffer)?;
    runtime.spawn(async move {
        let outcome = run(transport, request, &emitter, body).await;
        emitter.finish(outcome);
    });
    Ok(stream)
}

/// Run one request to its terminal result under an absolute deadline.
///
/// The deadline covers the whole lifecycle: connecting, streaming, publishing events to a
/// possibly backpressured consumer, reading error bodies and waiting between attempts. When it
/// fires, the request future is dropped (releasing the HTTP work) and the timeout is delivered
/// through the terminal channel, so a consumer that never drains events still learns the
/// outcome.
async fn run(
    transport: Arc<Transport>,
    request: StreamRequest,
    emitter: &AssistantEmitter,
    body: serde_json::Value,
) -> Result<AssistantMessage, AiError> {
    let deadline = total_deadline(&request.options)?;
    let cancellation = emitter.cancellation();
    tokio::select! {
        _ = cancellation.cancelled() => Err(AiError::Cancelled),
        _ = sleep_until(deadline) => Err(AiError::Timeout(TimeoutStage::Total)),
        outcome = run_request(&transport, &request, emitter, body, deadline) => outcome,
    }
}

/// Attempt the request, retrying retryable setup failures inside the deadline.
async fn run_request(
    transport: &Transport,
    request: &StreamRequest,
    emitter: &AssistantEmitter,
    body: serde_json::Value,
    deadline: Instant,
) -> Result<AssistantMessage, AiError> {
    let options = &request.options;
    let mut retries_remaining = options.max_retries;
    let response = loop {
        match send_once(transport, request, &body, deadline).await {
            Ok(response) if response.status().is_success() => break response,
            Ok(response) => {
                let status = response.status();
                if status.is_redirection() {
                    return Err(AiError::Provider {
                        status: status.as_u16(),
                        failure: crate::ProviderFailure::Redirect,
                    });
                }
                if is_retryable_status(status) && retries_remaining > 0 {
                    let delay = next_retry_delay(
                        requested_cooldown(response.headers()),
                        options,
                        retries_remaining,
                        deadline,
                    )?;
                    retries_remaining -= 1;
                    sleep(delay).await;
                    continue;
                }
                let error_body =
                    read_error_body(response, deadline, options.stream_inactivity_timeout).await?;
                let failure = provider_failure_from_body(&error_body)
                    .unwrap_or_else(|| provider_failure(Some(status.as_u16()), None));
                return Err(AiError::Provider {
                    status: status.as_u16(),
                    failure,
                });
            }
            Err(failure) => {
                if failure.retryable && retries_remaining > 0 {
                    let delay =
                        next_retry_delay(Cooldown::Absent, options, retries_remaining, deadline)?;
                    retries_remaining -= 1;
                    sleep(delay).await;
                    continue;
                }
                return Err(failure.error);
            }
        }
    };

    stream_response(transport, request, emitter, response, deadline).await
}

/// A setup failure and whether retrying it is allowed.
struct SetupFailure {
    error: AiError,
    retryable: bool,
}

/// Send the request and wait for response headers.
async fn send_once(
    transport: &Transport,
    request: &StreamRequest,
    body: &serde_json::Value,
    deadline: Instant,
) -> Result<reqwest::Response, SetupFailure> {
    let header_deadline = stage_deadline(deadline, request.options.response_header_timeout);

    let mut builder = transport
        .client
        .post(transport.endpoint.clone())
        .headers(transport.headers.clone());
    if !request.credential.is_empty() {
        builder = builder.bearer_auth(request.credential.expose_secret());
    }
    let send = builder.json(body).send();

    match timeout_at(header_deadline, send).await {
        Err(_elapsed) => Err(SetupFailure {
            error: header_timeout_error(deadline),
            retryable: false,
        }),
        Ok(Err(error)) => Err(setup_failure(&error)),
        Ok(Ok(response)) => Ok(response),
    }
}

/// Decode the response body into protocol events and the terminal message.
async fn stream_response(
    transport: &Transport,
    request: &StreamRequest,
    emitter: &AssistantEmitter,
    response: reqwest::Response,
    deadline: Instant,
) -> Result<AssistantMessage, AiError> {
    let source = AssistantSource {
        api: request.model.api,
        provider: request.model.provider.clone(),
        model: request.model.id.clone(),
        response_model: None,
        response_id: None,
    };
    publish(
        emitter,
        vec![AssistantEvent::Start {
            source: source.clone(),
        }],
    )
    .await?;

    let mut parser = SseParser::new(transport.config.max_sse_event_bytes);
    let mut decoder = Decoder::new(source, transport.config.max_content_blocks);
    let body = response.bytes_stream();
    tokio::pin!(body);

    loop {
        let next = timeout_at(
            stage_deadline(deadline, request.options.stream_inactivity_timeout),
            body.next(),
        )
        .await;
        let (payloads, done) = match next {
            Ok(Some(Ok(bytes))) => (parser.push(&bytes)?, false),
            Ok(Some(Err(error))) => return Err(body_failure(&error)),
            Ok(None) => (parser.finish()?, true),
            Err(_elapsed) => return Err(body_timeout_error(deadline)),
        };
        let mut saw_done = false;
        for payload in payloads {
            match decoder.decode(&payload)? {
                PayloadOutcome::Done => {
                    saw_done = true;
                    break;
                }
                PayloadOutcome::Events(events) => publish(emitter, events).await?,
            }
        }
        if saw_done || done {
            break;
        }
    }

    let finalized = decoder.finish()?;
    publish(emitter, finalized.events).await?;
    Ok(finalized.message)
}

/// Publish events. The emitter reports cancellation, consumer drop and post-terminal publication
/// itself, so a fake or real producer never needs a wrapper.
async fn publish(emitter: &AssistantEmitter, events: Vec<AssistantEvent>) -> Result<(), AiError> {
    for event in events {
        emitter.emit(event).await.map_err(emit_failure)?;
    }
    Ok(())
}

fn emit_failure(error: EmitError) -> AiError {
    match error {
        EmitError::Cancelled | EmitError::ConsumerDropped => AiError::Cancelled,
        EmitError::Finished => {
            AiError::Protocol("an event was published after the terminal result".to_string())
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// A provider-requested retry cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cooldown {
    /// The provider requested no usable cooldown.
    Absent,
    /// The provider asked to wait this long; zero means the cooldown already expired.
    After(Duration),
    /// The provider asked for a delay that cannot be represented at all.
    Unrepresentable,
}

/// Read `Retry-After-Ms` first, then `Retry-After` as seconds or as an HTTP-date.
fn requested_cooldown(headers: &HeaderMap) -> Cooldown {
    if let Some(value) = header_value(headers, "retry-after-ms") {
        if let Some(cooldown) = parse_numeric_cooldown(value, 1.0) {
            return cooldown;
        }
    }
    let Some(value) = header_value(headers, "retry-after") else {
        return Cooldown::Absent;
    };
    if let Some(cooldown) = parse_numeric_cooldown(value, 1000.0) {
        return cooldown;
    }
    http_date_cooldown(value).unwrap_or(Cooldown::Absent)
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Parse a cooldown expressed in `unit_ms` milliseconds per unit.
///
/// Unparsable text is ignored (the caller falls back to its own backoff), a negative value means
/// the cooldown already expired, and a finite value that no [`Duration`] can hold is reported as
/// [`Cooldown::Unrepresentable`] instead of panicking.
fn parse_numeric_cooldown(value: &str, unit_ms: f64) -> Option<Cooldown> {
    let parsed: f64 = value.trim().parse().ok()?;
    if !parsed.is_finite() {
        // Digit/exponent overflow is an excessive cooldown, not missing advice.
        let numeric = value
            .trim()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
        return (numeric && parsed.is_sign_positive()).then_some(Cooldown::Unrepresentable);
    }
    if parsed < 0.0 {
        return Some(Cooldown::After(Duration::ZERO));
    }
    Some(
        match Duration::try_from_secs_f64(parsed * unit_ms / 1000.0) {
            Ok(delay) => Cooldown::After(delay),
            Err(_) => Cooldown::Unrepresentable,
        },
    )
}

/// Parse all three standard HTTP-date forms, including obsolete forms accepted by HTTP.
fn http_date_cooldown(value: &str) -> Option<Cooldown> {
    let deadline = httpdate::parse_http_date(value.trim()).ok()?;
    Some(Cooldown::After(
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    ))
}

/// Plan the delay before the next attempt of an in-flight retry loop.
fn next_retry_delay(
    cooldown: Cooldown,
    options: &RequestOptions,
    retries_remaining: u32,
    deadline: Instant,
) -> Result<Duration, AiError> {
    plan_retry_delay(
        cooldown,
        options.max_retries - retries_remaining,
        options,
        deadline,
    )
}

/// Plan the delay before the next attempt.
///
/// A provider-requested cooldown is never shortened: when it exceeds the allowed wait or the
/// remaining total budget, the request fails with [`AiError::RetryDelayExceeded`] instead of
/// retrying early. The transport's own backoff is skipped when it cannot fit in the remaining
/// budget.
fn plan_retry_delay(
    cooldown: Cooldown,
    attempt: u32,
    options: &RequestOptions,
    deadline: Instant,
) -> Result<Duration, AiError> {
    let remaining = remaining_budget(deadline);
    let allowed = options.max_retry_delay.min(remaining);
    match cooldown {
        Cooldown::After(requested) if requested > allowed => {
            Err(AiError::RetryDelayExceeded { requested, allowed })
        }
        Cooldown::After(requested) => Ok(requested),
        Cooldown::Unrepresentable => Err(AiError::RetryDelayExceeded {
            requested: Duration::MAX,
            allowed,
        }),
        Cooldown::Absent => {
            let delay = backoff_delay(attempt).min(options.max_retry_delay);
            if delay > remaining {
                return Err(AiError::Timeout(TimeoutStage::Total));
            }
            Ok(delay)
        }
    }
}

/// Deterministic exponential backoff: 500ms, 1s, 2s, 4s, then 8s.
fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_millis(500).saturating_mul(1u32 << attempt.min(4))
}

fn remaining_budget(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Read at most [`MAX_ERROR_BODY_BYTES`] of an error response and classify it.
///
/// The body is only inspected for an allowlisted machine-readable code; its text is never kept.
/// A stalled body cannot replace cancellation or a deadline with an HTTP status: cancellation
/// drops this future through the `run` select, and the inactivity or total deadline is reported
/// as [`AiError::Timeout`].
async fn read_error_body(
    response: reqwest::Response,
    deadline: Instant,
    inactivity: Duration,
) -> Result<String, AiError> {
    let mut buffer = Vec::new();
    let body = response.bytes_stream();
    tokio::pin!(body);
    while buffer.len() < MAX_ERROR_BODY_BYTES {
        let next = timeout_at(stage_deadline(deadline, inactivity), body.next()).await;
        match next {
            Ok(Some(Ok(bytes))) => append_error_bytes(&mut buffer, &bytes),
            // An unreadable body adds no classification; the HTTP status still stands.
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_elapsed) => return Err(body_timeout_error(deadline)),
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn append_error_bytes(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(buffer.len());
    buffer.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

/// A stage can never outlive the already checked total deadline. Check again at use time:
/// a duration schedulable during validation may overflow after the clock advances.
fn stage_deadline(total: Instant, duration: Duration) -> Instant {
    stage_deadline_at(Instant::now(), total, duration)
}

fn stage_deadline_at(now: Instant, total: Instant, duration: Duration) -> Instant {
    now.checked_add(duration)
        .map_or(total, |stage| stage.min(total))
}

/// Validate every configured duration before a request is started.
fn validate_timeouts(options: &RequestOptions) -> Result<(), AiError> {
    validate_deadline("response_header_timeout", options.response_header_timeout)?;
    validate_deadline(
        "stream_inactivity_timeout",
        options.stream_inactivity_timeout,
    )?;
    validate_deadline("total_timeout", options.total_timeout)?;
    validate_delay("max_retry_delay", options.max_retry_delay)?;
    Ok(())
}

/// A deadline must be positive and schedulable.
fn validate_deadline(name: &str, duration: Duration) -> Result<(), AiError> {
    if duration.is_zero() {
        return Err(AiError::InvalidRequest(format!(
            "`{name}` must be greater than zero"
        )));
    }
    validate_delay(name, duration)
}

/// A delay must be schedulable on the monotonic clock.
fn validate_delay(name: &str, duration: Duration) -> Result<(), AiError> {
    if Instant::now().checked_add(duration).is_none() {
        return Err(AiError::InvalidRequest(format!(
            "`{name}` is too large to schedule"
        )));
    }
    Ok(())
}

/// The request's absolute deadline.
fn total_deadline(options: &RequestOptions) -> Result<Instant, AiError> {
    Instant::now()
        .checked_add(options.total_timeout)
        .ok_or_else(|| {
            AiError::InvalidRequest("`total_timeout` is too large to schedule".to_string())
        })
}

/// Name the deadline that a body read exceeded.
fn body_timeout_error(deadline: Instant) -> AiError {
    if Instant::now() >= deadline {
        AiError::Timeout(TimeoutStage::Total)
    } else {
        AiError::Timeout(TimeoutStage::StreamInactivity)
    }
}

/// Name the deadline that a header wait exceeded.
fn header_timeout_error(deadline: Instant) -> AiError {
    if Instant::now() >= deadline {
        AiError::Timeout(TimeoutStage::Total)
    } else {
        AiError::Timeout(TimeoutStage::ResponseHeaders)
    }
}

/// Classify a request failure without leaking the request URL from `reqwest`'s `Display`.
///
/// Everything that fails before response headers is a setup failure and is retried, because no
/// generated content has been published yet. A builder error is permanent and is not retried.
fn setup_failure(error: &reqwest::Error) -> SetupFailure {
    if error.is_timeout() {
        SetupFailure {
            error: AiError::Transport("the connection to the provider timed out".to_string()),
            retryable: true,
        }
    } else if error.is_connect() {
        SetupFailure {
            error: AiError::Transport("could not connect to the provider".to_string()),
            retryable: true,
        }
    } else if error.is_builder() {
        SetupFailure {
            error: AiError::Transport("the provider request could not be built".to_string()),
            retryable: false,
        }
    } else {
        SetupFailure {
            error: AiError::Transport(
                "the connection to the provider failed before a response arrived".to_string(),
            ),
            retryable: true,
        }
    }
}

fn body_failure(error: &reqwest::Error) -> AiError {
    if error.is_timeout() {
        AiError::Transport("reading the response body timed out".to_string())
    } else if error.is_body() {
        AiError::Transport("the response body was interrupted".to_string())
    } else if error.is_decode() {
        AiError::Transport("the response body could not be decoded".to_string())
    } else {
        AiError::Transport("the response stream failed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("name"),
                HeaderValue::from_str(value).expect("value"),
            );
        }
        headers
    }

    fn options_with(max_retry_delay: Duration) -> RequestOptions {
        RequestOptions {
            max_retry_delay,
            ..RequestOptions::default()
        }
    }

    #[test]
    fn parses_numeric_cooldowns() {
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "2")])),
            Cooldown::After(Duration::from_secs(2))
        );
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after-ms", "1500")])),
            Cooldown::After(Duration::from_millis(1500))
        );
        // `Retry-After-Ms` wins over `Retry-After`.
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after-ms", "20"), ("retry-after", "9")])),
            Cooldown::After(Duration::from_millis(20))
        );
        // Expired and negative cooldowns retry immediately.
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "-5")])),
            Cooldown::After(Duration::ZERO)
        );
    }

    #[test]
    fn ignores_unusable_cooldowns() {
        assert_eq!(requested_cooldown(&HeaderMap::new()), Cooldown::Absent);
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "later please")])),
            Cooldown::Absent
        );
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "nan")])),
            Cooldown::Absent
        );
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "inf")])),
            Cooldown::Absent
        );
    }

    #[test]
    fn parses_http_date_cooldowns() {
        // A past date has already expired.
        assert_eq!(
            requested_cooldown(&headers(&[(
                "retry-after",
                "Sun, 06 Nov 1994 08:49:37 GMT"
            )])),
            Cooldown::After(Duration::ZERO)
        );
        // All HTTP-date forms are parsed, not ignored. Use 2030 for RFC 850's
        // two-digit year interpretation (dates >50 years away refer to the past).
        for date in [
            "Tue, 01 Jan 2030 08:49:37 GMT",
            "Tuesday, 01-Jan-30 08:49:37 GMT",
            "Tue Jan  1 08:49:37 2030",
        ] {
            match requested_cooldown(&headers(&[("retry-after", date)])) {
                Cooldown::After(delay) => assert!(delay > Duration::from_secs(60 * 60)),
                other => panic!("expected a parsed date for {date}, got {other:?}"),
            }
        }
        // Text that is neither a number nor a date is ignored.
        assert_eq!(
            requested_cooldown(&headers(&[("retry-after", "as soon as possible")])),
            Cooldown::Absent
        );
        // A date the parser rejects is also ignored, never guessed at.
        assert!(matches!(
            requested_cooldown(&headers(&[("retry-after", "21 Oct 2015 07:28:00")])),
            Cooldown::Absent | Cooldown::After(Duration::ZERO)
        ));
    }

    #[test]
    fn refuses_unrepresentable_cooldowns_without_panicking() {
        let deadline = Instant::now() + Duration::from_secs(60);
        for value in ["1e30", "1e300", "1e308", "9e30"] {
            let cooldown = requested_cooldown(&headers(&[("retry-after-ms", value)]));
            assert_eq!(cooldown, Cooldown::Unrepresentable, "value {value}");
            assert_eq!(
                plan_retry_delay(cooldown, 0, &options_with(Duration::from_secs(5)), deadline),
                Err(AiError::RetryDelayExceeded {
                    requested: Duration::MAX,
                    allowed: Duration::from_secs(5),
                })
            );
        }
    }

    #[test]
    fn plans_in_budget_and_refused_cooldowns() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let retry_options = options_with(Duration::from_secs(5));

        assert_eq!(
            plan_retry_delay(
                Cooldown::After(Duration::from_secs(1)),
                0,
                &retry_options,
                deadline
            ),
            Ok(Duration::from_secs(1))
        );
        // Provider cooldowns are never shortened.
        assert_eq!(
            plan_retry_delay(
                Cooldown::After(Duration::from_secs(3600)),
                0,
                &retry_options,
                deadline
            ),
            Err(AiError::RetryDelayExceeded {
                requested: Duration::from_secs(3600),
                allowed: Duration::from_secs(5),
            })
        );
        // The remaining total budget also caps what may be awaited.
        let short = Instant::now() + Duration::from_millis(120);
        assert!(matches!(
            plan_retry_delay(
                Cooldown::After(Duration::from_secs(2)),
                0,
                &retry_options,
                short
            ),
            Err(AiError::RetryDelayExceeded { requested, allowed })
                if requested == Duration::from_secs(2) && allowed < Duration::from_secs(1)
        ));
        // The transport's own backoff is skipped when it cannot fit.
        assert_eq!(
            plan_retry_delay(Cooldown::Absent, 0, &retry_options, short),
            Err(AiError::Timeout(TimeoutStage::Total))
        );
        assert_eq!(
            plan_retry_delay(Cooldown::Absent, 0, &retry_options, deadline),
            Ok(Duration::from_millis(500))
        );
        assert_eq!(
            plan_retry_delay(Cooldown::Absent, 0, &options_with(Duration::ZERO), deadline),
            Ok(Duration::ZERO)
        );
    }

    #[test]
    fn rejects_unschedulable_durations() {
        for (name, options) in [
            (
                "total_timeout",
                RequestOptions {
                    total_timeout: Duration::MAX,
                    ..RequestOptions::default()
                },
            ),
            (
                "response_header_timeout",
                RequestOptions {
                    response_header_timeout: Duration::MAX,
                    ..RequestOptions::default()
                },
            ),
            (
                "stream_inactivity_timeout",
                RequestOptions {
                    stream_inactivity_timeout: Duration::MAX,
                    ..RequestOptions::default()
                },
            ),
            (
                "max_retry_delay",
                RequestOptions {
                    max_retry_delay: Duration::MAX,
                    ..RequestOptions::default()
                },
            ),
        ] {
            match validate_timeouts(&options) {
                Err(AiError::InvalidRequest(message)) => {
                    assert!(message.contains(name), "{message}");
                }
                other => panic!("{name} was accepted: {other:?}"),
            }
        }
        // A zero retry budget means "retry immediately" and stays valid.
        assert!(validate_timeouts(&RequestOptions {
            max_retry_delay: Duration::ZERO,
            ..RequestOptions::default()
        })
        .is_ok());
    }

    #[test]
    fn stage_deadlines_are_checked_after_clock_advances() {
        let initial = Instant::now();
        // Find a duration schedulable at validation time but not after the clock advances.
        let (mut low, mut high) = (0u64, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2 + 1;
            if initial.checked_add(Duration::from_secs(middle)).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let boundary = Duration::from_secs(low);
        assert!(initial.checked_add(boundary).is_some());
        let advanced = initial.checked_add(Duration::from_secs(2)).unwrap();
        assert!(advanced.checked_add(boundary).is_none());
        let total = advanced.checked_add(Duration::from_secs(30)).unwrap();
        assert_eq!(stage_deadline_at(advanced, total, boundary), total);
        assert_eq!(stage_deadline_at(advanced, total, Duration::MAX), total);
    }

    #[test]
    fn error_body_copy_never_exceeds_its_limit() {
        let mut buffer = vec![b'a'; MAX_ERROR_BODY_BYTES - 10];
        append_error_bytes(&mut buffer, &vec![b'b'; MAX_ERROR_BODY_BYTES * 4]);
        assert_eq!(buffer.len(), MAX_ERROR_BODY_BYTES);
        assert_eq!(&buffer[MAX_ERROR_BODY_BYTES - 10..], &[b'b'; 10]);
        append_error_bytes(&mut buffer, b"not copied");
        assert_eq!(buffer.len(), MAX_ERROR_BODY_BYTES);
    }

    #[test]
    fn rejects_zero_deadlines() {
        for (name, options) in [
            (
                "response_header_timeout",
                RequestOptions {
                    response_header_timeout: Duration::ZERO,
                    ..RequestOptions::default()
                },
            ),
            (
                "stream_inactivity_timeout",
                RequestOptions {
                    stream_inactivity_timeout: Duration::ZERO,
                    ..RequestOptions::default()
                },
            ),
            (
                "total_timeout",
                RequestOptions {
                    total_timeout: Duration::ZERO,
                    ..RequestOptions::default()
                },
            ),
        ] {
            assert!(validate_timeouts(&options).is_err(), "{name}");
        }
        assert!(validate_timeouts(&RequestOptions::default()).is_ok());
    }

    #[test]
    fn huge_finite_server_numbers_do_not_panic() {
        let deadline = Instant::now() + Duration::from_secs(30);
        for value in ["1e300", "1.7976931348623157e308", "1e400"] {
            let cooldown = requested_cooldown(&headers(&[("retry-after", value)]));
            assert!(matches!(
                plan_retry_delay(cooldown, 0, &options_with(Duration::from_secs(5)), deadline),
                Err(AiError::RetryDelayExceeded { .. })
            ));
            let cooldown_ms = requested_cooldown(&headers(&[("retry-after-ms", value)]));
            assert!(matches!(
                plan_retry_delay(
                    cooldown_ms,
                    0,
                    &options_with(Duration::from_secs(5)),
                    deadline
                ),
                Err(AiError::RetryDelayExceeded { .. })
            ));
        }
    }
}
