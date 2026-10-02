use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub is_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub is_archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub credential_status: Option<String>,
    pub credential_error: Option<String>,
    pub is_default: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSettings {
    pub chat_model_config_id: Option<String>,
    pub background_model_config_id: Option<String>,
    pub background_model_follows_chat: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: i64,
    pub fact: String,
    pub memory_type: Option<String>,
    pub importance: i64,
    pub confidence: f64,
    pub tags: Option<String>,
    pub source_conversation_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
    pub use_count: i64,
    pub is_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPatch {
    pub fact: Option<String>,
    pub memory_type: Option<String>,
    pub importance: Option<i64>,
    pub confidence: Option<f64>,
    pub tags: Option<String>,
    pub is_archived: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageResult {
    pub conversation: Conversation,
    pub user_message: ChatMessage,
    pub assistant_message: Option<ChatMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageStreamDelta {
    pub request_id: String,
    pub conversation_id: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_omits_reasoning_when_absent() {
        let message = ChatMessage {
            id: "m1".to_string(),
            conversation_id: "c1".to_string(),
            role: "assistant".to_string(),
            content: "hello".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            reasoning: None,
        };

        let value = serde_json::to_value(&message).expect("message should serialize");
        assert!(value.get("reasoning").is_none());

        let with_reasoning = ChatMessage {
            reasoning: Some("thought".to_string()),
            ..message
        };
        let value = serde_json::to_value(&with_reasoning).expect("message should serialize");
        assert_eq!(value["reasoning"], "thought");
    }

    #[test]
    fn memory_patch_accepts_partial_updates() {
        let patch: MemoryPatch = serde_json::from_str(r#"{"importance":9,"is_archived":false}"#)
            .expect("patch should deserialize");

        assert_eq!(patch.importance, Some(9));
        assert_eq!(patch.is_archived, Some(false));
        assert!(patch.fact.is_none());
        assert!(patch.tags.is_none());
    }
}
