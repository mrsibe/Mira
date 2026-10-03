//! Typed failures of the session journal.
//!
//! Every variant carries structure, never content. A journal holds private user data, so an
//! error never includes a message, a prompt, a memory or a raw line: corruption is reported
//! as the physical line number instead, and decoding failures name the structural field that
//! failed rather than echoing its value.

use std::io;

/// Why a session journal operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The session id is empty, too long, or could not be used as a file name.
    #[error("the session id is empty, too long or not usable as a file name")]
    InvalidSessionId,
    /// A journal with this id already exists; `create` never overwrites one.
    #[error("a session journal with this id already exists")]
    AlreadyExists,
    /// Another writer holds the journal's single-writer lock.
    ///
    /// The lock is never stolen automatically. Inspect it with
    /// [`JsonlFileStore::lock_owner`](crate::JsonlFileStore::lock_owner) and release a lock
    /// left behind by a crashed process with
    /// [`JsonlFileStore::break_stale_lock`](crate::JsonlFileStore::break_stale_lock).
    #[error("another writer holds the session journal lock")]
    Locked,
    /// The store has no journal open yet, or its journal was already closed.
    #[error("the session store has no open journal")]
    NotOpen,
    /// The journal file or directory could not be read or written.
    #[error("the session journal could not be read or written")]
    Io(#[from] io::Error),
    /// The journal has no header line, so it is not a session journal.
    #[error("the session journal has no header")]
    EmptyJournal,
    /// The first line is not a valid session header.
    #[error("the session journal header is malformed")]
    MalformedHeader,
    /// The header names a session id other than the one the journal was opened as.
    ///
    /// The id names the journal file, so a header that disagrees with its file name means the
    /// file is not the requested journal. `open` refuses it before any repair or write, so no
    /// other file is created, locked or written.
    #[error("the session journal header names a different session id")]
    MismatchedJournalId,
    /// A journal write failed and may have left the file in an unknown state.
    ///
    /// This open state must be dropped and the journal reopened before another append or
    /// rewrite. The lock stays held until the store is dropped.
    #[error("the session journal must be reopened after a failed write")]
    NeedsReopen,
    /// The header names a format version this reader does not know.
    #[error(
        "unsupported session journal version {found}; this reader supports version {supported}"
    )]
    UnsupportedVersion {
        /// Version found in the journal.
        found: u32,
        /// Version this crate reads and writes ([`SESSION_VERSION`](crate::SESSION_VERSION)).
        supported: u32,
    },
    /// A complete line that is not the final fragment is not a valid entry.
    ///
    /// `line` is the 1-based physical line number, counting the header as line 1. The line's
    /// content is deliberately not part of the error.
    #[error("session journal line {line} is corrupt")]
    CorruptLine {
        /// 1-based physical line number of the corrupt line.
        line: usize,
    },
    /// One encoded entry is larger than [`MAX_ENTRY_BYTES`](crate::MAX_ENTRY_BYTES).
    #[error("a session entry is larger than the encoded size bound")]
    EntryTooLarge {
        /// Encoded size of the entry, in bytes.
        bytes: usize,
    },
    /// A `parent_id` names an entry that this journal does not contain.
    #[error("a session entry references a parent that does not exist")]
    DanglingParent,
    /// Following `parent_id` links loops instead of reaching a root entry.
    #[error("the session entries contain a parent cycle")]
    ParentCycle,
    /// Two entries share an id, so the entry graph is ambiguous.
    #[error("the session entries contain a duplicate entry id")]
    DuplicateEntryId,
    /// An entry could not be encoded, so it was not written.
    #[error("the session entry could not be encoded")]
    InvalidEntry,
}
