//! Wire types for the OpenAI-compatible Chat Completions protocol.

use serde::{Deserialize, Serialize};

use crate::types::{StopReason, Usage};

/// Request body sent to the endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct ChatCompletionRequestBody<'a> {
    pub(crate) model: &'a str,
    pub(crate) messages: Vec<serde_json::Value>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stream_options: Option<IncludeUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking: Option<DeepSeekThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tools: Vec<serde_json::Value>,
}

/// `stream_options` request field.
#[derive(Debug, Serialize)]
pub(crate) struct IncludeUsage {
    pub(crate) include_usage: bool,
}

/// DeepSeek reasoning switch, sent as `thinking: { "type": "enabled" }`.
#[derive(Debug, Serialize)]
pub(crate) struct DeepSeekThinking {
    #[serde(rename = "type")]
    pub(crate) thinking_type: &'static str,
}

/// One streamed response chunk. Unknown fields are ignored.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Chunk {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<RawUsage>,
    /// Some providers stream an error object instead of a chunk.
    #[serde(default)]
    pub(crate) error: Option<serde_json::Value>,
}

/// One completion choice. Only the first choice of a chunk is used.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub(crate) delta: Option<Delta>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

/// Incremental content of one choice.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) reasoning_content: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    #[serde(default)]
    pub(crate) reasoning_text: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Option<Vec<ToolCallDelta>>,
}

/// One tool call fragment. Fragments of the same call are matched by `index`, then by `id`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToolCallDelta {
    #[serde(default)]
    pub(crate) index: Option<u32>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) function: Option<FunctionDelta>,
}

/// Incremental function name and arguments of a tool call.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct FunctionDelta {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) arguments: Option<String>,
}

/// Reported token usage. Providers report these fields inconsistently.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) completion_tokens: Option<u64>,
    /// DeepSeek and Kimi report cache hits as a top-level field.
    #[serde(default)]
    pub(crate) prompt_cache_hit_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(default)]
    pub(crate) completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct PromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CompletionTokensDetails {
    #[serde(default)]
    pub(crate) reasoning_tokens: Option<u64>,
}

impl RawUsage {
    /// Convert reported usage, or `None` when the provider did not report token counts.
    ///
    /// A usage object without both `prompt_tokens` and `completion_tokens` is ignored rather
    /// than filled in with zeroes.
    pub(crate) fn to_usage(&self) -> Option<Usage> {
        let input_tokens = self.prompt_tokens?;
        let output_tokens = self.completion_tokens?;
        let cached_input_tokens = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .or(self.prompt_cache_hit_tokens);
        let reasoning_tokens = self
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens);
        Some(Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
        })
    }
}

/// How a provider finish reason maps onto the portable protocol.
pub(crate) struct FinishOutcome {
    pub(crate) stop_reason: StopReason,
    pub(crate) error_message: Option<String>,
}

/// Map one provider `finish_reason`.
///
/// The human-readable description is a constant: the provider's own value is kept only as
/// bounded data on the message, never as an error description.
pub(crate) fn finish_outcome(raw: &str) -> FinishOutcome {
    let stop_reason = match raw {
        "stop" | "end" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        _ => StopReason::Failed,
    };
    let error_message = matches!(stop_reason, StopReason::Failed)
        .then(|| "the provider stopped the response without usable output".to_string());
    FinishOutcome {
        stop_reason,
        error_message,
    }
}

/// Bound provider-controlled text kept as data on a message.
pub(crate) fn bounded_reason(raw: &str) -> String {
    raw.chars()
        .take(32)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_finish_reasons() {
        assert_eq!(finish_outcome("stop").stop_reason, StopReason::EndTurn);
        assert_eq!(finish_outcome("end").stop_reason, StopReason::EndTurn);
        assert_eq!(finish_outcome("length").stop_reason, StopReason::MaxTokens);
        assert_eq!(
            finish_outcome("tool_calls").stop_reason,
            StopReason::ToolUse
        );
        assert_eq!(
            finish_outcome("function_call").stop_reason,
            StopReason::ToolUse
        );
    }

    #[test]
    fn fails_unknown_and_filtered_finish_reasons_with_a_constant_description() {
        const DESCRIPTION: &str = "the provider stopped the response without usable output";
        for raw in [
            "content_filter",
            "insufficient_system_resource",
            "PRIVATE-PROMPT-TEXT",
        ] {
            let outcome = finish_outcome(raw);
            assert_eq!(outcome.stop_reason, StopReason::Failed);
            assert_eq!(outcome.error_message.as_deref(), Some(DESCRIPTION));
        }
        // Known stop reasons carry no error description at all.
        assert_eq!(finish_outcome("stop").error_message, None);
        assert_eq!(finish_outcome("length").error_message, None);
        assert_eq!(finish_outcome("tool_calls").error_message, None);
    }

    #[test]
    fn bounds_provider_controlled_data() {
        assert_eq!(bounded_reason("stop"), "stop");
        assert_eq!(bounded_reason("a\u{7}b"), "a b");
        assert_eq!(bounded_reason(&"x".repeat(100)).len(), 32);
    }

    #[test]
    fn keeps_reported_usage_without_inventing_counts() {
        let complete = RawUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(3),
            }),
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: Some(2),
            }),
            ..RawUsage::default()
        };
        let usage = complete.to_usage().expect("usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.cached_input_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, Some(2));

        let deepseek = RawUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            prompt_cache_hit_tokens: Some(6),
            ..RawUsage::default()
        };
        assert_eq!(
            deepseek.to_usage().expect("usage").cached_input_tokens,
            Some(6)
        );

        assert!(RawUsage {
            prompt_tokens: Some(10),
            ..RawUsage::default()
        }
        .to_usage()
        .is_none());
        assert!(RawUsage::default().to_usage().is_none());
    }
}
