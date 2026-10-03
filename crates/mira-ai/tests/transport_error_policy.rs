//! Regression tests for the error and redirect policy: no provider prose, no secrets and no
//! redirect following, and typed outcomes for stalled error bodies.

#![cfg(feature = "openai-completions")]

mod support;

use std::time::Duration;

use mira_ai::{AiError, Context, Provider, ProviderFailure, RequestOptions, TimeoutStage};
use support::*;

const PROMPT: &str = "PRIVATE-PROMPT-TEXT";
const CUSTOM_HEADER_SECRET: &str = "custom-header-secret-value";

/// Provider with a custom credential header, as an OpenAI-compatible gateway would use.
fn provider_with_custom_header(
    server: &TestServer,
) -> mira_ai::api::openai_completions::OpenAiCompletionsProvider {
    let mut config = config_for(server);
    config.extra_headers = vec![("x-api-key".to_string(), CUSTOM_HEADER_SECRET.to_string())];
    mira_ai::api::openai_completions::OpenAiCompletionsProvider::new(config).expect("provider")
}

fn assert_no_secrets(error: &AiError) {
    let rendered = format!("{error} {error:?}");
    for needle in [
        PROMPT,
        TEST_CREDENTIAL,
        CUSTOM_HEADER_SECRET,
        "PRIVATE",
        "127.0.0.1",
    ] {
        assert!(
            !rendered.contains(needle),
            "'{needle}' leaked into: {rendered}"
        );
    }
}

#[tokio::test]
async fn http_error_bodies_never_reach_the_error() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        400,
        serde_json::json!({
            "error": {
                "message": format!(
                    "rejected prompt: {PROMPT}; bearer {TEST_CREDENTIAL}; x-api-key: {CUSTOM_HEADER_SECRET}"
                ),
                "type": "invalid_request_error"
            }
        }),
    ));
    let provider = provider_with_custom_header(&server);

    let error = provider
        .stream(request(Context::user_text(PROMPT)))
        .expect("stream")
        .result()
        .await
        .expect_err("provider error");

    assert_eq!(
        error,
        AiError::Provider {
            status: 400,
            failure: ProviderFailure::InvalidRequest
        }
    );
    assert_no_secrets(&error);
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn provider_codes_are_mapped_to_allowlisted_categories() {
    for (status, code, expected) in [
        (
            400,
            "context_length_exceeded",
            ProviderFailure::ContextOverflow,
        ),
        (400, "rate_limit_exceeded", ProviderFailure::RateLimited),
        (500, "invalid_api_key", ProviderFailure::Unauthorized),
        (400, "totally_unknown_code", ProviderFailure::InvalidRequest),
    ] {
        let server = TestServer::start().await;
        server.enqueue(Reply::json(
            status,
            serde_json::json!({
                "error": { "message": format!("{PROMPT} {TEST_CREDENTIAL}"), "code": code }
            }),
        ));
        let provider = provider_with_custom_header(&server);
        let mut classified = request(Context::user_text(PROMPT));
        // Classification only: never retry in this test.
        classified.options.max_retries = 0;

        let error = provider
            .stream(classified)
            .expect("stream")
            .result()
            .await
            .expect_err("provider error");

        assert_eq!(
            error,
            AiError::Provider {
                status,
                failure: expected
            }
        );
        assert_no_secrets(&error);
        assert_eq!(server.request_count(), 1);
    }
}

#[tokio::test]
async fn stream_error_payloads_never_reach_the_error() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[&json_frame(serde_json::json!({
        "error": {
            "message": format!(
                "rejected prompt: {PROMPT}; bearer {TEST_CREDENTIAL}; x-api-key: {CUSTOM_HEADER_SECRET}"
            ),
            "type": "server_error"
        }
    }))]));
    let provider = provider_with_custom_header(&server);

    let error = provider
        .stream(request(Context::user_text(PROMPT)))
        .expect("stream")
        .result()
        .await
        .expect_err("provider stream error");

    assert_eq!(
        error,
        AiError::ProviderStream {
            failure: ProviderFailure::Unavailable
        }
    );
    assert_no_secrets(&error);
}

#[tokio::test]
async fn unknown_finish_reasons_use_a_constant_description() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "x"}}]})),
        &json_frame(
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": format!("weird-{TEST_CREDENTIAL}-{PROMPT}")}]}),
        ),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_with_custom_header(&server);

    let message = provider
        .stream(request(Context::user_text(PROMPT)))
        .expect("stream")
        .result()
        .await
        .expect("message");

    assert_eq!(
        message.error_message.as_deref(),
        Some("the provider stopped the response without usable output")
    );
    let rendered = format!("{:?}", message.error_message);
    assert!(!rendered.contains(TEST_CREDENTIAL));
    assert!(!rendered.contains(PROMPT));
}

#[tokio::test]
async fn redirects_are_refused_and_never_reach_the_second_endpoint() {
    let first = TestServer::start().await;
    let second = TestServer::start().await;
    first.enqueue(Reply::redirect(&format!(
        "{}/v1/chat/completions",
        second.base_url()
    )));
    let provider = provider_with_custom_header(&first);

    let error = provider
        .stream(request(Context::user_text(PROMPT)))
        .expect("stream")
        .result()
        .await
        .expect_err("redirect");

    assert_eq!(
        error,
        AiError::Provider {
            status: 302,
            failure: ProviderFailure::Redirect
        }
    );
    assert_no_secrets(&error);
    assert_eq!(first.request_count(), 1);
    assert_eq!(
        second.request_count(),
        0,
        "the redirected endpoint must not be contacted"
    );
}

#[tokio::test]
async fn redirects_are_classified_without_reading_their_bodies() {
    for status in [302, 307] {
        for stalled in [false, true] {
            let server = TestServer::start().await;
            server.enqueue(if stalled {
                Reply::stalled_body(status)
            } else {
                Reply::json(
                    status,
                    serde_json::json!({"error": {"code": "invalid_api_key"}}),
                )
            });
            let provider = provider_for(&server);
            let outcome = tokio::time::timeout(
                Duration::from_secs(1),
                provider
                    .stream(request(Context::user_text(PROMPT)))
                    .unwrap()
                    .result(),
            )
            .await
            .expect("redirect classification must not wait for a body");
            assert_eq!(
                outcome,
                Err(AiError::Provider {
                    status,
                    failure: ProviderFailure::Redirect
                })
            );
            assert_eq!(server.request_count(), 1);
        }
    }
}

#[tokio::test]
async fn cancelling_a_stalled_error_body_reports_cancellation() {
    let server = TestServer::start().await;
    // Headers arrive with a 400, then the body stalls forever.
    server.enqueue(Reply::stalled_body(400));
    let provider = provider_for(&server);
    let mut request = request(Context::user_text(PROMPT));
    request.options = RequestOptions {
        response_header_timeout: Duration::from_secs(5),
        stream_inactivity_timeout: Duration::from_secs(30),
        total_timeout: Duration::from_secs(30),
        ..RequestOptions::default()
    };

    let stream = provider.stream(request).expect("stream");
    let cancellation = stream.cancellation();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
    });

    let error = stream.result().await.expect_err("cancelled");
    assert_eq!(error, AiError::Cancelled);
}

#[tokio::test]
async fn timing_out_a_stalled_error_body_reports_a_timeout() {
    let server = TestServer::start().await;
    server.enqueue(Reply::stalled_body(400));
    let provider = provider_for(&server);
    let mut request = request(Context::user_text(PROMPT));
    request.options = RequestOptions {
        response_header_timeout: Duration::from_secs(5),
        stream_inactivity_timeout: Duration::from_millis(150),
        total_timeout: Duration::from_secs(30),
        ..RequestOptions::default()
    };

    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("timeout");
    assert_eq!(error, AiError::Timeout(TimeoutStage::StreamInactivity));
}
