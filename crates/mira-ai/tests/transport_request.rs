#![cfg(feature = "openai-completions")]

//! Wire-level request tests against a loopback server.

mod support;

use mira_ai::api::openai_completions::{
    MaxTokensField, OpenAiCompletionsCompat, OpenAiCompletionsConfig, OpenAiCompletionsProvider,
    ReasoningEncoding,
};
use mira_ai::{
    AiError, Api, Context, Credential, InputContent, Message, Model, ModelCapabilities, Provider,
    ReasoningLevel, StopReason, ToolDefinition, ToolResultMessage, UserMessage,
};
use support::*;

#[tokio::test]
async fn posts_to_chat_completions_with_a_per_request_bearer_credential() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("hello"));
    let provider = provider_for(&server);

    let mut context = Context::user_text("hi");
    context.system_prompt = Some("be brief".to_string());
    let message = provider
        .stream(request(context))
        .expect("stream")
        .result()
        .await
        .expect("result");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].request_line(),
        "POST /chat/completions HTTP/1.1"
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {TEST_CREDENTIAL}").as_str())
    );
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    let body = requests[0].json();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "hi");
    assert_eq!(message.text(), "hello");
    assert_eq!(message.stop_reason, StopReason::EndTurn);
    assert_eq!(message.source.provider, "test-provider");
}

#[tokio::test]
async fn preserves_a_configured_v1_prefix() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let mut config =
        OpenAiCompletionsConfig::new(format!("{}/v1", server.base_url())).expect("config");
    config.compat = OpenAiCompletionsCompat::default();
    let provider = OpenAiCompletionsProvider::new(config).expect("provider");

    provider
        .stream(request(Context::user_text("hi")))
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(server.requests()[0].path(), "/v1/chat/completions");
}

#[tokio::test]
async fn encodes_a_deepseek_reasoning_request() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let provider = provider_with(&server, OpenAiCompletionsCompat::deepseek());

    let mut request = request(Context::user_text("hi"));
    request.options.reasoning = Some(ReasoningLevel::High);
    request.options.max_output_tokens = Some(1024);
    request.model = Model::new(Api::OpenAiCompletions, "deepseek", "deepseek-reasoner")
        .with_capabilities(ModelCapabilities {
            reasoning: true,
            ..Default::default()
        });
    provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect("result");

    let body = server.requests()[0].json();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["max_tokens"], 1024);
    assert!(body.get("max_completion_tokens").is_none());
}

#[tokio::test]
async fn omits_the_authorization_header_for_an_empty_credential() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let provider = provider_for(&server);

    let mut request = request(Context::user_text("hi"));
    request.credential = Credential::new("");
    provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect("result");

    assert_eq!(server.requests()[0].header("authorization"), None);
}

#[tokio::test]
async fn encodes_images_tools_and_tool_results() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let provider = provider_for(&server);

    let mut context = Context::user_text("look");
    context.tools.push(ToolDefinition::new(
        "add",
        "Add",
        serde_json::json!({"type": "object"}),
    ));
    context.messages.insert(
        0,
        Message::User(UserMessage {
            content: vec![
                InputContent::text("here"),
                InputContent::image("image/png", "AAA"),
            ],
        }),
    );
    context
        .messages
        .push(Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_1".to_string(),
            tool_name: "add".to_string(),
            content: vec![InputContent::text("3")],
            is_error: false,
        }));

    let mut request = request(context);
    request.model = text_model().with_capabilities(ModelCapabilities {
        images: true,
        ..Default::default()
    });
    provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect("result");

    let body = server.requests()[0].json();
    assert_eq!(body["tools"][0]["function"]["name"], "add");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAA"
    );
    let tool = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("tool result");
    assert_eq!(tool["tool_call_id"], "call_1");
    assert_eq!(tool["content"], "3");
}

#[tokio::test]
async fn rejects_reserved_and_malformed_extra_headers() {
    let server = TestServer::start().await;
    let mut config = config_for(&server);
    config.extra_headers = vec![("Authorization".to_string(), "secret".to_string())];
    match OpenAiCompletionsProvider::new(config) {
        Err(AiError::InvalidRequest(message)) => assert!(message.contains("authorization")),
        other => panic!("unexpected result: {other:?}"),
    }

    let mut config = config_for(&server);
    config.extra_headers = vec![("x-custom".to_string(), "bad\r\nvalue".to_string())];
    match OpenAiCompletionsProvider::new(config) {
        Err(AiError::InvalidRequest(message)) => assert!(message.contains("x-custom")),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn accepts_a_custom_header_without_leaking_it_in_debug_output() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let mut config = config_for(&server);
    config.extra_headers = vec![("x-api-key".to_string(), "super-secret-value".to_string())];
    let provider = OpenAiCompletionsProvider::new(config.clone()).expect("provider");

    assert!(!format!("{config:?}").contains("super-secret-value"));
    assert!(!format!("{provider:?}").contains("super-secret-value"));

    provider
        .stream(request(Context::user_text("hi")))
        .expect("stream")
        .result()
        .await
        .expect("result");
    assert_eq!(
        server.requests()[0].header("x-api-key"),
        Some("super-secret-value")
    );
}

#[tokio::test]
async fn rejects_base_urls_that_cannot_address_the_endpoint() {
    for base_url in [
        "ftp://api.example.com",
        "https://user:secret@api.example.com/v1",
        "https://api.example.com/v1/chat/completions",
        "/relative/path",
    ] {
        match OpenAiCompletionsConfig::new(base_url) {
            Err(AiError::InvalidBaseUrl(message)) => {
                assert!(!message.contains("secret"));
                assert!(!message.contains("api.example.com"));
            }
            other => panic!("unexpected result for {base_url}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn reports_an_unsupported_reasoning_request_before_sending() {
    let server = TestServer::start().await;
    let provider = provider_for(&server);
    let mut request = request(Context::user_text("hi"));
    request.options.reasoning = Some(ReasoningLevel::Low);

    match provider.stream(request) {
        Err(AiError::Unsupported(message)) => assert!(message.contains("reasoning")),
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn oversized_event_capacity_returns_an_error_before_http_work() {
    let server = TestServer::start().await;
    let provider = provider_for(&server);
    let mut request = request(Context::user_text("hi"));
    request.options.event_buffer = std::num::NonZeroUsize::new(usize::MAX).unwrap();
    assert!(matches!(
        provider.stream(request),
        Err(AiError::InvalidRequest(_))
    ));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn requires_a_tokio_runtime() {
    let server_stub = OpenAiCompletionsConfig::new("http://127.0.0.1:1").expect("config");
    let provider = OpenAiCompletionsProvider::new(server_stub).expect("provider");
    match provider.stream(request(Context::user_text("hi"))) {
        Err(AiError::NoRuntime) => {}
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn uses_the_configured_token_cap_field() {
    let server = TestServer::start().await;
    server.enqueue(text_reply("ok"));
    let mut compat = OpenAiCompletionsCompat::default();
    compat.max_tokens_field = MaxTokensField::MaxTokens;
    compat.reasoning = Some(ReasoningEncoding::ReasoningEffort);
    let provider = provider_with(&server, compat);

    let mut request = request(Context::user_text("hi"));
    request.options.max_output_tokens = Some(64);
    request.options.reasoning = Some(ReasoningLevel::Medium);
    provider
        .stream(request)
        .expect("stream")
        .result()
        .await
        .expect("result");

    let body = server.requests()[0].json();
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["reasoning_effort"], "medium");
    assert!(body.get("max_completion_tokens").is_none());
}
