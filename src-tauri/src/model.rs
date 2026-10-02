use crate::types::{ChatMessage, Memory, ModelConfig};
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::{sleep, timeout};

const MAX_REQUEST_ATTEMPTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<DeepSeekThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeepSeekThinking {
    #[serde(rename = "type")]
    thinking_type: String,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionStreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    #[allow(dead_code)]
    reasoning_content: Option<String>,
}

pub async fn complete_chat_streaming<F, G>(
    config: &ModelConfig,
    history: &[ChatMessage],
    project_context: &[ChatMessage],
    injected_memories: &[Memory],
    user_content: &str,
    system_prompt_extra: &str,
    mut on_delta: F,
    mut on_reasoning: G,
    cancel_requested: &AtomicBool,
) -> Result<String, String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    let api_key = config
        .api_key
        .clone()
        .filter(|value| !value.trim().is_empty() && value != "******")
        .ok_or_else(|| format!("模型配置 {} 缺少 API Key", config.name))?;
    let endpoint = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let messages = build_messages(
        history,
        project_context,
        injected_memories,
        user_content,
        system_prompt_extra,
    );
    let request = build_request(config, messages);
    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("模型客户端初始化失败: {error}"))?;
    let response = send_chat_completion_request(&client, &endpoint, &api_key, &request).await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("模型请求失败: HTTP {status} {body}"));
    }

    let mut full_content = String::new();
    let mut buffer = Vec::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel_requested.load(Ordering::SeqCst) {
            flush_stream_buffer(
                &mut buffer,
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )?;
            drop(stream);
            return Err("__CANCELLED__".to_string());
        }
        let chunk = chunk.map_err(|error| format!("模型流式响应读取失败: {error}"))?;
        append_stream_chunk(
            &mut buffer,
            &chunk,
            &mut full_content,
            &mut on_delta,
            &mut on_reasoning,
        )?;
    }

    flush_stream_buffer(
        &mut buffer,
        &mut full_content,
        &mut on_delta,
        &mut on_reasoning,
    )?;

    if full_content.trim().is_empty() {
        return Err("模型返回了空内容".to_string());
    }
    Ok(full_content)
}

async fn send_chat_completion_request(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    request: &ChatCompletionRequest,
) -> Result<Response, String> {
    let mut last_error = None;
    for attempt in 1..=MAX_REQUEST_ATTEMPTS {
        let send_result = timeout(
            RESPONSE_HEADER_TIMEOUT,
            client
                .post(endpoint)
                .bearer_auth(api_key)
                .json(request)
                .send(),
        )
        .await;

        match send_result {
            Ok(Ok(response)) => {
                if should_retry_status(response.status()) && attempt < MAX_REQUEST_ATTEMPTS {
                    sleep(retry_delay(attempt)).await;
                    continue;
                }
                return Ok(response);
            }
            Ok(Err(error)) if is_retryable_send_error(&error) && attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some(format!("模型请求失败: {error}"));
                sleep(retry_delay(attempt)).await;
            }
            Ok(Err(error)) => return Err(format!("模型请求失败: {error}")),
            Err(_) if attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some("模型请求超时：等待响应头超过 45 秒".to_string());
                sleep(retry_delay(attempt)).await;
            }
            Err(_) => return Err("模型请求超时：等待响应头超过 45 秒".to_string()),
        }
    }
    Err(last_error.unwrap_or_else(|| "模型请求失败".to_string()))
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_retryable_send_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(500 * attempt as u64)
}

fn append_stream_chunk<F, G>(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    full_content: &mut String,
    on_delta: &mut F,
    on_reasoning: &mut G,
) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    buffer.extend_from_slice(chunk);
    while let Some((separator, separator_len)) = find_event_separator(buffer) {
        let raw_event = String::from_utf8(buffer[..separator].to_vec())
            .map_err(|error| format!("模型流式响应不是有效 UTF-8: {error}"))?;
        buffer.drain(..separator + separator_len);
        handle_stream_event(&raw_event, full_content, on_delta, on_reasoning)?;
    }
    Ok(())
}

fn flush_stream_buffer<F, G>(
    buffer: &mut Vec<u8>,
    full_content: &mut String,
    on_delta: &mut F,
    on_reasoning: &mut G,
) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    if buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
        buffer.clear();
        return Ok(());
    }
    let raw_event = String::from_utf8(std::mem::take(buffer))
        .map_err(|error| format!("模型流式响应不是有效 UTF-8: {error}"))?;
    if !raw_event.trim().is_empty() {
        handle_stream_event(&raw_event, full_content, on_delta, on_reasoning)?;
    }
    Ok(())
}

fn find_event_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    lf.or(crlf)
}

fn handle_stream_event<F, G>(
    raw_event: &str,
    full_content: &mut String,
    on_delta: &mut F,
    on_reasoning: &mut G,
) -> Result<(), String>
where
    F: FnMut(&str) -> Result<(), String>,
    G: FnMut(&str) -> Result<(), String>,
{
    for line in raw_event.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data.trim() == "[DONE]" {
                return Ok(());
            }
            if let Ok(chunk) = serde_json::from_str::<ChatCompletionStreamChunk>(data) {
                if let Some(choice) = chunk.choices.first() {
                    if let Some(ref delta) = choice.delta.content {
                        full_content.push_str(delta);
                        on_delta(delta)?;
                    }
                    if let Some(ref reasoning) = choice.delta.reasoning_content {
                        // Don't add reasoning to full_content — keep it separate
                        on_reasoning(reasoning)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn build_messages(
    history: &[ChatMessage],
    project_context: &[ChatMessage],
    injected_memories: &[Memory],
    user_content: &str,
    system_prompt_extra: &str,
) -> Vec<OpenAiMessage> {
    let memory_block = if injected_memories.is_empty() {
        "暂无长期记忆。".to_string()
    } else {
        injected_memories
            .iter()
            .map(|memory| format!("- {}", memory.fact))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let project_block = if project_context.is_empty() {
        "当前会话不在项目中，或项目内暂无其他对话上下文。".to_string()
    } else {
        project_context
            .iter()
            .map(|message| {
                let role = if message.role == "user" {
                    "用户"
                } else {
                    "助手"
                };
                format!("- {role}: {}", compact(&message.content, 180))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let extra = if system_prompt_extra.is_empty() {
        String::new()
    } else {
        format!("\n\n用户自定义指令：\n{}", system_prompt_extra)
    };
    let mut messages = vec![OpenAiMessage {
        role: "system".to_string(),
        content: format!(
            "你是 Mira，一个本地个人 AI 记忆客户端。请自然使用长期记忆和项目上下文，但不要逐条暴露。\n\n长期记忆：\n{}\n\n同项目其他对话上下文：\n{}{}",
            memory_block, project_block, extra
        ),
    }];

    for message in history.iter().rev().take(20).rev() {
        if message.role == "user" || message.role == "assistant" {
            messages.push(OpenAiMessage {
                role: message.role.clone(),
                content: message.content.clone(),
            });
        }
    }
    messages.push(OpenAiMessage {
        role: "user".to_string(),
        content: user_content.to_string(),
    });
    messages
}

fn build_request(config: &ModelConfig, messages: Vec<OpenAiMessage>) -> ChatCompletionRequest {
    if supports_deepseek_reasoning(config) {
        return ChatCompletionRequest {
            model: config.model.clone(),
            messages,
            stream: true,
            thinking: Some(DeepSeekThinking {
                thinking_type: "enabled".to_string(),
            }),
            reasoning_effort: Some("high".to_string()),
        };
    }

    ChatCompletionRequest {
        model: config.model.clone(),
        messages,
        stream: true,
        thinking: None,
        reasoning_effort: None,
    }
}

fn is_deepseek_config(config: &ModelConfig) -> bool {
    config.provider.eq_ignore_ascii_case("deepseek") || config.base_url.contains("api.deepseek.com")
}

fn supports_deepseek_reasoning(config: &ModelConfig) -> bool {
    if !is_deepseek_config(config) {
        return false;
    }
    let model = config.model.to_lowercase();
    model.contains("v4") || model.contains("reasoner")
}

fn compact(content: &str, max_chars: usize) -> String {
    let mut compacted = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if compacted.chars().count() > max_chars {
        compacted = compacted.chars().take(max_chars).collect::<String>();
        compacted.push_str("...");
    }
    compacted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_config(provider: &str, model: &str) -> ModelConfig {
        ModelConfig {
            id: "model-1".to_string(),
            provider: provider.to_string(),
            name: "Mock provider".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            model: model.to_string(),
            api_key: Some("test-key".to_string()),
            credential_status: None,
            credential_error: None,
            is_default: true,
            created_at: None,
            updated_at: None,
        }
    }

    fn chat_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: "message-1".to_string(),
            conversation_id: "conversation-1".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            reasoning: None,
        }
    }

    fn memory(fact: &str) -> Memory {
        Memory {
            id: 1,
            fact: fact.to_string(),
            memory_type: Some("saved".to_string()),
            importance: 8,
            confidence: 1.0,
            tags: None,
            source_conversation_id: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            last_used_at: None,
            use_count: 0,
            is_archived: false,
        }
    }

    #[test]
    fn finds_lf_and_crlf_event_separators() {
        assert_eq!(find_event_separator(b"data: x\n\nrest"), Some((7, 2)));
        assert_eq!(find_event_separator(b"data: x\r\n\r\nrest"), Some((7, 4)));
        assert_eq!(find_event_separator(b"no separator here"), None);
    }

    #[test]
    fn append_stream_chunk_splits_events_and_flushes_leftovers() {
        let mut buffer = Vec::new();
        let mut full_content = String::new();
        let mut deltas = Vec::new();
        let mut reasoning = Vec::new();

        {
            let mut on_delta = |value: &str| {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_reasoning = |value: &str| {
                reasoning.push(value.to_string());
                Ok(())
            };

            append_stream_chunk(
                &mut buffer,
                b"data: {\"choices\":[{\"delta\":{\"content\":\"H",
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )
            .expect("first chunk should buffer");
            assert!(full_content.is_empty());
            assert!(!buffer.is_empty());

            append_stream_chunk(
                &mut buffer,
                b"e\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}]}",
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )
            .expect("split event should parse");
            assert_eq!(full_content, "He");
            assert!(!buffer.is_empty());

            flush_stream_buffer(
                &mut buffer,
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )
            .expect("leftover buffer should flush");
        }

        assert_eq!(full_content, "Hello");
        assert_eq!(deltas, vec!["He".to_string(), "llo".to_string()]);
        assert!(reasoning.is_empty());
        assert!(buffer.is_empty());
    }

    #[test]
    fn reasoning_events_stay_out_of_the_content_buffer() {
        let mut full_content = String::new();
        let mut deltas = Vec::new();
        let mut reasoning = Vec::new();

        {
            let mut on_delta = |value: &str| {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_reasoning = |value: &str| {
                reasoning.push(value.to_string());
                Ok(())
            };

            handle_stream_event(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}",
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )
            .expect("reasoning event should parse");
        }

        assert!(full_content.is_empty());
        assert!(deltas.is_empty());
        assert_eq!(reasoning, vec!["thinking".to_string()]);
    }

    #[test]
    fn malformed_stream_payload_is_ignored() {
        let mut full_content = String::new();
        let mut deltas = Vec::new();
        let mut reasoning = Vec::new();

        {
            let mut on_delta = |value: &str| {
                deltas.push(value.to_string());
                Ok(())
            };
            let mut on_reasoning = |value: &str| {
                reasoning.push(value.to_string());
                Ok(())
            };

            handle_stream_event(
                "data: not-json",
                &mut full_content,
                &mut on_delta,
                &mut on_reasoning,
            )
            .expect("malformed payload should be ignored");
        }

        assert!(full_content.is_empty());
        assert!(deltas.is_empty());
        assert!(reasoning.is_empty());
    }

    #[test]
    fn build_messages_includes_context_and_keeps_last_history_turns() {
        let history = (0..25)
            .map(|index| {
                chat_message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &format!("history-{index}"),
                )
            })
            .collect::<Vec<_>>();
        let project_context = vec![chat_message("assistant", "project context")];
        let memories = vec![memory("likes tea")];

        let messages = build_messages(
            &history,
            &project_context,
            &memories,
            "latest question",
            "be brief",
        );

        assert_eq!(messages[0].role, "system");
        assert!(messages[0].content.contains("likes tea"));
        assert!(messages[0].content.contains("project context"));
        assert!(messages[0].content.contains("be brief"));
        // system message + last 20 history turns + the new user message
        assert_eq!(messages.len(), 22);
        assert_eq!(messages[1].content, "history-5");
        assert_eq!(messages[21].role, "user");
        assert_eq!(messages[21].content, "latest question");
    }

    #[test]
    fn build_messages_marks_missing_memory_and_project_context() {
        let messages = build_messages(&[], &[], &[], "hi", "");

        assert_eq!(messages.len(), 2);
        assert!(messages[0]
            .content
            .contains("\u{6682}\u{65e0}\u{957f}\u{671f}\u{8bb0}\u{5fc6}\u{3002}"));
        assert!(messages[0]
            .content
            .contains("\u{5f53}\u{524d}\u{4f1a}\u{8bdd}\u{4e0d}\u{5728}\u{9879}\u{76ee}\u{4e2d}"));
        assert!(!messages[0]
            .content
            .contains("\u{7528}\u{6237}\u{81ea}\u{5b9a}\u{4e49}\u{6307}\u{4ee4}"));
        assert_eq!(messages[1].content, "hi");
    }

    #[test]
    fn deepseek_reasoner_requests_reasoning_fields() {
        let config = model_config("deepseek", "deepseek-reasoner");
        assert!(supports_deepseek_reasoning(&config));

        let request = build_request(&config, Vec::new());

        assert!(request.stream);
        assert!(request.thinking.is_some());
        assert_eq!(request.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn other_models_omit_reasoning_fields() {
        let config = model_config("openai", "gpt-4o-mini");
        assert!(!supports_deepseek_reasoning(&config));

        let request = build_request(&config, Vec::new());

        assert!(request.thinking.is_none());
        assert!(request.reasoning_effort.is_none());
    }

    #[test]
    fn retries_server_and_rate_limit_statuses_only() {
        assert!(should_retry_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!should_retry_status(StatusCode::BAD_REQUEST));
        assert_eq!(retry_delay(2), Duration::from_millis(1000));
    }

    #[test]
    fn compact_collapses_whitespace_and_truncates() {
        assert_eq!(compact("  a   b  ", 10), "a b");
        assert_eq!(compact("abcdef", 3), "abc...");
    }
}
