use crate::runtime::{self, RuntimeConfig, RuntimeError, RuntimeMessage, StreamRequest};
use crate::types::{ChatMessage, Memory, ModelConfig};
use std::sync::atomic::AtomicBool;

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
    let messages = build_messages(
        history,
        project_context,
        injected_memories,
        user_content,
        system_prompt_extra,
    );
    let request = StreamRequest {
        config: RuntimeConfig {
            provider: config.provider.clone(),
            name: config.name.clone(),
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            api_key,
        },
        messages,
        temperature: None,
    };

    match runtime::complete(request, &mut on_delta, &mut on_reasoning, cancel_requested).await {
        Ok(content) => {
            if content.trim().is_empty() {
                Err("模型返回了空内容".to_string())
            } else {
                Ok(content)
            }
        }
        Err(RuntimeError::Cancelled) => Err("__CANCELLED__".to_string()),
        Err(error) => Err(error.message()),
    }
}

fn build_messages(
    history: &[ChatMessage],
    project_context: &[ChatMessage],
    injected_memories: &[Memory],
    user_content: &str,
    system_prompt_extra: &str,
) -> Vec<RuntimeMessage> {
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
    let mut messages = vec![RuntimeMessage {
        role: "system".to_string(),
        content: format!(
            "你是 Mira，一个本地个人 AI 记忆客户端。请自然使用长期记忆和项目上下文，但不要逐条暴露。\n\n长期记忆：\n{}\n\n同项目其他对话上下文：\n{}{}",
            memory_block, project_block, extra
        ),
    }];

    for message in history.iter().rev().take(20).rev() {
        if message.role == "user" || message.role == "assistant" {
            messages.push(RuntimeMessage {
                role: message.role.clone(),
                content: message.content.clone(),
            });
        }
    }
    messages.push(RuntimeMessage {
        role: "user".to_string(),
        content: user_content.to_string(),
    });
    messages
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
    fn compact_collapses_whitespace_and_truncates() {
        assert_eq!(compact("  a   b  ", 10), "a b");
        assert_eq!(compact("abcdef", 3), "abc...");
    }
}
