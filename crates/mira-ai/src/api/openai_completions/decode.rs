//! Decoding of streamed Chat Completions chunks into protocol events.
//!
//! The decoder accumulates fragmented tool-call arguments and parses them strictly once the
//! response terminates. It never repairs, salvages or partially parses arguments: a call
//! whose argument text is not complete JSON object text keeps `arguments: None`, so a
//! truncated turn can never authorize tool execution.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::api::openai_completions::wire::{
    bounded_reason, finish_outcome, Chunk, Delta, FinishOutcome, ToolCallDelta,
};
use crate::error::{provider_failure_from_value, AiError};
use crate::stream::{AssistantEvent, BlockKind};
use crate::types::{
    AssistantContent, AssistantMessage, AssistantSource, TextBlock, ThinkingBlock, ToolCall, Usage,
};

/// Outcome of decoding one SSE payload.
#[derive(Debug)]
pub(crate) enum PayloadOutcome {
    /// Events to publish for this payload.
    Events(Vec<AssistantEvent>),
    /// The payload was the `[DONE]` sentinel.
    Done,
}

/// Final decoded response: closing block events plus the terminal message.
#[derive(Debug)]
pub(crate) struct Finalized {
    pub(crate) events: Vec<AssistantEvent>,
    pub(crate) message: AssistantMessage,
}

/// Content block as it arrives from the provider.
enum StreamingBlock {
    Text(String),
    Thinking(ThinkingBlock),
    ToolCall(StreamingToolCall),
}

struct StreamingToolCall {
    id: String,
    name: String,
    arguments_raw: String,
    signature: Option<String>,
}

impl StreamingBlock {
    fn into_content(self) -> AssistantContent {
        match self {
            Self::Text(text) => AssistantContent::Text(TextBlock { text }),
            Self::Thinking(block) => AssistantContent::Thinking(block),
            Self::ToolCall(call) => AssistantContent::ToolCall(ToolCall {
                id: call.id,
                name: call.name,
                arguments: strict_arguments(&call.arguments_raw),
                arguments_raw: call.arguments_raw,
                signature: call.signature,
            }),
        }
    }
}

/// Parse accumulated tool arguments strictly: complete JSON object text or nothing.
pub(crate) fn strict_arguments(arguments_raw: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(arguments_raw.trim()).ok()?;
    parsed.is_object().then_some(parsed)
}

/// Decoder for one streamed response.
pub(crate) struct Decoder {
    source: AssistantSource,
    blocks: Vec<StreamingBlock>,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tool_calls: Vec<usize>,
    tool_by_index: HashMap<u32, usize>,
    tool_by_id: HashMap<String, usize>,
    finish: Option<FinishOutcome>,
    raw_finish_reason: Option<String>,
    usage: Option<Usage>,
    max_content_blocks: usize,
}

impl Decoder {
    pub(crate) fn new(source: AssistantSource, max_content_blocks: usize) -> Self {
        Self {
            source,
            blocks: Vec::new(),
            text_index: None,
            thinking_index: None,
            tool_calls: Vec::new(),
            tool_by_index: HashMap::new(),
            tool_by_id: HashMap::new(),
            finish: None,
            raw_finish_reason: None,
            usage: None,
            max_content_blocks,
        }
    }

    /// Decode one SSE payload.
    pub(crate) fn decode(&mut self, payload: &str) -> Result<PayloadOutcome, AiError> {
        if payload.trim() == "[DONE]" {
            return Ok(PayloadOutcome::Done);
        }
        let chunk: Chunk = serde_json::from_str(payload).map_err(|error| {
            AiError::Protocol(format!(
                "provider sent an event payload that is not a valid chunk (line {}, column {})",
                error.line(),
                error.column()
            ))
        })?;
        if let Some(error) = chunk.error {
            return Err(AiError::ProviderStream {
                failure: provider_failure_from_value(&error),
            });
        }
        if let Some(id) = chunk.id {
            if !id.is_empty() && self.source.response_id.is_none() {
                self.source.response_id = Some(id);
            }
        }
        if let Some(model) = chunk.model {
            if !model.is_empty()
                && model != self.source.model
                && self.source.response_model.is_none()
            {
                self.source.response_model = Some(model);
            }
        }
        if let Some(usage) = chunk.usage.as_ref().and_then(|usage| usage.to_usage()) {
            self.usage = Some(usage);
        }

        let mut events = Vec::new();
        let Some(choice) = chunk.choices.first() else {
            return Ok(PayloadOutcome::Events(events));
        };
        if let Some(raw) = choice.finish_reason.as_deref() {
            if self.finish.is_none() {
                self.finish = Some(finish_outcome(raw));
                self.raw_finish_reason = Some(bounded_reason(raw));
            }
        }
        let Some(delta) = choice.delta.as_ref() else {
            return Ok(PayloadOutcome::Events(events));
        };

        if let Some(content) = delta.content.as_deref() {
            if !content.is_empty() {
                let index = self.ensure_text_block(&mut events)?;
                if let StreamingBlock::Text(text) = &mut self.blocks[index] {
                    text.push_str(content);
                }
                events.push(AssistantEvent::BlockDelta {
                    index,
                    delta: Arc::from(content),
                });
            }
        }

        if let Some((field, reasoning)) = reasoning_delta(delta) {
            let index = self.ensure_thinking_block(field, &mut events)?;
            if let StreamingBlock::Thinking(block) = &mut self.blocks[index] {
                block.thinking.push_str(reasoning);
            }
            events.push(AssistantEvent::BlockDelta {
                index,
                delta: Arc::from(reasoning),
            });
        }

        if let Some(tool_calls) = delta.tool_calls.as_ref() {
            for tool_call in tool_calls {
                let index = self.ensure_tool_call_block(tool_call, &mut events)?;
                let arguments = tool_call
                    .function
                    .as_ref()
                    .and_then(|function| function.arguments.as_deref())
                    .unwrap_or_default();
                if arguments.is_empty() {
                    continue;
                }
                if let StreamingBlock::ToolCall(call) = &mut self.blocks[index] {
                    call.arguments_raw.push_str(arguments);
                }
                events.push(AssistantEvent::BlockDelta {
                    index,
                    delta: Arc::from(arguments),
                });
            }
        }

        Ok(PayloadOutcome::Events(events))
    }

    /// Close the response and build the terminal message.
    pub(crate) fn finish(self) -> Result<Finalized, AiError> {
        let finish = self.finish.ok_or(AiError::IncompleteStream)?;
        let mut events = Vec::with_capacity(self.blocks.len());
        let mut content = Vec::with_capacity(self.blocks.len());
        for (index, block) in self.blocks.into_iter().enumerate() {
            let block = block.into_content();
            events.push(AssistantEvent::BlockEnd {
                index,
                block: block.clone(),
            });
            content.push(block);
        }
        let message = AssistantMessage {
            source: self.source,
            content,
            stop_reason: finish.stop_reason,
            raw_stop_reason: self.raw_finish_reason,
            usage: self.usage,
            error_message: finish.error_message,
        };
        Ok(Finalized { events, message })
    }

    fn ensure_text_block(&mut self, events: &mut Vec<AssistantEvent>) -> Result<usize, AiError> {
        if let Some(index) = self.text_index {
            return Ok(index);
        }
        let index = self.push_block(StreamingBlock::Text(String::new()))?;
        self.text_index = Some(index);
        events.push(AssistantEvent::BlockStart {
            index,
            kind: BlockKind::Text,
        });
        Ok(index)
    }

    fn ensure_thinking_block(
        &mut self,
        field: &'static str,
        events: &mut Vec<AssistantEvent>,
    ) -> Result<usize, AiError> {
        if let Some(index) = self.thinking_index {
            return Ok(index);
        }
        let index = self.push_block(StreamingBlock::Thinking(ThinkingBlock {
            thinking: String::new(),
            // The field name is opaque replay metadata: it records where the reasoning came
            // from so a later request can be encoded against the same provider.
            signature: Some(field.to_string()),
            redacted: false,
        }))?;
        self.thinking_index = Some(index);
        events.push(AssistantEvent::BlockStart {
            index,
            kind: BlockKind::Thinking,
        });
        Ok(index)
    }

    fn ensure_tool_call_block(
        &mut self,
        fragment: &ToolCallDelta,
        events: &mut Vec<AssistantEvent>,
    ) -> Result<usize, AiError> {
        let ambiguous = fragment.index.is_none() && fragment.id.is_none();
        let mut resolved = fragment
            .index
            .and_then(|index| self.tool_by_index.get(&index).copied())
            .or_else(|| {
                fragment
                    .id
                    .as_ref()
                    .and_then(|id| self.tool_by_id.get(id).copied())
            });
        if resolved.is_none() && ambiguous {
            match self.tool_calls.as_slice() {
                [] => {}
                [only] => resolved = Some(*only),
                _ => {
                    return Err(AiError::Protocol(
                        "provider sent a tool call fragment without an index or id while several calls are open"
                            .to_string(),
                    ))
                }
            }
        }

        let index = match resolved {
            Some(index) => {
                if let StreamingBlock::ToolCall(call) = &mut self.blocks[index] {
                    if call.id.is_empty() {
                        if let Some(id) = fragment.id.as_ref() {
                            call.id = id.clone();
                        }
                    }
                    if call.name.is_empty() {
                        if let Some(name) = fragment
                            .function
                            .as_ref()
                            .and_then(|function| function.name.as_deref())
                        {
                            call.name = name.to_string();
                        }
                    }
                }
                if let Some(field) = fragment.index {
                    self.tool_by_index.entry(field).or_insert(index);
                }
                if let Some(id) = fragment.id.as_ref() {
                    self.tool_by_id.entry(id.clone()).or_insert(index);
                }
                index
            }
            None => {
                let index = self.push_block(StreamingBlock::ToolCall(StreamingToolCall {
                    id: fragment.id.clone().unwrap_or_default(),
                    name: fragment
                        .function
                        .as_ref()
                        .and_then(|function| function.name.clone())
                        .unwrap_or_default(),
                    arguments_raw: String::new(),
                    signature: None,
                }))?;
                self.tool_calls.push(index);
                if let Some(field) = fragment.index {
                    self.tool_by_index.insert(field, index);
                }
                if let Some(id) = fragment.id.as_ref() {
                    self.tool_by_id.insert(id.clone(), index);
                }
                events.push(AssistantEvent::BlockStart {
                    index,
                    kind: BlockKind::ToolCall,
                });
                index
            }
        };
        Ok(index)
    }

    fn push_block(&mut self, block: StreamingBlock) -> Result<usize, AiError> {
        if self.blocks.len() >= self.max_content_blocks {
            return Err(AiError::Protocol(format!(
                "provider stream produced more than {} content blocks",
                self.max_content_blocks
            )));
        }
        self.blocks.push(block);
        Ok(self.blocks.len() - 1)
    }
}

/// Pick the reasoning text of one delta: the first non-empty known field wins.
/// Reasoning text of one delta: the first non-empty known field wins, so a provider that
/// reports the same reasoning under two names does not duplicate it.
fn reasoning_delta(delta: &Delta) -> Option<(&'static str, &str)> {
    let candidates = [
        ("reasoning_content", delta.reasoning_content.as_deref()),
        ("reasoning", delta.reasoning.as_deref()),
        ("reasoning_text", delta.reasoning_text.as_deref()),
    ];
    for (field, value) in candidates {
        if let Some(value) = value {
            if !value.is_empty() {
                return Some((field, value));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StopReason;

    fn source() -> AssistantSource {
        AssistantSource {
            api: crate::types::Api::OpenAiCompletions,
            provider: "test".to_string(),
            model: "test-model".to_string(),
            response_model: None,
            response_id: None,
        }
    }

    fn decoder() -> Decoder {
        Decoder::new(source(), 64)
    }

    fn decode(decoder: &mut Decoder, payload: &str) -> Vec<AssistantEvent> {
        match decoder.decode(payload).expect("decode") {
            PayloadOutcome::Events(events) => events,
            PayloadOutcome::Done => Vec::new(),
        }
    }

    #[test]
    fn strict_arguments_accepts_only_complete_objects() {
        assert_eq!(
            strict_arguments("{\"a\":1}"),
            Some(serde_json::json!({"a": 1}))
        );
        assert_eq!(strict_arguments(""), None);
        assert_eq!(strict_arguments("{\"a\":"), None);
        assert_eq!(strict_arguments("[1,2]"), None);
        assert_eq!(strict_arguments("7"), None);
        assert_eq!(strict_arguments("not json"), None);
    }

    #[test]
    fn accumulates_interleaved_tool_calls_and_parses_strictly() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_a","function":{"name":"one","arguments":"{\"a\""}},
                {"index":1,"id":"call_b","function":{"name":"two","arguments":"{\"b\""}}]}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":1,"function":{"arguments":":2}"}},
                {"index":0,"function":{"arguments":":1}"}}]}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let finalized = decoder.finish().expect("finish");
        assert_eq!(finalized.message.stop_reason, StopReason::ToolUse);
        let calls: Vec<_> = finalized.message.tool_calls().collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].arguments, Some(serde_json::json!({"a": 1})));
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].arguments, Some(serde_json::json!({"b": 2})));
        assert_eq!(finalized.events.len(), 2);
    }

    #[test]
    fn truncated_arguments_are_preserved_but_not_executable() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"one","arguments":"{\"a\":1"}}]}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        );
        let finalized = decoder.finish().expect("finish");
        assert_eq!(finalized.message.stop_reason, StopReason::MaxTokens);
        let call = finalized.message.tool_calls().next().expect("call");
        assert_eq!(call.arguments_raw, "{\"a\":1");
        assert_eq!(call.arguments, None);
    }

    #[test]
    fn reuses_a_tool_call_by_id_when_the_index_is_missing() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"one","arguments":"{}"}}]}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"arguments":" "}}]}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let finalized = decoder.finish().expect("finish");
        assert_eq!(finalized.message.tool_calls().count(), 1);
    }

    #[test]
    fn rejects_ambiguous_fragments_without_index_or_id() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_a","function":{"name":"one","arguments":"{}"}},
                {"index":1,"id":"call_b","function":{"name":"two","arguments":"{}"}}]}}]}"#,
        );
        let error = decoder
            .decode(r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{}"}}]}}]}"#)
            .expect_err("ambiguous");
        assert!(matches!(error, AiError::Protocol(_)));
    }

    #[test]
    fn keeps_one_thinking_and_one_text_block() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
        );
        decode(&mut decoder, r#"{"choices":[{"delta":{"content":"say"}}]}"#);
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{"reasoning_content":"ing"}}]}"#,
        );
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        );
        let finalized = decoder.finish().expect("finish");
        assert_eq!(finalized.message.content.len(), 2);
        assert_eq!(
            finalized.message.content[0],
            AssistantContent::Thinking(ThinkingBlock {
                thinking: "thinking".to_string(),
                signature: Some("reasoning_content".to_string()),
                redacted: false,
            })
        );
        assert_eq!(finalized.message.text(), "say");
    }

    #[test]
    fn rejects_a_stream_without_a_finish_reason() {
        let mut decoder = decoder();
        decode(&mut decoder, r#"{"choices":[{"delta":{"content":"hi"}}]}"#);
        assert_eq!(
            decoder.finish().expect_err("missing"),
            AiError::IncompleteStream
        );
    }

    #[test]
    fn surfaces_provider_stream_errors_as_a_category_without_prose() {
        const PROMPT: &str = "PRIVATE-PROMPT-TEXT";
        const KEY: &str = "sk-secret-value-1234567890";
        const CUSTOM: &str = "CUSTOM-HEADER-SECRET";

        let mut error_decoder = decoder();
        let error = error_decoder
            .decode(&format!(
                r#"{{"error":{{"message":"{PROMPT} {KEY} {CUSTOM}","type":"rate_limit_error"}}}}"#
            ))
            .expect_err("provider error");
        assert_eq!(
            error,
            AiError::ProviderStream {
                failure: crate::error::ProviderFailure::RateLimited
            }
        );
        let rendered = format!("{error}");
        for needle in [PROMPT, KEY, CUSTOM] {
            assert!(
                !rendered.contains(needle),
                "'{needle}' leaked into: {rendered}"
            );
        }

        let mut unknown_decoder = decoder();
        let unknown = unknown_decoder
            .decode(r#"{"error":{"message":"boom"}}"#)
            .expect_err("provider error");
        assert_eq!(
            unknown,
            AiError::ProviderStream {
                failure: crate::error::ProviderFailure::StreamError
            }
        );
    }

    #[test]
    fn ignores_trailing_usage_only_chunks() {
        let mut decoder = decoder();
        decode(
            &mut decoder,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        );
        let events = decode(
            &mut decoder,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":3}}"#,
        );
        assert!(events.is_empty());
        let finalized = decoder.finish().expect("finish");
        assert_eq!(finalized.message.usage.expect("usage").input_tokens, 9);
    }

    #[test]
    fn bounds_the_number_of_content_blocks() {
        let mut decoder = Decoder::new(source(), 1);
        decode(&mut decoder, r#"{"choices":[{"delta":{"content":"a"}}]}"#);
        let error = decoder
            .decode(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"one"}}]}}]}"#,
            )
            .expect_err("too many blocks");
        assert!(matches!(error, AiError::Protocol(_)));
    }
}
