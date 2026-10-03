//! Journal operations and the model-context projection.
//!
//! [`Session`] owns one journal through an injected [`SessionStore`]. It appends entries,
//! resolves the active branch from `parent_id` links, and projects that branch into the
//! message list a model request is built from.

use std::fmt;

use mira_ai::{Message, ReasoningLevel, Usage, UserMessage};

use crate::entry::{validate_entries, Entry, EntryKind, SessionHeader};
use crate::error::SessionError;
use crate::ids::{EntryId, SessionId};
use crate::store::{OpenedJournal, RecoveryNotice, SessionStore};

/// One open journal: its header, its entries and the store it is persisted through.
///
/// Entries are append-only. Context edits and (later) compaction add entries; they never
/// rewrite or delete an earlier message.
pub struct Session<S: SessionStore> {
    store: S,
    header: SessionHeader,
    entries: Vec<Entry>,
    notice: Option<RecoveryNotice>,
}

/// Redacting [`fmt::Debug`]: the header, the entry count and the recovery notice, never the
/// entries. A `Session` prints its stored turns through `entries`, so a derived `Debug` would
/// leak message content.
impl<S: SessionStore> fmt::Debug for Session<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("header", &self.header)
            .field("entries", &self.entries.len())
            .field("notice", &self.notice)
            .finish()
    }
}

impl<S: SessionStore> Session<S> {
    /// Create a new journal for `header`.
    ///
    /// Fails with [`SessionError::AlreadyExists`] when a journal with that id exists; an
    /// existing journal is never overwritten.
    pub fn create(mut store: S, header: SessionHeader) -> Result<Self, SessionError> {
        store.create(&header)?;
        Ok(Self {
            store,
            header,
            entries: Vec::new(),
            notice: None,
        })
    }

    /// Open an existing journal, acquiring its single-writer lock for as long as this session
    /// and its store live.
    ///
    /// The entry graph is validated: a dangling `parent_id`, a parent cycle or a duplicate
    /// entry id is an error rather than a partially usable journal.
    pub fn open(mut store: S, id: &SessionId) -> Result<Self, SessionError> {
        let OpenedJournal {
            header,
            entries,
            notice,
        } = store.open(id)?;
        validate_entries(&entries)?;
        Ok(Self {
            store,
            header,
            entries,
            notice,
        })
    }

    /// The journal's header.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Every stored entry, in append order, including entries that are not on the active path.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// A repair the backend applied while opening this journal, if any.
    pub fn recovery_notice(&self) -> Option<&RecoveryNotice> {
        self.notice.as_ref()
    }

    /// Identifier of the leaf of the active branch: the entry most recently appended.
    pub fn leaf_id(&self) -> Option<&EntryId> {
        self.entries.last().map(|entry| &entry.id)
    }

    /// Append one conversation turn.
    ///
    /// # Credentials
    ///
    /// A journal stores protocol messages, never credentials: `mira_ai::Credential`
    /// implements neither `Serialize` nor `Deserialize`, so there is no way to pass one here.
    /// The function below fails to compile by design:
    ///
    /// ```compile_fail
    /// let credential = mira_ai::Credential::new("sk-not-a-journal-value");
    /// let _ = serde_json::to_string(&credential).unwrap();
    /// ```
    pub fn append_message(&mut self, message: Message) -> Result<EntryId, SessionError> {
        self.append(EntryKind::Message { message })
    }

    /// Append the model following turns are produced with.
    pub fn append_model_change(
        &mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::ModelChange {
            provider: provider.into(),
            model: model.into(),
        })
    }

    /// Append the reasoning effort following turns request.
    pub fn append_thinking_level_change(
        &mut self,
        level: ReasoningLevel,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::ThinkingLevelChange { level })
    }

    /// Append token usage a provider reported for one scope.
    pub fn append_usage(
        &mut self,
        kind: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        usage: Usage,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::Usage {
            kind: kind.into(),
            provider: provider.into(),
            model: model.into(),
            usage,
        })
    }

    /// Append a compaction summary.
    ///
    /// The entry is stored and preserved, but P0 does not apply it when projecting the model
    /// context; see the crate documentation.
    pub fn append_compaction(
        &mut self,
        summary: impl Into<String>,
        first_kept_entry_id: EntryId,
        tokens_before: u64,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::Compaction {
            summary: summary.into(),
            first_kept_entry_id,
            tokens_before,
        })
    }

    /// Append an edit that the projection applies to `target_id`.
    ///
    /// `None` omits the target from the projection. `Some(text)` replaces it with a text-only
    /// user turn; structured replacements are out of P0 scope.
    pub fn append_context_edit(
        &mut self,
        target_id: EntryId,
        replacement: Option<String>,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::ContextEdit {
            target_id,
            replacement,
        })
    }

    /// Append a label for an earlier entry.
    pub fn append_label(
        &mut self,
        target_id: EntryId,
        label: Option<String>,
    ) -> Result<EntryId, SessionError> {
        self.append(EntryKind::Label { target_id, label })
    }

    /// The active branch, from its leaf back to its root.
    ///
    /// The leaf is the entry most recently appended, and the branch follows `parent_id` links.
    /// Entries on the branch that are not conversation turns (a model change, a label) are
    /// still returned here; use [`Session::active_messages`] for the model context.
    pub fn active_path(&self) -> Result<Vec<&Entry>, SessionError> {
        validate_entries(&self.entries)?;
        let mut path = Vec::new();
        let mut current = self.entries.last();
        while let Some(entry) = current {
            path.push(entry);
            current = match &entry.parent_id {
                None => None,
                Some(parent_id) => Some(
                    self.entries
                        .iter()
                        .find(|candidate| &candidate.id == parent_id)
                        .ok_or(SessionError::DanglingParent)?,
                ),
            };
        }
        Ok(path)
    }

    /// The model context for the active branch: turns from root to leaf, with
    /// [`EntryKind::ContextEdit`] entries applied in the order they were appended.
    ///
    /// A `context_edit` whose `replacement` is `None` omits its target; `Some(text)` replaces
    /// the target with a text-only user turn. An edit that targets an entry which is not a
    /// projected turn on this branch (another branch, or a metadata entry) has no effect.
    ///
    /// `compaction`, `usage`, `model_change`, `thinking_level_change` and `label` entries do
    /// not change the projected turns in P0.
    pub fn active_messages(&self) -> Result<Vec<Message>, SessionError> {
        let mut path = self.active_path()?;
        path.reverse();
        let mut projected: Vec<(EntryId, Message)> = Vec::new();
        for entry in path {
            match &entry.kind {
                EntryKind::Message { message } => {
                    projected.push((entry.id.clone(), message.clone()));
                }
                EntryKind::ContextEdit {
                    target_id,
                    replacement,
                } => match replacement {
                    None => projected.retain(|(id, _)| id != target_id),
                    Some(text) => {
                        if let Some(turn) = projected
                            .iter_mut()
                            .find(|(id, _)| id == target_id)
                            .map(|(_, message)| message)
                        {
                            *turn = Message::User(UserMessage::text(text.clone()));
                        }
                    }
                },
                _ => {}
            }
        }
        Ok(projected.into_iter().map(|(_, message)| message).collect())
    }

    fn append(&mut self, kind: EntryKind) -> Result<EntryId, SessionError> {
        let entry = Entry::new(
            EntryId::generate(),
            self.leaf_id().cloned(),
            crate::now_unix_seconds(),
            kind,
        );
        self.store.append(&entry)?;
        let id = entry.id.clone();
        self.entries.push(entry);
        Ok(id)
    }
}
