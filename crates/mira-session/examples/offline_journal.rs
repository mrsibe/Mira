//! An offline session journal: create, append, reload from disk and project.
//!
//! Run with `cargo run -p mira-session --example offline_journal`. It writes to a fresh
//! temporary directory and removes it again. No provider, no network, no credential.

use std::fs;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use mira_ai::{
    Api, AssistantContent, AssistantMessage, AssistantSource, Message, StopReason, TextBlock,
    ThinkingBlock, ToolCall, ToolResultMessage, Usage, UserMessage,
};
use mira_session::{
    EntryId, JsonlFileStore, RecoveryNotice, Session, SessionError, SessionHeader, SessionId,
};

fn main() -> Result<(), SessionError> {
    let root = temporary_directory();
    let store = JsonlFileStore::new(&root);
    let id = SessionId::new("offline-example")?;

    let mut session = Session::create(store, SessionHeader::new(id.clone()).with_cwd("/work"))?;
    println!("created journal for {}", session.header().id);

    let user = session.append_message(Message::User(UserMessage::text(
        "what is in the attached screenshot?",
    )))?;
    session.append_model_change("example-provider", "example-model")?;
    session.append_thinking_level_change(mira_ai::ReasoningLevel::High)?;

    session.append_message(Message::Assistant(AssistantMessage {
        source: AssistantSource {
            api: Api::OpenAiCompletions,
            provider: "example-provider".to_string(),
            model: "example-model".to_string(),
            response_model: None,
            response_id: Some("resp_1".to_string()),
        },
        content: vec![
            AssistantContent::Thinking(ThinkingBlock {
                thinking: "the user attached an image".to_string(),
                signature: Some("sig-example".to_string()),
                redacted: false,
            }),
            AssistantContent::Text(TextBlock::new("let me look that up")),
            AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "describe_image".to_string(),
                arguments_raw: "{\"detail\":\"high\"}".to_string(),
                arguments: Some(serde_json::json!({ "detail": "high" })),
                signature: None,
            }),
        ],
        stop_reason: StopReason::ToolUse,
        raw_stop_reason: Some("tool_calls".to_string()),
        usage: Some(Usage {
            input_tokens: 120,
            output_tokens: 40,
            cached_input_tokens: Some(64),
            reasoning_tokens: Some(12),
        }),
        error_message: None,
    }))?;

    session.append_message(Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "describe_image".to_string(),
        content: vec![mira_ai::InputContent::text(
            "a terminal window showing a passing test run",
        )],
        is_error: false,
    }))?;

    session.append_usage(
        "turn",
        "example-provider",
        "example-model",
        Usage {
            input_tokens: 120,
            output_tokens: 40,
            cached_input_tokens: Some(64),
            reasoning_tokens: Some(12),
        },
    )?;

    // A context edit rewrites nothing: it adds an entry that the projection applies.
    session.append_context_edit(user.clone(), Some("what is in the screenshot?".to_string()))?;
    session.append_label(user, Some("attachment question".to_string()))?;

    // Compaction is stored and preserved; P0 does not apply it to the projection yet.
    let first_kept: EntryId = session.leaf_id().expect("the journal has entries").clone();
    session.append_compaction("earlier turns summarized", first_kept, 512)?;

    println!(
        "appended {} entries; active branch depth {}",
        session.entries().len(),
        session.active_path()?.len()
    );

    // Drop the session (and with it the journal lock), then reload the same file from disk.
    drop(session);
    let reopened = Session::open(JsonlFileStore::new(&root), &id)?;
    println!(
        "reloaded {} entries, recovery notice: {}",
        reopened.entries().len(),
        match reopened.recovery_notice() {
            None => "none".to_string(),
            Some(RecoveryNotice::MissingTrailingNewline) => "trailing newline repaired".to_string(),
            Some(RecoveryNotice::QuarantinedTail { quarantine_path }) =>
                format!("quarantined tail at {quarantine_path:?}"),
            Some(other) => format!("{other:?}"),
        }
    );

    for (index, message) in reopened.active_messages()?.iter().enumerate() {
        // Print the shape of the projection, never the message content: this example must not
        // demonstrate a diagnostic pattern that logs a journal's private data.
        let (role, parts) = match message {
            Message::User(user) => ("user", user.content.len()),
            Message::Assistant(assistant) => ("assistant", assistant.content.len()),
            Message::ToolResult(result) => ("tool_result", result.content.len()),
            _ => ("other", 0),
        };
        println!("  turn {index}: {role} with {parts} part(s)");
    }

    // The journal is a plain JSONL file: one header line, then one entry per line.
    let line_count = fs::read_to_string(root.join(format!("{id}.jsonl")))
        .expect("the journal is readable")
        .lines()
        .count();
    println!("journal file has {line_count} lines");

    fs::remove_dir_all(&root).expect("the temporary directory is removable");
    Ok(())
}

fn temporary_directory() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let path =
        std::env::temp_dir().join(format!("mira-session-example-{}-{nanos:x}", process::id()));
    fs::create_dir_all(&path).expect("the temporary directory is creatable");
    path
}
