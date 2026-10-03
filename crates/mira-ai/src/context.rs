//! Request context: the system prompt, conversation history and tool declarations.

use serde_json::Value;

use crate::types::Message;

/// Everything a provider needs to produce one assistant turn.
///
/// The system prompt and tool declarations live here instead of in dedicated transcript
/// messages. Dynamic mid-conversation prompt or tool changes are not supported.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// Instructions rendered as the leading system message. `None` and `Some("")` both
    /// produce no system message.
    pub system_prompt: Option<String>,
    /// Conversation history, oldest first.
    pub messages: Vec<Message>,
    /// Tools the model may call, in declaration order.
    pub tools: Vec<ToolDefinition>,
}

impl Context {
    /// An empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// A context with one user message.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            system_prompt: None,
            messages: vec![Message::User(crate::types::UserMessage::text(text))],
            tools: Vec::new(),
        }
    }
}

/// A callable tool declared to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    /// Name the model uses in its tool call.
    pub name: String,
    /// Instructions for the model.
    pub description: String,
    /// JSON Schema object for the arguments, sent verbatim as `parameters`.
    pub parameters: Value,
}

impl ToolDefinition {
    /// Declare a tool with a JSON Schema `parameters` object.
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}
