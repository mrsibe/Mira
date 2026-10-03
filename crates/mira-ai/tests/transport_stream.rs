#![cfg(feature = "openai-completions")]

//! Wire-level streaming tests against a loopback server.

mod support;

use std::sync::Arc;
use std::time::Duration;

use mira_ai::api::openai_completions::{OpenAiCompletionsConfig, OpenAiCompletionsProvider};
use mira_ai::{
    AiError, AssistantContent, AssistantEvent, BlockKind, Context, Provider, StopReason,
};
use support::*;

/// Run one request and collect its events plus terminal message.
async fn run(
    provider: &OpenAiCompletionsProvider,
    context: Context,
) -> (
    Vec<AssistantEvent>,
    Result<mira_ai::AssistantMessage, AiError>,
) {
    let mut stream = provider.stream(request(context)).expect("stream");
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let outcome = stream.result().await;
    (events, outcome)
}

#[tokio::test]
async fn streams_immutable_block_events() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"id": "chatcmpl-1", "model": "test-model-2026", "choices": [{"delta": {"content": "He"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "llo"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (events, outcome) = run(&provider, Context::user_text("hi")).await;
    let message = outcome.expect("result");

    assert!(matches!(events[0], AssistantEvent::Start { .. }));
    assert_eq!(
        events[1],
        AssistantEvent::BlockStart {
            index: 0,
            kind: BlockKind::Text
        }
    );
    assert_eq!(
        events[2],
        AssistantEvent::BlockDelta {
            index: 0,
            delta: Arc::from("He")
        }
    );
    assert_eq!(events.len(), 5);
    assert_eq!(
        events[4],
        AssistantEvent::BlockEnd {
            index: 0,
            block: AssistantContent::Text(mira_ai::TextBlock::new("Hello")),
        }
    );
    assert_eq!(message.text(), "Hello");
    assert_eq!(message.stop_reason, StopReason::EndTurn);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.source.response_id.as_deref(), Some("chatcmpl-1"));
    assert_eq!(
        message.source.response_model.as_deref(),
        Some("test-model-2026")
    );
    assert_eq!(message.usage, None);
    assert_eq!(message.error_message, None);
}

#[tokio::test]
async fn keeps_thinking_and_text_in_separate_blocks() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"reasoning_content": "think"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "answer"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"reasoning_content": "ing"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    let message = outcome.expect("result");

    assert_eq!(message.content.len(), 2);
    assert_eq!(
        message.content[0],
        AssistantContent::Thinking(mira_ai::ThinkingBlock {
            thinking: "thinking".to_string(),
            signature: Some("reasoning_content".to_string()),
            redacted: false,
        })
    );
    assert_eq!(message.text(), "answer");
}

#[tokio::test]
async fn reassembles_events_split_at_every_byte() {
    let server = TestServer::start().await;
    let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"日本語 🎉 café\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let chunks: Vec<(Duration, Vec<u8>)> = payload
        .as_bytes()
        .chunks(1)
        .map(|byte| (Duration::ZERO, byte.to_vec()))
        .collect();
    server.enqueue(Reply::sse_chunks(chunks, ChunkedEnd::Terminate));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    assert_eq!(outcome.expect("result").text(), "日本語 🎉 café");
}

#[tokio::test]
async fn accepts_comments_empty_frames_crlf_and_multiline_data() {
    let body = ": keep-alive\r\nevent: message\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"o\"}}]}\r\n\r\n\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"k\"}\r\ndata: }]}\r\n\r\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    let server = TestServer::start().await;
    server.enqueue(Reply::sse(body));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    assert_eq!(outcome.expect("result").text(), "ok");
}

#[tokio::test]
async fn accumulates_multiple_interleaved_tool_calls() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_a", "function": {"name": "add", "arguments": "{\"a\""}},
            {"index": 1, "id": "call_b", "function": {"name": "sub", "arguments": "{\"b\""}}
        ]}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {"tool_calls": [
            {"index": 1, "function": {"arguments": ":2}"}},
            {"index": 0, "function": {"arguments": ":1}"}}
        ]}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (events, outcome) = run(&provider, Context::user_text("hi")).await;
    let message = outcome.expect("result");

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let calls: Vec<_> = message.tool_calls().collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "add");
    assert_eq!(calls[0].arguments.as_ref().expect("arguments")["a"], 1);
    assert_eq!(calls[1].name, "sub");
    assert_eq!(calls[1].arguments.as_ref().expect("arguments")["b"], 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AssistantEvent::BlockEnd { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn preserves_truncated_tool_arguments_without_making_them_executable() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_a", "function": {"name": "add", "arguments": "{\"a\": 1"}}
        ]}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "length"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    let message = outcome.expect("result");

    assert_eq!(message.stop_reason, StopReason::MaxTokens);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("length"));
    let call = message.tool_calls().next().expect("tool call");
    assert_eq!(call.arguments_raw, "{\"a\": 1");
    assert_eq!(call.arguments, None);
}

#[tokio::test]
async fn reports_rejected_tool_arguments() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_a", "function": {"name": "add", "arguments": "not json"}}
        ]}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    let call = outcome
        .expect("result")
        .tool_calls()
        .next()
        .expect("tool call")
        .clone();
    assert_eq!(call.arguments, None);
    assert_eq!(call.arguments_raw, "not json");
}

#[tokio::test]
async fn applies_usage_from_a_trailing_usage_chunk() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "hi"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        &json_frame(serde_json::json!({"choices": [], "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 4},
            "completion_tokens_details": {"reasoning_tokens": 2}
        }})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    let usage = outcome.expect("result").usage.expect("usage");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cached_input_tokens, Some(4));
    assert_eq!(usage.reasoning_tokens, Some(2));
}

#[tokio::test]
async fn leaves_usage_and_reasoning_absent_when_the_provider_omits_them() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "hi"}}]})),
        &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    let message = outcome.expect("result");
    assert_eq!(message.usage, None);
}

#[tokio::test]
async fn fails_when_the_stream_ends_without_a_finish_reason() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "partial"}}]}),
    )]));
    let provider = provider_for(&server);

    let (events, outcome) = run(&provider, Context::user_text("hi")).await;
    assert_eq!(outcome.expect_err("incomplete"), AiError::IncompleteStream);
    assert!(events
        .iter()
        .any(|event| matches!(event, AssistantEvent::BlockDelta { .. })));
}

#[tokio::test]
async fn fails_when_done_arrives_without_a_finish_reason() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "partial"}}]})),
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    assert_eq!(outcome.expect_err("incomplete"), AiError::IncompleteStream);
}

#[tokio::test]
async fn fails_on_a_malformed_event_payload() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[
        "data: {not json\n\n",
        "data: [DONE]\n\n",
    ]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    match outcome.expect_err("protocol error") {
        AiError::Protocol(message) => assert!(message.contains("not a valid chunk")),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn fails_on_an_oversized_event() {
    let server = TestServer::start().await;
    let mut config = OpenAiCompletionsConfig::new(server.base_url()).expect("config");
    config.max_sse_event_bytes = 64;
    let provider = OpenAiCompletionsProvider::new(config).expect("provider");
    server.enqueue(Reply::sse_frames(&[
        &json_frame(serde_json::json!({"choices": [{"delta": {"content": "x".repeat(2000)}}]})),
        "data: [DONE]\n\n",
    ]));

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    match outcome.expect_err("oversized event") {
        AiError::Protocol(message) => assert!(message.contains("exceeded 64 bytes")),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn fails_on_a_provider_error_payload() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_frames(&[&json_frame(
        serde_json::json!({"error": {"message": "overloaded"}}),
    )]));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    // The provider's own text is not part of the error; only the category is.
    let error = outcome.expect_err("provider error");
    assert_eq!(
        error,
        AiError::ProviderStream {
            failure: mira_ai::ProviderFailure::StreamError
        }
    );
    assert!(!format!("{error}").contains("overloaded"));
}

#[tokio::test]
async fn maps_filtered_and_unknown_finish_reasons_to_a_failed_turn() {
    // The description is a constant; the provider value is preserved as bounded data only.
    let expected = "the provider stopped the response without usable output";
    for raw in ["content_filter", "insufficient_system_resource"] {
        let server = TestServer::start().await;
        server.enqueue(Reply::sse_frames(&[
            &json_frame(serde_json::json!({"choices": [{"delta": {"content": "x"}}]})),
            &json_frame(serde_json::json!({"choices": [{"delta": {}, "finish_reason": raw}]})),
            "data: [DONE]\n\n",
        ]));
        let provider = provider_for(&server);

        let (_, outcome) = run(&provider, Context::user_text("hi")).await;
        let message = outcome.expect("result");
        assert_eq!(message.stop_reason, StopReason::Failed);
        assert_eq!(message.raw_stop_reason.as_deref(), Some(raw));
        assert_eq!(message.error_message.as_deref(), Some(expected));
        assert!(!expected.contains(raw));
    }
}

#[tokio::test]
async fn fails_on_an_interrupted_body_without_retrying() {
    let server = TestServer::start().await;
    server.enqueue(Reply::sse_truncated(&[&json_frame(
        serde_json::json!({"choices": [{"delta": {"content": "half"}}]}),
    )]));
    server.enqueue(text_reply("must not be used"));
    let provider = provider_for(&server);

    let (_, outcome) = run(&provider, Context::user_text("hi")).await;
    assert!(matches!(
        outcome.expect_err("interrupted"),
        AiError::Transport(_)
    ));
    assert_eq!(server.request_count(), 1);
}
