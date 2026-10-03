//! Serialization contract of the portable protocol types.
//!
//! These tests run with and without the `openai-completions` feature: they exercise only
//! `mira_ai` protocol types, never a transport. All fixtures are local, offline values.

use std::fmt::Debug;

use mira_ai::{
    Api, AssistantContent, AssistantMessage, AssistantSource, ImageInput, InputContent, Message,
    ReasoningLevel, StopReason, TextBlock, ThinkingBlock, ToolCall, ToolResultMessage, Usage,
    UserMessage,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).expect("value serializes");
    let restored: T = serde_json::from_str(&json).expect("value deserializes");
    assert_eq!(&restored, value, "round trip changed the value via {json}");
}

fn assistant_source() -> AssistantSource {
    AssistantSource {
        api: Api::OpenAiCompletions,
        provider: "example-provider".to_string(),
        model: "example-model".to_string(),
        response_model: Some("example-model-2026-01-01".to_string()),
        response_id: Some("resp_42".to_string()),
    }
}

fn assistant_message() -> AssistantMessage {
    AssistantMessage {
        source: assistant_source(),
        content: vec![
            AssistantContent::Text(TextBlock::new("visible answer")),
            AssistantContent::Thinking(ThinkingBlock {
                thinking: "private reasoning".to_string(),
                signature: Some("sig-abc".to_string()),
                redacted: true,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "lookup".to_string(),
                arguments_raw: "{\"q\":\"mira\"}".to_string(),
                arguments: Some(serde_json::json!({ "q": "mira" })),
                signature: Some("call-sig".to_string()),
            }),
        ],
        stop_reason: StopReason::ToolUse,
        raw_stop_reason: Some("tool_calls".to_string()),
        usage: Some(Usage {
            input_tokens: 11,
            output_tokens: 22,
            cached_input_tokens: Some(3),
            reasoning_tokens: Some(4),
        }),
        error_message: None,
    }
}

#[test]
fn wire_shape_of_an_assistant_message_is_stable() {
    let json = serde_json::to_string(&Message::Assistant(assistant_message()))
        .expect("assistant message serializes");

    assert_eq!(
        json,
        concat!(
            r#"{"type":"assistant","source":{"api":"openai-completions","provider":"example-provider","#,
            r#""model":"example-model","response_model":"example-model-2026-01-01","#,
            r#""response_id":"resp_42"},"content":["#,
            r#"{"type":"text","text":"visible answer"},"#,
            r#"{"type":"thinking","thinking":"private reasoning","signature":"sig-abc","redacted":true},"#,
            r#"{"type":"tool_call","id":"call_1","name":"lookup","#,
            r#""arguments_raw":"{\"q\":\"mira\"}","arguments":{"q":"mira"},"signature":"call-sig"}],"#,
            r#""stop_reason":"tool_use","raw_stop_reason":"tool_calls","#,
            r#""usage":{"input_tokens":11,"output_tokens":22,"cached_input_tokens":3,"reasoning_tokens":4},"#,
            r#""error_message":null}"#,
        ),
        "the serialized assistant message shape is part of the stored format"
    );
}

#[test]
fn message_variants_round_trip() {
    let user = Message::User(UserMessage {
        content: vec![
            InputContent::text("what is in this image?"),
            InputContent::image("image/png", "aGVsbG8="),
        ],
    });
    let assistant = Message::Assistant(assistant_message());
    let tool_result = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "lookup".to_string(),
        content: vec![
            InputContent::text("result text"),
            InputContent::image("image/jpeg", "d29ybGQ="),
        ],
        is_error: true,
    });

    for message in [user, assistant, tool_result] {
        round_trip(&message);
    }
}

#[test]
fn failed_assistant_message_round_trips_with_its_reason() {
    let message = Message::Assistant(AssistantMessage {
        source: AssistantSource {
            api: Api::OpenAiCompletions,
            provider: "example-provider".to_string(),
            model: "example-model".to_string(),
            response_model: None,
            response_id: None,
        },
        content: Vec::new(),
        stop_reason: StopReason::Failed,
        raw_stop_reason: Some("content_filter".to_string()),
        usage: None,
        error_message: Some("the provider did not produce usable output".to_string()),
    });

    round_trip(&message);
}

#[test]
fn tool_call_round_trips_an_unparsed_argument_and_a_missing_signature() {
    // A turn truncated at the output token limit keeps `arguments_raw` and has no parsed
    // arguments; a provider without thought signatures reports none.
    let call = ToolCall {
        id: "call_2".to_string(),
        name: "lookup".to_string(),
        arguments_raw: "{\"q\":\"mir".to_string(),
        arguments: None,
        signature: None,
    };
    round_trip(&call);

    let json = serde_json::to_string(&call).expect("tool call serializes");
    assert!(
        json.contains(r#""arguments":null"#) && json.contains(r#""signature":null"#),
        "absent optional tool-call fields are written as null: {json}"
    );
}

#[test]
fn usage_round_trips_with_and_without_optional_counts() {
    round_trip(&Usage {
        input_tokens: 1,
        output_tokens: 2,
        cached_input_tokens: Some(3),
        reasoning_tokens: Some(4),
    });
    round_trip(&Usage {
        input_tokens: u64::MAX,
        output_tokens: 0,
        cached_input_tokens: None,
        reasoning_tokens: None,
    });
}

#[test]
fn stop_reasons_and_reasoning_levels_round_trip() {
    for reason in [
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::ToolUse,
        StopReason::Failed,
    ] {
        round_trip(&reason);
    }
    for level in [
        ReasoningLevel::Minimal,
        ReasoningLevel::Low,
        ReasoningLevel::Medium,
        ReasoningLevel::High,
    ] {
        round_trip(&level);
    }
}

#[test]
fn enum_wire_values_match_the_documented_identifiers() {
    // The serialized `Api` is the same stable identifier the transport and configuration use.
    assert_eq!(
        serde_json::to_string(&Api::OpenAiCompletions).expect("api serializes"),
        format!("\"{}\"", Api::OpenAiCompletions.as_str())
    );
    assert_eq!(
        serde_json::to_string(&StopReason::EndTurn).expect("stop reason serializes"),
        "\"end_turn\""
    );
    assert_eq!(
        serde_json::to_string(&ReasoningLevel::High).expect("reasoning level serializes"),
        format!("\"{}\"", ReasoningLevel::High.as_str())
    );
}

#[test]
fn input_content_wire_shape_is_tagged() {
    assert_eq!(
        serde_json::to_string(&InputContent::text("hi")).expect("text part serializes"),
        r#"{"type":"text","text":"hi"}"#
    );
    assert_eq!(
        serde_json::to_string(&InputContent::image("image/png", "aGVsbG8="))
            .expect("image part serializes"),
        r#"{"type":"image","media_type":"image/png","base64_data":"aGVsbG8="}"#
    );
}

#[test]
fn unknown_extra_fields_are_tolerated_on_read() {
    // A newer writer may add fields. An older reader must still load the turn instead of
    // rejecting the file.
    let json = r#"{"type":"assistant","source":{"api":"openai-completions","provider":"p","model":"m","response_model":null,"response_id":null,"future_provider_field":1},"content":[{"type":"text","text":"hi","future_block_field":true}],"stop_reason":"end_turn","raw_stop_reason":null,"usage":null,"error_message":null,"future_message_field":{"nested":[]}}"#;

    let message: Message = serde_json::from_str(json).expect("unknown fields are ignored");
    let Message::Assistant(assistant) = message else {
        panic!("expected an assistant message");
    };
    assert_eq!(
        assistant.content,
        vec![AssistantContent::Text(TextBlock::new("hi"))]
    );
    assert_eq!(assistant.stop_reason, StopReason::EndTurn);
}

#[test]
fn absent_optional_fields_load_as_none() {
    // Minimal payloads load: every optional protocol field may be missing in older data.
    let assistant: Message = serde_json::from_str(
        r#"{"type":"assistant","source":{"api":"openai-completions","provider":"p","model":"m"},"content":[],"stop_reason":"end_turn"}"#,
    )
    .expect("absent optional fields load");
    let Message::Assistant(assistant) = assistant else {
        panic!("expected an assistant message");
    };
    assert_eq!(assistant.source.response_model, None);
    assert_eq!(assistant.source.response_id, None);
    assert_eq!(assistant.raw_stop_reason, None);
    assert_eq!(assistant.usage, None);
    assert_eq!(assistant.error_message, None);

    let tool_call: ToolCall = serde_json::from_str(
        r#"{"id":"call_1","name":"lookup","arguments_raw":"{\"q\":\"mira\"}"}"#,
    )
    .expect("absent optional tool-call fields load");
    assert_eq!(tool_call.arguments, None);
    assert_eq!(tool_call.signature, None);

    let thinking: ThinkingBlock =
        serde_json::from_str(r#"{"thinking":"reasoning","redacted":false}"#)
            .expect("absent signature loads");
    assert_eq!(thinking.signature, None);

    let usage: Usage = serde_json::from_str(r#"{"input_tokens":1,"output_tokens":2}"#)
        .expect("absent optional usage counts load");
    assert_eq!(usage.cached_input_tokens, None);
    assert_eq!(usage.reasoning_tokens, None);
}

#[test]
fn a_rejected_payload_never_becomes_a_silent_zero_value() {
    // Missing required fields are an error, not a default: usage counts are never fabricated
    // and a message without a role tag is not a message.
    assert!(serde_json::from_str::<Usage>(r#"{"input_tokens":1}"#).is_err());
    assert!(serde_json::from_str::<Message>(r#"{"text":"hi"}"#).is_err());
    assert!(serde_json::from_str::<Message>(r#"{"type":"system","content":[]}"#).is_err());
}

#[test]
fn an_image_part_keeps_its_payload_bytes() {
    let payload = "iVBORw0KGgoAAAANSUhEUg==".to_string();
    let part = InputContent::Image(ImageInput::new("image/png", payload.clone()));
    let json = serde_json::to_string(&part).expect("image part serializes");
    let restored: InputContent = serde_json::from_str(&json).expect("image part deserializes");
    assert_eq!(restored, part);
    assert!(json.contains(&payload));
}
