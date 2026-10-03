//! Append-only JSONL session journal with an injectable storage seam for Mira.
//!
//! `mira-session` sits next to `mira-agent` in the Mira library stack
//! (`mira-runtime` → `mira-agent` → `mira-ai`, see
//! [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)) and owns one thing: the durable log
//! of a conversation. The log is a file of one JSON object per line — a versioned header, then
//! `id`/`parent_id`-linked entries — and the model context is a projection of the active
//! branch of that graph.
//!
//! The crate knows nothing about Tauri, SQLite, the keyring, environment variables, provider
//! adapters or application directories. It never reads configuration, never contacts a
//! network, and never picks a storage location for you: a caller injects a
//! [`SessionStore`] and each journal is addressed by a validated [`SessionId`].
//!
//! ```txt
//! Session::create(store, header)   new journal, fails when the id is taken
//! Session::open(store, id)         reload a journal and take its single-writer lock
//! Session::append_*()              one entry per call, durable before it returns
//! Session::active_path()           leaf -> root through parent_id
//! Session::active_messages()       model context of that path, context edits applied
//! ```
//!
//! # Scope
//!
//! Shipped in P0:
//!
//! - The entry model ([`SessionHeader`], [`Entry`], [`EntryKind`]) with lossless storage of
//!   `mira_ai::Message` turns, model and thinking-level changes, usage reports, compaction
//!   summaries, context edits and labels.
//! - A [`SessionStore`] seam with a JSONL file backend ([`JsonlFileStore`]) and an in-memory
//!   backend ([`MemoryStore`]) for tests.
//! - Create, open (with a recovery notice), append, rewrite, the active path, and the message
//!   projection over it.
//! - Corruption handling: a torn tail is repaired or quarantined, a corrupt middle line and an
//!   unsupported header version are typed errors, and the `parent_id` graph is validated.
//!
//! # Compatibility
//!
//! Unknown fields are tolerated on read. Unknown entry kinds and unknown **top-level** entry
//! fields survive a rewrite; unknown fields **nested inside a known payload** (a
//! [`mira_ai::Message`], its provenance, a content block, `Usage`) do **not**, and neither do
//! unknown **header** fields. A payload-shape change therefore requires a [`SESSION_VERSION`]
//! bump rather than an additive field. The version is validated immediately after the header is
//! decoded, before entries are interpreted and before any repair or rewrite, and a reader never
//! rewrites a journal whose version it does not understand.
//!
//! Out of scope in P0, and deliberately not stubbed:
//!
//! - **Compaction projection.** A [`EntryKind::Compaction`] entry is accepted, stored and
//!   preserved through a rewrite, but the projection ignores it: it does not yet shorten the
//!   context. Compaction lands in a later phase.
//! - **Branch switching.** The active branch is the entry most recently appended; there is no
//!   API to select another leaf or fork a branch.
//! - **Provider adapters.** No model I/O, no retries, no transcript transforms.
//! - **Desktop integration.** No SQLite index, no migration of existing rows, no application
//!   paths or Tauri commands.
//! - **Structured context edits.** A replacement is text, projected as a user turn.
//!
//! # Privacy
//!
//! A journal is private user data: it holds prompts, model output, tool arguments and provider
//! signatures. This crate never logs anything, never writes message content into an error, and
//! never echoes a journal line. Corruption is reported as a physical line number, and a
//! fragment that cannot be parsed is preserved in a quarantine file rather than printed.
//!
//! Credentials are outside the journal's type surface: `mira_ai::Credential` implements
//! neither `Serialize` nor `Deserialize` and has a redacting `Debug`, so a credential value
//! cannot be stored or logged directly. That is a type-level exclusion only:
//! `Credential::expose_secret` still returns the secret, so a caller must not copy it into a
//! message text, an error or a log line.
//!
//! A complete offline journal — create, append, reload, project — is in
//! `examples/offline_journal.rs` (`cargo run -p mira-session --example offline_journal`).

#![deny(missing_docs)]

mod entry;
mod error;
mod file_store;
mod ids;
mod memory_store;
mod session;
mod store;

use std::time::{SystemTime, UNIX_EPOCH};

pub use entry::{Entry, EntryKind, SessionHeader};
pub use error::SessionError;
pub use file_store::{migrate_journal, JsonlFileStore, LockOwner};
pub use ids::{EntryId, SessionId, MAX_SESSION_ID_LEN};
pub use memory_store::MemoryStore;
pub use session::Session;
pub use store::{OpenedJournal, RecoveryNotice, SessionStore};

/// Journal format version this crate reads and writes.
///
/// The header carries it, and [`migrate_journal`] is the documented, idempotent place where a
/// future version is upgraded on load.
pub const SESSION_VERSION: u32 = 1;

/// Largest encoded size of one entry line, in bytes.
///
/// The bound limits **one entry**, not the whole journal: a reader reads the entire file into
/// memory with `fs::read` before checking any individual line, so a journal with many entries
/// still allocates as a whole and there is no total-journal or header bound in P0. The bound is
/// generous enough for inline base64 image parts: 8 MiB of encoded entry is roughly a 6 MiB
/// image payload on top of the text around it. An entry above the bound is refused with
/// [`SessionError::EntryTooLarge`].
pub const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;

/// The whole `mira-ai` protocol is reachable through this crate.
pub use mira_ai;
pub use mira_ai::{Message, ReasoningLevel, Usage};

/// Unix timestamp in seconds, or 0 when the system clock is before the epoch.
pub(crate) fn now_unix_seconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Unix timestamp in nanoseconds, used to make generated names unique.
pub(crate) fn now_unix_nanos() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos(),
        Err(_) => 0,
    }
}
