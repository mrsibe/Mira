//! An offline agent run: a fake provider, a calculator tool, no network.
//!
//! ```bash
//! cargo run -p mira-agent --example offline_agent
//! ```
//!
//! The provider answers the first request with a tool call and the second with text, so the run
//! shows the whole loop: streaming deltas, a tool turn, and the terminal outcome.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use mira_agent::mira_ai::{
    AiError, Api, AssistantContent, AssistantEmitter, AssistantEvent, AssistantMessage,
    AssistantSource, BlockKind, Credential, Model, Provider, StopReason, StreamHandle,
    StreamRequest, TextBlock, ToolCall, ToolDefinition,
};
use mira_agent::{
    Agent, AgentConfig, AgentEvent, CancellationToken, Tool, ToolFuture, ToolRegistry, ToolResult,
};
use serde_json::{json, Value};

/// A calculator with a JSON Schema declaration, executed only with validated arguments.
struct Calculator {
    definition: ToolDefinition,
}

impl Calculator {
    fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                "calculator",
                "Add or subtract two numbers.",
                json!({
                    "type": "object",
                    "properties": {
                        "a": { "type": "number" },
                        "b": { "type": "number" },
                        "op": { "type": "string", "enum": ["add", "sub"] }
                    },
                    "required": ["a", "b"],
                    "additionalProperties": false
                }),
            ),
        }
    }
}

impl Tool for Calculator {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute<'a>(&'a self, arguments: Value, _cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let a = arguments
                .get("a")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let b = arguments
                .get("b")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let value = if arguments.get("op").and_then(Value::as_str) == Some("sub") {
                a - b
            } else {
                a + b
            };
            ToolResult::text(format!("{value}"))
        })
    }
}

/// A provider that answers the first request with a tool call and every later request with text.
struct ScriptedProvider {
    turn: Mutex<usize>,
}

impl Provider for ScriptedProvider {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
        let source = AssistantSource {
            api: request.model.api,
            provider: request.model.provider.clone(),
            model: request.model.id.clone(),
            response_model: None,
            response_id: None,
        };
        let mut turn = self.turn.lock().expect("script lock");
        let message = if *turn == 0 {
            AssistantMessage {
                source,
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call-1".to_string(),
                    name: "calculator".to_string(),
                    arguments_raw: json!({ "a": 20, "b": 22 }).to_string(),
                    arguments: Some(json!({ "a": 20, "b": 22 })),
                    signature: None,
                })],
                stop_reason: StopReason::ToolUse,
                raw_stop_reason: None,
                usage: None,
                error_message: None,
            }
        } else {
            AssistantMessage {
                source,
                content: vec![AssistantContent::Text(TextBlock::new("20 + 22 = 42"))],
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: None,
                error_message: None,
            }
        };
        *turn += 1;

        let (mut producer, handle) = AssistantEmitter::new(NonZeroUsize::new(8).expect("buffer"));
        tokio::spawn(async move {
            for event in block_events(&message) {
                if producer.emit(event).await.is_err() {
                    return;
                }
            }
            producer.finish(Ok(message));
        });
        Ok(handle)
    }
}

/// The immutable events a well-behaved provider publishes for one message.
fn block_events(message: &AssistantMessage) -> Vec<AssistantEvent> {
    let mut events = vec![AssistantEvent::Start {
        source: message.source.clone(),
    }];
    for (index, block) in message.content.iter().enumerate() {
        let (kind, delta) = match block {
            AssistantContent::Text(text) => (BlockKind::Text, text.text.clone()),
            AssistantContent::Thinking(thinking) => {
                (BlockKind::Thinking, thinking.thinking.clone())
            }
            AssistantContent::ToolCall(call) => (BlockKind::ToolCall, call.arguments_raw.clone()),
            _ => continue,
        };
        events.push(AssistantEvent::BlockStart { index, kind });
        if !delta.is_empty() {
            events.push(AssistantEvent::BlockDelta {
                index,
                delta: delta.into(),
            });
        }
        events.push(AssistantEvent::BlockEnd {
            index,
            block: block.clone(),
        });
    }
    events
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = std::sync::Arc::new(ScriptedProvider {
        turn: Mutex::new(0),
    });
    let tools = ToolRegistry::new().with_tool(std::sync::Arc::new(Calculator::new()))?;
    let agent = Agent::new(
        AgentConfig::new(
            provider,
            Model::new(Api::OpenAiCompletions, "offline-provider", "offline-model"),
        )
        .with_system_prompt("You are a calculator.")
        .with_tools(tools),
    );

    let mut run = agent.prompt(
        "What is 20 + 22?",
        Credential::new("offline-placeholder-credential"),
    )?;

    print!("assistant: ");
    while let Some(event) = run.recv().await {
        match event {
            AgentEvent::MessageUpdate {
                update: AssistantEvent::BlockDelta { delta, .. },
                ..
            } => print!("{delta}"),
            AgentEvent::ToolStart { tool, .. } => println!("\ntool start: {tool}"),
            AgentEvent::ToolEnd { tool, is_error, .. } => {
                println!("tool end: {tool} (error: {is_error})")
            }
            _ => {}
        }
    }

    let outcome = run.outcome().await?;
    println!("\nfinished with: {}", outcome.message().text());
    println!("committed messages: {}", agent.snapshot().messages.len());
    Ok(())
}
