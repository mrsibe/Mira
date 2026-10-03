//! Test support: temporary directories and small journal fixtures.
//!
//! Deliberately dependency-free: a test only needs a private directory that is removed again,
//! which `std::env::temp_dir` plus a process-unique name provides.
//!
//! Each test target compiles this module separately, so helpers a given target does not use
//! are allowed.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mira_ai::{
    AssistantContent, AssistantMessage, AssistantSource, Message, TextBlock, UserMessage,
};
use mira_session::SessionId;

/// A journal header line used by fixtures that write a journal by hand.
pub const HEADER: &str = r#"{"type":"session","version":1,"id":"raw","timestamp":1}"#;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A temporary directory that removes itself when the test ends.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a fresh directory for one test.
    pub fn new(label: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mira-session-test-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("the test directory is creatable");
        Self { path }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path of the journal file for `id`.
    pub fn journal(&self, id: &SessionId) -> PathBuf {
        self.path.join(format!("{id}.jsonl"))
    }

    /// Path of the lock file for `id`.
    pub fn lock(&self, id: &SessionId) -> PathBuf {
        self.path.join(format!("{id}.jsonl.lock"))
    }

    /// The journal as text.
    pub fn journal_text(&self, id: &SessionId) -> String {
        fs::read_to_string(self.journal(id)).expect("the journal is readable")
    }

    /// Replace the journal's bytes.
    pub fn write_journal(&self, id: &SessionId, bytes: &[u8]) {
        fs::write(self.journal(id), bytes).expect("the journal is writable");
    }

    /// Append raw bytes to the journal, bypassing the crate.
    pub fn append_journal(&self, id: &SessionId, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(self.journal(id))
            .expect("the journal is openable for append");
        file.write_all(bytes).expect("the journal is writable");
    }

    /// Names of the entries in the directory, sorted.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.path)
            .expect("the test directory is readable")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Names that contain `needle`, sorted.
    pub fn names_containing(&self, needle: &str) -> Vec<String> {
        self.names()
            .into_iter()
            .filter(|name| name.contains(needle))
            .collect()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A user turn carrying `text`.
pub fn user_message(text: &str) -> Message {
    Message::User(UserMessage::text(text))
}

/// An assistant turn with one text block, from `provider`/`model`.
pub fn assistant_message(provider: &str, model: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        source: AssistantSource {
            api: mira_ai::Api::OpenAiCompletions,
            provider: provider.to_string(),
            model: model.to_string(),
            response_model: None,
            response_id: None,
        },
        content: vec![AssistantContent::Text(TextBlock::new(text))],
        stop_reason: mira_ai::StopReason::EndTurn,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    })
}

/// Text of a user or assistant message, for assertions.
pub fn message_text(message: &Message) -> String {
    match message {
        Message::User(user) => user
            .content
            .iter()
            .filter_map(|part| match part {
                mira_ai::InputContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect(),
        Message::Assistant(assistant) => assistant.text(),
        Message::ToolResult(result) => result
            .content
            .iter()
            .filter_map(|part| match part {
                mira_ai::InputContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}
