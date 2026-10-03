//! The storage seam every consumer depends on.
//!
//! [`SessionStore`] is the only type a caller needs to name to change how a journal is
//! persisted. The crate ships two backends — a JSONL file backend
//! ([`JsonlFileStore`](crate::JsonlFileStore)) and an in-memory backend
//! ([`MemoryStore`](crate::MemoryStore)) — and a caller can supply its own, for example one
//! that writes through an application-owned file handle.
//!
//! A store is bound to one journal at a time: `create` or `open` selects it, and every later
//! `append`/`rewrite` targets that journal until the store is dropped. That is what lets a
//! backend hold whatever exclusive access it needs for the journal's whole lifetime instead of
//! re-acquiring it per write.

use std::fmt;
use std::path::PathBuf;

use crate::entry::{Entry, SessionHeader};
use crate::error::SessionError;
use crate::ids::SessionId;

/// Journal content returned by [`SessionStore::open`].
#[derive(Clone, PartialEq)]
pub struct OpenedJournal {
    /// The journal's header.
    pub header: SessionHeader,
    /// Entries in append order, oldest first.
    pub entries: Vec<Entry>,
    /// A repair the backend applied while opening, when one was needed.
    pub notice: Option<RecoveryNotice>,
}

impl fmt::Debug for OpenedJournal {
    /// Redacting: the header and the entry count are reported, never the entries. An entry
    /// holds private conversation content, so printing it through a derived `Debug` would
    /// leak messages, reasoning and tool arguments.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedJournal")
            .field("header", &self.header)
            .field("entries", &self.entries.len())
            .field("notice", &self.notice)
            .finish()
    }
}

/// A non-fatal repair applied while opening a journal.
///
/// A notice is not an error: the journal was loaded and is usable. It is reported so the
/// application can tell the user what happened to their data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryNotice {
    /// The last line was a complete entry without a trailing newline.
    ///
    /// It was loaded as an entry, and the next append terminates it before writing.
    MissingTrailingNewline,
    /// The last line was an incomplete fragment.
    ///
    /// The fragment was moved to `quarantine_path` and the good prefix was rewritten
    /// atomically, so the fragment is never mixed into the repaired journal.
    QuarantinedTail {
        /// File the incomplete fragment was moved to, next to the journal.
        quarantine_path: PathBuf,
    },
}

/// The storage seam: create, open, append and rewrite one journal.
///
/// Consumers depend on this trait, never on a concrete path. Implementations own whatever
/// durability policy their medium needs; the JSONL backend's policy is documented on
/// [`JsonlFileStore`](crate::JsonlFileStore).
pub trait SessionStore: Send {
    /// Create a journal for `header`, failing with [`SessionError::AlreadyExists`] if one
    /// already exists. Selection remains active until the store is dropped.
    fn create(&mut self, header: &SessionHeader) -> Result<(), SessionError>;

    /// Open the journal named by `id`, acquiring exclusive access for the store's lifetime.
    fn open(&mut self, id: &SessionId) -> Result<OpenedJournal, SessionError>;

    /// Append one entry after every entry already stored, then make it durable.
    fn append(&mut self, entry: &Entry) -> Result<(), SessionError>;

    /// Replace the journal's entries atomically, keeping its header.
    fn rewrite(&mut self, entries: &[Entry]) -> Result<(), SessionError>;
}

impl<T: SessionStore + ?Sized> SessionStore for Box<T> {
    fn create(&mut self, header: &SessionHeader) -> Result<(), SessionError> {
        (**self).create(header)
    }

    fn open(&mut self, id: &SessionId) -> Result<OpenedJournal, SessionError> {
        (**self).open(id)
    }

    fn append(&mut self, entry: &Entry) -> Result<(), SessionError> {
        (**self).append(entry)
    }

    fn rewrite(&mut self, entries: &[Entry]) -> Result<(), SessionError> {
        (**self).rewrite(entries)
    }
}
