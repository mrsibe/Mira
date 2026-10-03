//! Encoding of protocol requests into Chat Completions wire bodies.

use serde_json::{json, Value};

use crate::api::openai_completions::wire::{
    ChatCompletionRequestBody, DeepSeekThinking, IncludeUsage,
};
use crate::api::openai_completions::{MaxTokensField, OpenAiCompletionsCompat, ReasoningEncoding};
use crate::context::Context;
use crate::error::AiError;
use crate::provider::{ReasoningLevel, StreamRequest};
use crate::types::{
    Api, AssistantContent, AssistantMessage, InputContent, Message, Model, StopReason,
    ToolResultMessage, UserMessage,
};

use super::OpenAiCompletionsConfig;

const NON_VISION_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";
const TOOL_RESULT_IMAGES_PREFIX: &str = "Attached image(s) from tool result:";
const EMPTY_TOOL_RESULT: &str = "(no tool output)";
const TOOL_RESULT_IMAGE_ONLY: &str = "(see attached image)";

/// Build the JSON body of one streaming Chat Completions request.
pub(crate) fn build_request_body(
    config: &OpenAiCompletionsConfig,
    request: &StreamRequest,
) -> Result<Value, AiError> {
    if request.model.api != Api::OpenAiCompletions {
        return Err(AiError::InvalidRequest(format!(
            "model '{}' is served by {} and not by the openai-completions transport",
            request.model.id, request.model.api
        )));
    }
    let options = &request.options;
    if let Some(temperature) = options.temperature {
        if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
            return Err(AiError::InvalidRequest(
                "temperature must be a finite number between 0 and 2".to_string(),
            ));
        }
    }
    if options.max_output_tokens == Some(0) {
        return Err(AiError::InvalidRequest(
            "max_output_tokens must be greater than zero".to_string(),
        ));
    }

    let compat = &config.compat;
    let (thinking, reasoning_effort) = reasoning_fields(options.reasoning, compat)?;
    let (max_completion_tokens, max_tokens) =
        match (options.max_output_tokens, compat.max_tokens_field) {
            (None, _) => (None, None),
            (Some(tokens), MaxTokensField::MaxCompletionTokens) => (Some(tokens), None),
            (Some(tokens), MaxTokensField::MaxTokens) => (None, Some(tokens)),
        };

    let body = ChatCompletionRequestBody {
        model: &request.model.id,
        messages: encode_messages(&request.model, &request.context, compat),
        stream: true,
        stream_options: compat.supports_usage_in_streaming.then_some(IncludeUsage {
            include_usage: true,
        }),
        temperature: options.temperature,
        max_completion_tokens,
        max_tokens,
        thinking,
        reasoning_effort,
        tools: encode_tools(&request.context.tools),
    };
    serde_json::to_value(&body).map_err(|_| {
        AiError::InvalidRequest("the request body could not be serialized".to_string())
    })
}

/// Encode the requested reasoning level, or fail when the configuration cannot express it.
fn reasoning_fields(
    requested: Option<ReasoningLevel>,
    compat: &OpenAiCompletionsCompat,
) -> Result<(Option<DeepSeekThinking>, Option<&'static str>), AiError> {
    let Some(level) = requested else {
        return Ok((None, None));
    };
    match compat.reasoning {
        Some(ReasoningEncoding::ReasoningEffort) => Ok((None, Some(level.as_str()))),
        Some(ReasoningEncoding::DeepSeekThinking) => Ok((
            Some(DeepSeekThinking {
                thinking_type: "enabled",
            }),
            compat.supports_reasoning_effort.then(|| level.as_str()),
        )),
        None => Err(AiError::Unsupported(format!(
            "reasoning level '{}' cannot be encoded: this provider configuration declares no reasoning encoding",
            level.as_str()
        ))),
    }
}

/// Encode the whole conversation, preserving the caller's order.
fn encode_messages(
    model: &Model,
    context: &Context,
    compat: &OpenAiCompletionsCompat,
) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = context.system_prompt.as_deref() {
        if !system_prompt.is_empty() {
            messages.push(json!({ "role": "system", "content": system_prompt }));
        }
    }
    for message in &context.messages {
        match message {
            Message::User(user) => {
                if let Some(value) = encode_user_message(user, model) {
                    messages.push(value);
                }
            }
            Message::Assistant(assistant) => {
                if let Some(value) = encode_assistant_message(assistant, model, compat) {
                    messages.push(value);
                }
            }
            Message::ToolResult(result) => {
                let (tool_message, image_message) = encode_tool_result(result, model);
                messages.push(tool_message);
                if let Some(image_message) = image_message {
                    messages.push(image_message);
                }
            }
        }
    }
    messages
}

/// A user turn is sent as plain text when it carries no image, so OpenAI-compatible servers
/// that only accept the classic shape keep working.
fn encode_user_message(message: &UserMessage, model: &Model) -> Option<Value> {
    let mut text_parts: Vec<&str> = Vec::new();
    let mut image_parts: Vec<Value> = Vec::new();
    for part in &message.content {
        match part {
            InputContent::Text(text) => {
                if !text.text.is_empty() {
                    text_parts.push(&text.text);
                }
            }
            InputContent::Image(image) => {
                if model.capabilities.images {
                    image_parts.push(image_part(image));
                } else {
                    text_parts.push(NON_VISION_IMAGE_PLACEHOLDER);
                }
            }
        }
    }
    if text_parts.is_empty() && image_parts.is_empty() {
        return None;
    }
    if image_parts.is_empty() {
        return Some(json!({ "role": "user", "content": text_parts.join("\n") }));
    }
    let mut content = Vec::with_capacity(text_parts.len() + image_parts.len());
    let joined = text_parts.join("\n");
    if !joined.is_empty() {
        content.push(json!({ "type": "text", "text": joined }));
    }
    content.extend(image_parts);
    Some(json!({ "role": "user", "content": content }))
}

fn image_part(image: &crate::types::ImageInput) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{};base64,{}", image.media_type, image.base64_data) },
    })
}

/// Assistant messages replay text and tool calls. Thinking is replayed only when the
/// configuration asks for it and the message came from the same model, so hidden reasoning
/// never leaks into another model's context.
fn encode_assistant_message(
    message: &AssistantMessage,
    model: &Model,
    compat: &OpenAiCompletionsCompat,
) -> Option<Value> {
    // A failed turn is incomplete: replaying it can mislead the model and can break providers
    // that require complete tool call and result pairs.
    if message.stop_reason == StopReason::Failed {
        return None;
    }
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let tool_calls: Vec<Value> = message
        .tool_calls()
        .map(|call| {
            json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    // Replay the provider's exact argument text: it is never rewritten.
                    "arguments": if call.arguments_raw.is_empty() { "{}" } else { call.arguments_raw.as_str() },
                },
            })
        })
        .collect();
    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }

    let mut encoded = serde_json::Map::new();
    encoded.insert("role".to_string(), Value::String("assistant".to_string()));
    encoded.insert(
        "content".to_string(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !tool_calls.is_empty() {
        encoded.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if compat.replay_reasoning_content && is_same_model(message, model) {
        let reasoning: String = message
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Thinking(thinking) if !thinking.thinking.is_empty() => {
                    Some(thinking.thinking.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !reasoning.is_empty() {
            encoded.insert("reasoning_content".to_string(), Value::String(reasoning));
        }
    }
    Some(Value::Object(encoded))
}

/// Whether the message was produced by the model the request addresses.
fn is_same_model(message: &AssistantMessage, model: &Model) -> bool {
    message.source.api == model.api
        && message.source.provider == model.provider
        && message.source.model == model.id
}

/// Tool results are text-only on the wire; images are attached in a following user turn,
/// which is the shape OpenAI-compatible endpoints accept.
fn encode_tool_result(message: &ToolResultMessage, model: &Model) -> (Value, Option<Value>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut image_parts: Vec<Value> = Vec::new();
    for part in &message.content {
        match part {
            InputContent::Text(text) => {
                if !text.text.is_empty() {
                    text_parts.push(text.text.clone());
                }
            }
            InputContent::Image(image) => {
                if model.capabilities.images {
                    image_parts.push(image_part(image));
                } else {
                    text_parts.push(NON_VISION_TOOL_IMAGE_PLACEHOLDER.to_string());
                }
            }
        }
    }
    let text = if text_parts.is_empty() {
        if image_parts.is_empty() {
            EMPTY_TOOL_RESULT.to_string()
        } else {
            TOOL_RESULT_IMAGE_ONLY.to_string()
        }
    } else {
        text_parts.join("\n")
    };
    let tool_message = json!({
        "role": "tool",
        "tool_call_id": message.tool_call_id,
        "content": text,
    });
    let image_message = if image_parts.is_empty() {
        None
    } else {
        let mut content = Vec::with_capacity(image_parts.len() + 1);
        content.push(json!({ "type": "text", "text": TOOL_RESULT_IMAGES_PREFIX }));
        content.extend(image_parts);
        Some(json!({ "role": "user", "content": content }))
    };
    (tool_message, image_message)
}

fn encode_tools(tools: &[crate::context::ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ToolDefinition;
    use crate::provider::Credential;
    use crate::types::{AssistantSource, ThinkingBlock, ToolCall};

    fn test_config() -> OpenAiCompletionsConfig {
        OpenAiCompletionsConfig::new("https://api.example.com/v1").expect("config")
    }

    fn test_request(model: Model, context: Context) -> StreamRequest {
        StreamRequest::new(model, context, Credential::new("sk-key"))
    }

    fn test_body(config: &OpenAiCompletionsConfig, request: &StreamRequest) -> Value {
        build_request_body(config, request).expect("body")
    }

    fn assistant(content: Vec<AssistantContent>) -> AssistantMessage {
        assistant_from("test", "test-model", content)
    }

    fn assistant_from(
        provider: &str,
        model: &str,
        content: Vec<AssistantContent>,
    ) -> AssistantMessage {
        AssistantMessage {
            source: AssistantSource {
                api: Api::OpenAiCompletions,
                provider: provider.to_string(),
                model: model.to_string(),
                response_model: None,
                response_id: None,
            },
            content,
            stop_reason: StopReason::EndTurn,
            raw_stop_reason: Some("stop".to_string()),
            usage: None,
            error_message: None,
        }
    }

    #[test]
    fn encodes_a_basic_request() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut context = Context::user_text("hello");
        context.system_prompt = Some("be brief".to_string());
        let body = test_body(&test_config(), &test_request(model, context));

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("thinking").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn rejects_invalid_temperature_and_token_caps() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut request = test_request(model.clone(), Context::user_text("hi"));
        request.options.temperature = Some(3.0);
        assert!(matches!(
            build_request_body(&test_config(), &request),
            Err(AiError::InvalidRequest(_))
        ));
        request.options.temperature = Some(f32::NAN);
        assert!(build_request_body(&test_config(), &request).is_err());

        let mut request = test_request(model, Context::user_text("hi"));
        request.options.max_output_tokens = Some(0);
        assert!(build_request_body(&test_config(), &request).is_err());
    }

    #[test]
    fn encodes_temperature_and_the_configured_token_cap_field() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut request = test_request(model, Context::user_text("hi"));
        request.options.temperature = Some(0.25);
        request.options.max_output_tokens = Some(512);
        let body = test_body(&test_config(), &request);
        assert_eq!(body["temperature"], 0.25);
        assert_eq!(body["max_completion_tokens"], 512);
        assert!(body.get("max_tokens").is_none());

        let mut config = test_config();
        config.compat.max_tokens_field = MaxTokensField::MaxTokens;
        let body = test_body(&config, &request);
        assert_eq!(body["max_tokens"], 512);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn encodes_deepseek_thinking_and_generic_reasoning() {
        let model = Model::new(Api::OpenAiCompletions, "deepseek", "deepseek-reasoner")
            .with_capabilities(crate::types::ModelCapabilities {
                reasoning: true,
                ..Default::default()
            });
        let mut request = test_request(model.clone(), Context::user_text("hi"));
        request.options.reasoning = Some(ReasoningLevel::High);

        let mut config = test_config();
        config.compat = OpenAiCompletionsCompat::deepseek();
        let body = test_body(&config, &request);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");

        let mut generic = test_config();
        generic.compat.reasoning = Some(ReasoningEncoding::ReasoningEffort);
        let body = test_body(&generic, &request);
        assert!(body.get("thinking").is_none());
        assert_eq!(body["reasoning_effort"], "high");

        // No requested reasoning never sends a reasoning parameter.
        let mut plain = request.clone();
        plain.options.reasoning = None;
        let body = test_body(&config, &plain);
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn rejects_reasoning_that_the_configuration_cannot_encode() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut request = test_request(model, Context::user_text("hi"));
        request.options.reasoning = Some(ReasoningLevel::Low);
        match build_request_body(&test_config(), &request) {
            Err(AiError::Unsupported(message)) => assert!(message.contains("reasoning")),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn encodes_images_and_placeholders_for_non_vision_models() {
        let vision = Model::new(Api::OpenAiCompletions, "test", "test-model").with_capabilities(
            crate::types::ModelCapabilities {
                images: true,
                ..Default::default()
            },
        );
        let mut context = Context::new();
        context.messages.push(Message::User(UserMessage {
            content: vec![
                InputContent::text("look"),
                InputContent::image("image/png", "AAA"),
            ],
        }));
        let body = test_body(&test_config(), &test_request(vision, context));
        assert_eq!(body["messages"][0]["content"][0]["text"], "look");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );

        let text_only = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut context = Context::new();
        context.messages.push(Message::User(UserMessage {
            content: vec![InputContent::image("image/png", "AAA")],
        }));
        let body = test_body(&test_config(), &test_request(text_only, context));
        assert_eq!(body["messages"][0]["content"], NON_VISION_IMAGE_PLACEHOLDER);
    }

    #[test]
    fn encodes_tools_and_replayed_tool_calls() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut context = Context::user_text("calculate");
        context.tools.push(ToolDefinition::new(
            "add",
            "Add numbers",
            json!({"type": "object"}),
        ));
        context.messages.push(Message::Assistant(assistant(vec![
            AssistantContent::Text(crate::types::TextBlock::new("sure")),
            AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "add".to_string(),
                arguments_raw: "{\"a\":1,\"b\":2}".to_string(),
                arguments: Some(json!({"a": 1, "b": 2})),
                signature: None,
            }),
        ])));
        context
            .messages
            .push(Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1".to_string(),
                tool_name: "add".to_string(),
                content: vec![InputContent::text("3")],
                is_error: false,
            }));
        let body = test_body(&test_config(), &test_request(model, context));

        assert_eq!(body["tools"][0]["function"]["name"], "add");
        let assistant = &body["messages"][1];
        assert_eq!(assistant["content"], "sure");
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1,\"b\":2}"
        );
        let tool = &body["messages"][2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "call_1");
        assert_eq!(tool["content"], "3");
    }

    #[test]
    fn attaches_tool_result_images_in_a_following_user_turn() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model").with_capabilities(
            crate::types::ModelCapabilities {
                images: true,
                ..Default::default()
            },
        );
        let mut context = Context::new();
        context
            .messages
            .push(Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1".to_string(),
                tool_name: "shot".to_string(),
                content: vec![
                    InputContent::text("done"),
                    InputContent::image("image/png", "AAA"),
                ],
                is_error: false,
            }));
        let body = test_body(&test_config(), &test_request(model, context));
        assert_eq!(body["messages"][0]["content"], "done");
        assert_eq!(
            body["messages"][1]["content"][0]["text"],
            TOOL_RESULT_IMAGES_PREFIX
        );
        assert_eq!(
            body["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAA"
        );
    }

    #[test]
    fn replays_reasoning_only_for_the_same_model_when_configured() {
        let model = Model::new(Api::OpenAiCompletions, "deepseek", "deepseek-reasoner");
        let thinking = AssistantContent::Thinking(ThinkingBlock {
            thinking: "hmm".to_string(),
            signature: Some("reasoning_content".to_string()),
            redacted: false,
        });
        let mut context = Context::new();
        context.messages.push(Message::Assistant(assistant_from(
            "deepseek",
            "deepseek-reasoner",
            vec![
                thinking,
                AssistantContent::Text(crate::types::TextBlock::new("answer")),
            ],
        )));
        context
            .messages
            .push(Message::User(UserMessage::text("next")));

        // Default configuration does not replay reasoning.
        let body = test_body(
            &test_config(),
            &test_request(model.clone(), context.clone()),
        );
        assert!(body["messages"][0].get("reasoning_content").is_none());

        let mut replay = test_config();
        replay.compat.replay_reasoning_content = true;
        let body = test_body(&replay, &test_request(model, context.clone()));
        assert_eq!(body["messages"][0]["reasoning_content"], "hmm");

        let other = Model::new(Api::OpenAiCompletions, "other", "other-model");
        let body = test_body(&replay, &test_request(other, context));
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn skips_empty_and_failed_assistant_messages() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut failed = assistant(Vec::new());
        failed.stop_reason = StopReason::Failed;
        failed
            .content
            .push(AssistantContent::Text(crate::types::TextBlock::new(
                "partial",
            )));

        let mut context = Context::new();
        context.messages.push(Message::Assistant(failed));
        context
            .messages
            .push(Message::User(UserMessage::text("next")));
        let body = test_body(&test_config(), &test_request(model, context));
        assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
        assert_eq!(body["messages"][0]["content"], "next");
    }

    #[test]
    fn skips_assistant_messages_without_content_or_tool_calls() {
        let model = Model::new(Api::OpenAiCompletions, "test", "test-model");
        let mut context = Context::new();
        context
            .messages
            .push(Message::Assistant(assistant(Vec::new())));
        let body = test_body(&test_config(), &test_request(model, context));
        assert!(body["messages"].as_array().expect("messages").is_empty());
    }
}
