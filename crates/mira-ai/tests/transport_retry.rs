#![cfg(feature = "openai-completions")]

//! Retry behavior and error sanitization against a loopback server.

mod support;

use std::time::{Duration, Instant};

use mira_ai::{AiError, Context, Credential, Provider, RequestOptions, StopReason, StreamRequest};
use support::*;

/// A request whose retry delays are short enough for tests.
fn fast_retry_request(context: Context, max_retry_delay: Duration) -> StreamRequest {
    let mut request = request(context);
    request.options = RequestOptions {
        max_retry_delay,
        ..RequestOptions::default()
    };
    request
}

#[tokio::test]
async fn retries_server_errors_and_rate_limits_until_success() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        500,
        serde_json::json!({"error": {"message": "boom"}}),
    ));
    server.enqueue(Reply::json(
        429,
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("recovered"));
    let provider = provider_for(&server);

    let outcome = provider
        .stream(fast_retry_request(Context::user_text("hi"), Duration::ZERO))
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert_eq!(outcome.text(), "recovered");
    assert_eq!(server.request_count(), 3);
}

#[tokio::test]
async fn refuses_a_provider_cooldown_above_the_allowed_wait() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        vec![("retry-after".to_string(), "3600".to_string())],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let started = Instant::now();
    let error = provider
        .stream(fast_retry_request(
            Context::user_text("hi"),
            Duration::from_millis(150),
        ))
        .expect("stream")
        .result()
        .await
        .expect_err("cooldown refusal");
    let elapsed = started.elapsed();

    assert_eq!(
        error,
        AiError::RetryDelayExceeded {
            requested: Duration::from_secs(3600),
            allowed: Duration::from_millis(150),
        }
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "a refused cooldown must not be waited for, took {elapsed:?}"
    );
    assert_eq!(
        server.request_count(),
        1,
        "no second attempt may be sent after a refused cooldown"
    );
}

#[tokio::test]
async fn refuses_a_provider_cooldown_above_the_remaining_budget() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        vec![("retry-after".to_string(), "2".to_string())],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        max_retry_delay: Duration::from_secs(30),
        total_timeout: Duration::from_millis(200),
        response_header_timeout: Duration::from_secs(5),
        stream_inactivity_timeout: Duration::from_secs(5),
        ..RequestOptions::default()
    };

    let started = Instant::now();
    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("cooldown refusal");

    match error {
        AiError::RetryDelayExceeded { requested, allowed } => {
            assert_eq!(requested, Duration::from_secs(2));
            assert!(
                allowed <= Duration::from_millis(200),
                "allowed: {allowed:?}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn honors_an_in_budget_provider_cooldown() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        vec![("retry-after-ms".to_string(), "150".to_string())],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("recovered"));
    let provider = provider_for(&server);

    let started = Instant::now();
    let outcome = provider
        .stream(fast_retry_request(
            Context::user_text("hi"),
            Duration::from_secs(5),
        ))
        .expect("stream")
        .result()
        .await
        .expect("result");
    let elapsed = started.elapsed();

    assert_eq!(outcome.text(), "recovered");
    assert!(
        elapsed >= Duration::from_millis(150),
        "the in-budget cooldown must be waited for, took {elapsed:?}"
    );
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn refuses_an_http_date_cooldown_beyond_the_budget() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        vec![(
            "retry-after".to_string(),
            "Sat, 06 Nov 2094 08:49:37 GMT".to_string(),
        )],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let error = provider
        .stream(fast_retry_request(
            Context::user_text("hi"),
            Duration::from_secs(5),
        ))
        .expect("stream")
        .result()
        .await
        .expect_err("date cooldown refusal");

    match error {
        AiError::RetryDelayExceeded { requested, allowed } => {
            assert!(
                requested > Duration::from_secs(60 * 60 * 24),
                "requested: {requested:?}"
            );
            assert_eq!(allowed, Duration::from_secs(5));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(
        server.request_count(),
        1,
        "a parsed HTTP-date must not fall back to a retry"
    );
}

#[tokio::test]
async fn refuses_overflow_and_all_standard_future_http_dates() {
    for value in [
        "9".repeat(400),
        "Tue, 01 Jan 2030 08:49:37 GMT".to_string(),
        "Tuesday, 01-Jan-30 08:49:37 GMT".to_string(),
        "Tue Jan  1 08:49:37 2030".to_string(),
    ] {
        let server = TestServer::start().await;
        server.enqueue(Reply::json_with_headers(
            429,
            vec![("retry-after".to_string(), value.clone())],
            serde_json::json!({}),
        ));
        server.enqueue(text_reply("must not be used"));
        let provider = provider_for(&server);
        let error = provider
            .stream(fast_retry_request(
                Context::user_text("hi"),
                Duration::from_secs(5),
            ))
            .unwrap()
            .result()
            .await
            .unwrap_err();
        assert!(
            matches!(error, AiError::RetryDelayExceeded { .. }),
            "{value}: {error:?}"
        );
        assert_eq!(server.request_count(), 1);
    }
}

#[tokio::test]
async fn treats_an_expired_http_date_cooldown_as_immediate() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        vec![(
            "retry-after".to_string(),
            "Sun, 06 Nov 1994 08:49:37 GMT".to_string(),
        )],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    server.enqueue(text_reply("recovered"));
    let provider = provider_for(&server);

    let outcome = provider
        .stream(fast_retry_request(
            Context::user_text("hi"),
            Duration::from_secs(5),
        ))
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(outcome.text(), "recovered");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn refuses_an_unrepresentable_cooldown_instead_of_panicking() {
    for max_retries in [2, 0] {
        let server = TestServer::start().await;
        server.enqueue(Reply::json_with_headers(
            429,
            vec![("retry-after-ms".to_string(), "1e300".to_string())],
            serde_json::json!({"error": {"message": "slow down"}}),
        ));
        server.enqueue(text_reply("must not be used"));
        let provider = provider_for(&server);

        let mut request = fast_retry_request(Context::user_text("hi"), Duration::from_secs(5));
        request.options.max_retries = max_retries;
        let error = provider
            .stream(request)
            .expect("stream")
            .result()
            .await
            .expect_err("huge cooldown");

        if max_retries == 0 {
            // No retry was going to happen, so the status is reported instead of a refusal.
            assert_eq!(
                error,
                AiError::Provider {
                    status: 429,
                    failure: mira_ai::ProviderFailure::RateLimited
                }
            );
        } else {
            assert_eq!(
                error,
                AiError::RetryDelayExceeded {
                    requested: Duration::MAX,
                    allowed: Duration::from_secs(5),
                }
            );
        }
        assert_eq!(server.request_count(), 1);
    }
}

#[tokio::test]
async fn rejects_unschedulable_request_options_before_sending() {
    let server = TestServer::start().await;
    let provider = provider_for(&server);

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
            "max_retry_delay",
            RequestOptions {
                max_retry_delay: Duration::MAX,
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
    ] {
        let mut request = request(Context::user_text("hi"));
        request.options = options;
        match provider.stream(request) {
            Err(AiError::InvalidRequest(message)) => assert!(message.contains(name), "{message}"),
            other => panic!("{name} was accepted: {other:?}"),
        }
    }
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn rejects_unschedulable_connect_timeouts() {
    let server = TestServer::start().await;
    for connect_timeout in [Duration::MAX, Duration::ZERO] {
        let mut config = config_for(&server);
        config.connect_timeout = connect_timeout;
        match mira_ai::api::openai_completions::OpenAiCompletionsProvider::new(config) {
            Err(AiError::InvalidRequest(message)) => {
                assert!(message.contains("connect_timeout"), "{message}")
            }
            other => panic!("{connect_timeout:?} was accepted: {other:?}"),
        }
    }
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn retries_a_connection_closed_before_the_response() {
    let server = TestServer::start().await;
    server.enqueue(Reply::closed());
    server.enqueue(text_reply("recovered"));
    let provider = provider_for(&server);

    let outcome = provider
        .stream(fast_retry_request(Context::user_text("hi"), Duration::ZERO))
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(outcome.text(), "recovered");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn does_not_retry_client_errors_and_never_surfaces_prose() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        400,
        serde_json::json!({
            "error": {
                "message": format!("invalid key {TEST_CREDENTIAL} for prompt: secret prompt text"),
            }
        }),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let error = provider
        .stream(fast_retry_request(Context::user_text("hi"), Duration::ZERO))
        .expect("stream")
        .result()
        .await
        .expect_err("client error");

    assert_eq!(
        error,
        AiError::Provider {
            status: 400,
            failure: mira_ai::ProviderFailure::InvalidRequest
        }
    );
    let rendered = format!("{error} {error:?}");
    for needle in [
        TEST_CREDENTIAL,
        "secret prompt text",
        "invalid key",
        "127.0.0.1",
    ] {
        assert!(
            !rendered.contains(needle),
            "'{needle}' leaked into: {rendered}"
        );
    }
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn does_not_leak_an_html_error_body() {
    let server = TestServer::start().await;
    server.enqueue(Reply::text(401, "<html>Unauthorized: bad key</html>"));
    let provider = provider_for(&server);

    let error = provider
        .stream(fast_retry_request(Context::user_text("hi"), Duration::ZERO))
        .expect("stream")
        .result()
        .await
        .expect_err("auth error");

    assert_eq!(
        error,
        AiError::Provider {
            status: 401,
            failure: mira_ai::ProviderFailure::Unauthorized
        }
    );
    assert!(!format!("{error}").contains("<html>"));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn stops_after_the_retry_limit() {
    let server = TestServer::start().await;
    for _ in 0..5 {
        server.enqueue(Reply::json(
            500,
            serde_json::json!({"error": {"message": "boom"}}),
        ));
    }
    let provider = provider_for(&server);

    let error = provider
        .stream(fast_retry_request(Context::user_text("hi"), Duration::ZERO))
        .expect("stream")
        .result()
        .await
        .expect_err("retry limit");

    assert!(matches!(error, AiError::Provider { status: 500, .. }));
    assert_eq!(
        server.request_count(),
        3,
        "two retries means three attempts"
    );
}

#[tokio::test]
async fn honors_a_zero_retry_budget() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        500,
        serde_json::json!({"error": {"message": "boom"}}),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options.max_retries = 0;
    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("no retries");

    assert!(matches!(error, AiError::Provider { status: 500, .. }));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn cancels_while_waiting_between_attempts() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json_with_headers(
        429,
        // In-budget on purpose: an over-budget cooldown is refused, not waited for.
        vec![("retry-after".to_string(), "2".to_string())],
        serde_json::json!({"error": {"message": "slow down"}}),
    ));
    let provider = provider_for(&server);
    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        max_retry_delay: Duration::from_secs(5),
        total_timeout: Duration::from_secs(30),
        ..RequestOptions::default()
    };
    request.credential = Credential::new(TEST_CREDENTIAL);

    let stream = provider.stream(request).expect("stream");
    let cancellation = stream.cancellation();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
    });

    let started = Instant::now();
    let error = stream.result().await.expect_err("cancelled");
    assert_eq!(error, AiError::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancellation must interrupt backoff, took {:?}",
        started.elapsed()
    );
    assert_eq!(server.request_count(), 1);
}
