#![cfg(feature = "openai-completions")]

//! Cancellation, deadlines, cleanup and request isolation against a loopback server.

mod support;

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use mira_ai::{AiError, Context, Credential, Provider, RequestOptions, TimeoutStage};
use support::*;

#[tokio::test]
async fn cancels_before_response_headers() {
    let server = TestServer::start().await;
    server.enqueue(Reply::silent());
    let provider = provider_for(&server);

    let stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    let started = Instant::now();
    stream.cancel();

    assert_eq!(
        stream.result().await.expect_err("cancelled"),
        AiError::Cancelled
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn cancels_in_the_middle_of_a_stream() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_stalling(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "half"}}]}),
    )]));
    let provider = provider_for(&server);

    let mut stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    let mut deltas = String::new();
    while let Some(event) = stream.recv().await {
        if let mira_ai::AssistantEvent::BlockDelta { delta, .. } = event {
            deltas.push_str(&delta);
            break;
        }
    }
    assert_eq!(deltas, "half");
    stream.cancel();

    assert_eq!(
        stream.result().await.expect_err("cancelled"),
        AiError::Cancelled
    );
}

#[tokio::test]
async fn times_out_waiting_for_response_headers() {
    let server = TestServer::start().await;
    server.enqueue(Reply::silent());
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        response_header_timeout: Duration::from_millis(150),
        ..RequestOptions::default()
    };

    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("header timeout");
    assert_eq!(error, AiError::Timeout(TimeoutStage::ResponseHeaders));
}

#[tokio::test]
async fn times_out_on_a_stalled_stream() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_stalling(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "half"}}]}),
    )]));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
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
        .expect_err("inactivity timeout");
    assert_eq!(error, AiError::Timeout(TimeoutStage::StreamInactivity));
}

#[tokio::test]
async fn times_out_at_the_total_deadline_while_content_is_still_arriving() {
    let server = TestServer::start().await;
    let chunks: Vec<(Duration, Vec<u8>)> = (0..10)
        .map(|index| {
            (
                Duration::from_millis(40),
                json_frame(serde_json::json!({
                    "choices": [{"delta": {"content": format!("{index}")}}]
                }))
                .into_bytes(),
            )
        })
        .collect();
    server.enqueue(Reply::sse_chunks(chunks, ChunkedEnd::Terminate));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        response_header_timeout: Duration::from_secs(5),
        stream_inactivity_timeout: Duration::from_secs(5),
        total_timeout: Duration::from_millis(150),
        ..RequestOptions::default()
    };

    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("total timeout");
    assert_eq!(error, AiError::Timeout(TimeoutStage::Total));
}

#[tokio::test]
async fn dropping_the_handle_cancels_the_request_and_releases_the_connection() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_stalling(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "half"}}]}),
    )]));
    let provider = provider_for(&server);

    let mut stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    while let Some(event) = stream.recv().await {
        if matches!(event, mira_ai::AssistantEvent::BlockDelta { .. }) {
            break;
        }
    }
    drop(stream);

    server.wait_for_client_closure().await;
}

#[tokio::test]
async fn isolates_credentials_across_concurrent_requests() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_echo_credential());
    server.enqueue(Reply::sse_echo_credential());
    let provider = provider_for(&server);

    let first = {
        let provider = provider.clone();
        tokio::spawn(async move {
            provider
                .stream(request(Context::user_text("first")))
                .expect("stream")
                .result()
                .await
                .expect("result")
        })
    };
    let second_credential = "sk-test-credential-second-0987654321";
    let second = {
        let provider = provider.clone();
        let context = Context::user_text("second");
        tokio::spawn(async move {
            let mut request = request(context);
            request.credential = Credential::new(second_credential);
            provider
                .stream(request)
                .expect("stream")
                .result()
                .await
                .expect("result")
        })
    };

    let (first, second) = (first.await.expect("task"), second.await.expect("task"));
    assert_eq!(first.text(), TEST_CREDENTIAL);
    assert_eq!(second.text(), second_credential);
    server.wait_for_requests(2).await;

    let mut headers: Vec<String> = server
        .requests()
        .iter()
        .filter_map(|request| request.header("authorization").map(str::to_string))
        .collect();
    headers.sort();
    let mut expected = vec![
        format!("Bearer {TEST_CREDENTIAL}"),
        format!("Bearer {second_credential}"),
    ];
    expected.sort();
    assert_eq!(headers, expected);
}

#[tokio::test]
async fn keeps_event_order_and_delivers_one_terminal_result() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "a"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_a", "function": {"name": "add", "arguments": "{}"}}
        ]}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let mut stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    let mut kinds = Vec::new();
    while let Some(event) = stream.recv().await {
        kinds.push(match event {
            mira_ai::AssistantEvent::Start { .. } => "start",
            mira_ai::AssistantEvent::BlockStart { kind, .. } => match kind {
                mira_ai::BlockKind::Text => "text_start",
                mira_ai::BlockKind::Thinking => "thinking_start",
                mira_ai::BlockKind::ToolCall => "tool_start",
            },
            mira_ai::AssistantEvent::BlockDelta { .. } => "delta",
            mira_ai::AssistantEvent::BlockEnd { .. } => "end",
            _ => "unknown",
        });
    }
    let message = stream.result().await.expect("result");

    assert_eq!(
        kinds,
        vec![
            "start",
            "text_start",
            "delta",
            "tool_start",
            "delta",
            "end",
            "end"
        ]
    );
    assert_eq!(message.text(), "a");
    assert_eq!(message.tool_calls().count(), 1);
}

#[tokio::test]
async fn terminal_result_arrives_when_only_events_were_read() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("done"));
    let provider = provider_for(&server);

    let mut stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    while stream.recv().await.is_some() {}

    let message = stream.result().await.expect("result");
    assert_eq!(message.text(), "done");
}

#[tokio::test]
async fn delivers_a_total_timeout_while_events_are_backpressured() {
    let server = TestServer::start().await;
    // A complete response in one body chunk: the producer can only finish publishing if the
    // consumer drains, and the consumer deliberately does not.
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "one"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "two"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "three"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        total_timeout: Duration::from_millis(150),
        event_buffer: NonZeroUsize::new(1).expect("non-zero"),
        ..RequestOptions::default()
    };
    let stream = provider.stream(request).expect("stream");

    // Never drain events; the buffer holds at most one.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let started = Instant::now();
    let error = stream
        .result()
        .await
        .expect_err("a backpressured request must time out, not succeed");
    assert_eq!(error, AiError::Timeout(TimeoutStage::Total));
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "the terminal outcome must already be available"
    );
}

#[tokio::test]
async fn a_retry_that_cannot_fit_the_total_budget_times_out() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        500,
        serde_json::json!({"error": {"message": "boom"}}),
    ));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        // The first backoff is 500ms; the budget is 200ms.
        total_timeout: Duration::from_millis(200),
        max_retry_delay: Duration::from_secs(30),
        response_header_timeout: Duration::from_secs(5),
        stream_inactivity_timeout: Duration::from_secs(5),
        ..RequestOptions::default()
    };

    let error = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect_err("retry budget");

    assert_eq!(error, AiError::Timeout(TimeoutStage::Total));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_bounded_backoff_still_retries_within_the_budget() {
    let server = TestServer::start().await;
    server.enqueue(Reply::json(
        503,
        serde_json::json!({"error": {"message": "boom"}}),
    ));
    server.enqueue(text_reply("recovered"));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.options = RequestOptions {
        // The 500ms backoff fits in this budget.
        total_timeout: Duration::from_secs(5),
        max_retry_delay: Duration::from_millis(200),
        ..RequestOptions::default()
    };

    let outcome = provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(outcome.text(), "recovered");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn does_not_record_a_closure_while_the_connection_is_open() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_stalling(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "half"}}]}),
    )]));
    let provider = provider_for(&server);

    let mut stream = provider
        .stream(request(Context::user_text("hi")))
        .expect("stream");
    while let Some(event) = stream.recv().await {
        if matches!(event, mira_ai::AssistantEvent::BlockDelta { .. }) {
            break;
        }
    }

    // The connection is still open: the fixture must not count a closure it never observed.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(server.client_closures(), 0);

    drop(stream);
    server.wait_for_client_closure().await;
}
