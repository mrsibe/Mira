//! In-memory [`SessionStore`] for tests and for callers that keep no file.
//!
//! This backend holds no lock and touches no filesystem: it is single-process by construction.
//! It enforces the same encoded entry bound as the file backend so a test cannot pass here and
//! fail there.

use std::collections::HashMap;
use std::fmt;

use crate::entry::{Entry, SessionHeader};
use crate::error::SessionError;
use crate::ids::SessionId;
use crate::store::{OpenedJournal, SessionStore};
use crate::{MAX_ENTRY_BYTES, SESSION_VERSION};

/// One journal held in memory.
#[derive(Clone, PartialEq)]
struct MemoryJournal {
    header: SessionHeader,
    entries: Vec<Entry>,
}

/// Redacting [`fmt::Debug`]: the header and the entry count, never the entries.
impl fmt::Debug for MemoryJournal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryJournal")
            .field("header", &self.header)
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// A [`SessionStore`] that keeps journals in memory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    journals: HashMap<SessionId, MemoryJournal>,
    current: Option<SessionId>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The ids currently held, in no particular order.
    pub fn ids(&self) -> impl Iterator<Item = &SessionId> {
        self.journals.keys()
    }

    fn current_mut(&mut self) -> Result<&mut MemoryJournal, SessionError> {
        let id = self.current.clone().ok_or(SessionError::NotOpen)?;
        let journal = self.journals.get_mut(&id).ok_or(SessionError::NotOpen)?;
        Ok(journal)
    }
}

impl SessionStore for MemoryStore {
    fn create(&mut self, header: &SessionHeader) -> Result<(), SessionError> {
        if header.version != SESSION_VERSION {
            return Err(SessionError::UnsupportedVersion {
                found: header.version,
                supported: SESSION_VERSION,
            });
        }
        if self.journals.contains_key(&header.id) {
            return Err(SessionError::AlreadyExists);
        }
        self.journals.insert(
            header.id.clone(),
            MemoryJournal {
                header: header.clone(),
                entries: Vec::new(),
            },
        );
        self.current = Some(header.id.clone());
        Ok(())
    }

    fn open(&mut self, id: &SessionId) -> Result<OpenedJournal, SessionError> {
        let journal = self.journals.get(id).ok_or(SessionError::NotOpen)?;
        if journal.header.version != SESSION_VERSION {
            return Err(SessionError::UnsupportedVersion {
                found: journal.header.version,
                supported: SESSION_VERSION,
            });
        }
        self.current = Some(id.clone());
        Ok(OpenedJournal {
            header: journal.header.clone(),
            entries: journal.entries.clone(),
            notice: None,
        })
    }

    fn append(&mut self, entry: &Entry) -> Result<(), SessionError> {
        let bytes = entry.encode()?.len();
        if bytes > MAX_ENTRY_BYTES {
            return Err(SessionError::EntryTooLarge { bytes });
        }
        self.current_mut()?.entries.push(entry.clone());
        Ok(())
    }

    fn rewrite(&mut self, entries: &[Entry]) -> Result<(), SessionError> {
        for entry in entries {
            let bytes = entry.encode()?.len();
            if bytes > MAX_ENTRY_BYTES {
                return Err(SessionError::EntryTooLarge { bytes });
            }
        }
        self.current_mut()?.entries = entries.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rejects_a_journal_whose_header_version_is_unsupported() {
        // `create` refuses an unsupported version, so reach the stored state directly: the
        // reader must apply the same version contract it does for a file journal.
        let id = SessionId::new("memory-version").expect("a valid session id");
        let mut header = SessionHeader::new(id.clone());
        header.version = 2;
        let mut store = MemoryStore::new();
        store.journals.insert(
            id.clone(),
            MemoryJournal {
                header,
                entries: Vec::new(),
            },
        );
        let error = store
            .open(&id)
            .expect_err("an unsupported version is refused");
        assert!(
            matches!(
                error,
                SessionError::UnsupportedVersion {
                    found: 2,
                    supported: SESSION_VERSION
                }
            ),
            "{error:?}"
        );
    }
}
