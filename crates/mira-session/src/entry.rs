//! Journal records: the versioned header and the append-only entries.
//!
//! # Wire shape
//!
//! One JSON object per line, `snake_case` field names, no trailing whitespace. The first line
//! is the header:
//!
//! ```json
//! {"type":"session","version":1,"id":"s-1","timestamp":1759478400,"cwd":"/work","parent_session":null}
//! ```
//!
//! Every later line is an entry: a flattened base (`type`, `id`, `parent_id`, `timestamp`)
//! followed by the fields of its kind, for example
//!
//! ```json
//! {"type":"message","id":"e-1","timestamp":1759478401,"message":{"type":"user","content":[{"type":"text","text":"hello"}]}}
//! ```
//!
//! `parent_id` is absent on a root entry and present on a child; `cwd` and `parent_session`
//! are absent when they are not set. Absent optional fields and unknown extra fields are both
//! accepted on read, so an extension inside the same format version still loads here. An entry
//! kind this version does not know is kept as [`EntryKind::Unrecognized`] holding the original
//! JSON object, so a rewrite preserves it instead of dropping it. A journal whose header names
//! a different format version is rejected rather than interpreted.
//!
//! # What a rewrite preserves
//!
//! Unknown fields are tolerated on read. Unknown entry kinds and unknown **top-level** entry
//! fields survive a rewrite, because [`EntryKind::Unrecognized`] keeps the whole object and
//! [`Entry::extra`] keeps the extra fields. Unknown fields **nested inside a known payload** —
//! a `mira_ai::Message`, its provenance, a content block, `Usage` — do **not** survive a
//! rewrite, and neither do unknown **header** fields: the payload is decoded into its known
//! shape and re-encoded, and the header has no extra-field map. That is exactly why a change
//! to any payload shape requires a new `SESSION_VERSION` rather than an additive field.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::de::{self, DeserializeOwned};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::SessionError;
use crate::ids::{EntryId, SessionId};
use crate::SESSION_VERSION;

/// Top-level fields of every entry, in wire order.
const BASE_FIELDS: [&str; 4] = ["type", "id", "parent_id", "timestamp"];

/// Header of one session journal.
///
/// A header always carries `"type":"session"`, so it has no public literal constructor; build
/// one with [`SessionHeader::new`] (or [`SessionHeader::at`] for a deterministic fixture).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHeader {
    /// Wire discriminator. Always `"session"`; private so it cannot be wrong.
    #[serde(rename = "type")]
    kind: HeaderKind,
    /// Journal format version. [`SESSION_VERSION`] for a journal this crate writes.
    pub version: u32,
    /// Identifier of this journal.
    pub id: SessionId,
    /// Unix timestamp in seconds when the journal was created.
    pub timestamp: i64,
    /// Working directory the session was started in, when the caller recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Session this journal continues, when the caller forked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<SessionId>,
}

/// Wire discriminator of the header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HeaderKind {
    Session,
}

impl SessionHeader {
    /// A header for `id` at the current time, in the current format version.
    pub fn new(id: SessionId) -> Self {
        Self::at(id, crate::now_unix_seconds())
    }

    /// A header for `id` with an explicit timestamp, for fixtures and tests.
    pub fn at(id: SessionId, timestamp: i64) -> Self {
        Self {
            kind: HeaderKind::Session,
            version: SESSION_VERSION,
            id,
            timestamp,
            cwd: None,
            parent_session: None,
        }
    }

    /// Record the working directory the session was started in.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Record the session this journal continues.
    pub fn with_parent_session(mut self, parent: SessionId) -> Self {
        self.parent_session = Some(parent);
        self
    }
}

/// One append-only record of a session journal.
#[derive(Clone, PartialEq)]
pub struct Entry {
    /// Identifier, unique inside this journal.
    pub id: EntryId,
    /// Parent entry on this branch, or `None` for a root entry.
    pub parent_id: Option<EntryId>,
    /// Unix timestamp in seconds when the entry was appended.
    pub timestamp: i64,
    /// What the entry records.
    pub kind: EntryKind,
    /// Top-level fields of this entry that this version does not know.
    ///
    /// They are preserved so that a rewrite does not silently drop data written by a newer
    /// version. Always empty for [`EntryKind::Unrecognized`], which keeps the whole original
    /// object instead.
    pub extra: Map<String, Value>,
}

impl Entry {
    /// An entry with no unknown extra fields.
    pub fn new(id: EntryId, parent_id: Option<EntryId>, timestamp: i64, kind: EntryKind) -> Self {
        Self {
            id,
            parent_id,
            timestamp,
            kind,
            extra: Map::new(),
        }
    }

    /// Encode this entry as one JSONL line without its line terminator.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, SessionError> {
        serde_json::to_vec(self).map_err(|_| SessionError::InvalidEntry)
    }
}

/// What one journal entry records.
///
/// Serialized with internal tagging (`"type"`). Unknown kinds read as
/// [`EntryKind::Unrecognized`] and are written back verbatim.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum EntryKind {
    /// One conversation turn, stored losslessly.
    Message {
        /// The stored turn.
        message: mira_ai::Message,
    },
    /// The model every following turn is produced with.
    ModelChange {
        /// Registry-level provider identifier, for example `"deepseek"`.
        provider: String,
        /// Model identifier, for example `"deepseek-reasoner"`.
        model: String,
    },
    /// The reasoning effort requested for following turns.
    ThinkingLevelChange {
        /// Requested reasoning effort.
        level: mira_ai::ReasoningLevel,
    },
    /// Token usage the provider reported for one scope.
    Usage {
        /// Free-form label for what this report covers, for example `"turn"`.
        kind: String,
        /// Provider the report came from.
        provider: String,
        /// Model the report came from.
        model: String,
        /// Reported counts, exactly as returned by the provider.
        usage: mira_ai::Usage,
    },
    /// A summary that replaces older turns.
    ///
    /// Stored and preserved, but **not applied** by the P0 projection: compaction is a later
    /// phase. See the crate documentation.
    Compaction {
        /// Summary text.
        summary: String,
        /// First entry of the history the summary keeps.
        first_kept_entry_id: EntryId,
        /// Reported size of the summarized history before compaction.
        tokens_before: u64,
    },
    /// An edit applied to an earlier turn when the model context is projected.
    ContextEdit {
        /// Entry this edit targets.
        target_id: EntryId,
        /// `None` omits the target from the projection; `Some(text)` replaces it with a
        /// text-only user turn. Structured replacements are out of P0 scope.
        replacement: Option<String>,
    },
    /// A label attached to an earlier entry.
    Label {
        /// Entry this label targets.
        target_id: EntryId,
        /// Label text, or `None` to clear an earlier label on the same target.
        label: Option<String>,
    },
    /// An entry kind written by another version of this crate.
    ///
    /// The value is the whole entry object as it was read, so rewriting the journal preserves
    /// the entry — its unknown kind, its fields and its base fields — instead of dropping it.
    /// The object's fields are preserved; key order is normalized.
    Unrecognized(Value),
}

impl EntryKind {
    /// The `type` tag this kind is written with, or `None` for [`EntryKind::Unrecognized`].
    fn tag(&self) -> Option<&'static str> {
        Some(match self {
            Self::Message { .. } => "message",
            Self::ModelChange { .. } => "model_change",
            Self::ThinkingLevelChange { .. } => "thinking_level_change",
            Self::Usage { .. } => "usage",
            Self::Compaction { .. } => "compaction",
            Self::ContextEdit { .. } => "context_edit",
            Self::Label { .. } => "label",
            Self::Unrecognized(_) => return None,
        })
    }

    /// Payload field names this kind owns, or `None` for [`EntryKind::Unrecognized`].
    fn payload_keys(&self) -> Option<&'static [&'static str]> {
        Some(match self {
            Self::Message { .. } => &["message"],
            Self::ModelChange { .. } => &["provider", "model"],
            Self::ThinkingLevelChange { .. } => &["level"],
            Self::Usage { .. } => &["kind", "provider", "model", "usage"],
            Self::Compaction { .. } => &["summary", "first_kept_entry_id", "tokens_before"],
            Self::ContextEdit { .. } => &["target_id", "replacement"],
            Self::Label { .. } => &["target_id", "label"],
            Self::Unrecognized(_) => return None,
        })
    }

    /// Read the kind payload out of a whole entry object.
    fn from_object(object: &Map<String, Value>) -> Result<Self, String> {
        let tag = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "the session entry has no string `type` field".to_string())?;
        Ok(match tag {
            "message" => Self::Message {
                message: required(object, "message")?,
            },
            "model_change" => Self::ModelChange {
                provider: required(object, "provider")?,
                model: required(object, "model")?,
            },
            "thinking_level_change" => Self::ThinkingLevelChange {
                level: required(object, "level")?,
            },
            "usage" => Self::Usage {
                kind: required(object, "kind")?,
                provider: required(object, "provider")?,
                model: required(object, "model")?,
                usage: required(object, "usage")?,
            },
            "compaction" => Self::Compaction {
                summary: required(object, "summary")?,
                first_kept_entry_id: required(object, "first_kept_entry_id")?,
                tokens_before: required(object, "tokens_before")?,
            },
            "context_edit" => Self::ContextEdit {
                target_id: required(object, "target_id")?,
                replacement: optional(object, "replacement")?,
            },
            "label" => Self::Label {
                target_id: required(object, "target_id")?,
                label: optional(object, "label")?,
            },
            _ => Self::Unrecognized(Value::Object(object.clone())),
        })
    }

    /// Number of payload fields this kind writes.
    fn payload_len(&self) -> usize {
        match self {
            Self::Message { .. } => 1,
            Self::ModelChange { .. } => 2,
            Self::ThinkingLevelChange { .. } => 1,
            Self::Usage { .. } => 4,
            Self::Compaction { .. } => 3,
            Self::ContextEdit { .. } => 2,
            Self::Label { .. } => 2,
            Self::Unrecognized(_) => 0,
        }
    }

    /// Whether `key` is a field this kind owns, base fields included.
    fn owns(&self, key: &str) -> bool {
        BASE_FIELDS.contains(&key) || self.payload_keys().is_some_and(|keys| keys.contains(&key))
    }

    /// Stream this kind's payload fields into an entry map, in declaration order.
    fn serialize_payload<S: SerializeMap>(&self, map: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Message { message } => field(map, "message", message),
            Self::ModelChange { provider, model } => {
                field(map, "provider", provider)?;
                field(map, "model", model)
            }
            Self::ThinkingLevelChange { level } => field(map, "level", level),
            Self::Usage {
                kind,
                provider,
                model,
                usage,
            } => {
                field(map, "kind", kind)?;
                field(map, "provider", provider)?;
                field(map, "model", model)?;
                field(map, "usage", usage)
            }
            Self::Compaction {
                summary,
                first_kept_entry_id,
                tokens_before,
            } => {
                field(map, "summary", summary)?;
                field(map, "first_kept_entry_id", first_kept_entry_id)?;
                field(map, "tokens_before", tokens_before)
            }
            Self::ContextEdit {
                target_id,
                replacement,
            } => {
                field(map, "target_id", target_id)?;
                field(map, "replacement", replacement)
            }
            Self::Label { target_id, label } => {
                field(map, "target_id", target_id)?;
                field(map, "label", label)
            }
            Self::Unrecognized(_) => Ok(()),
        }
    }
}

/// Write one map entry.
fn field<S: SerializeMap, T: Serialize>(map: &mut S, key: &str, value: &T) -> Result<(), S::Error> {
    map.serialize_entry(key, value)
}

/// Decode a required field. The failure names the field, never its value.
fn required<T: DeserializeOwned>(object: &Map<String, Value>, field: &str) -> Result<T, String> {
    let value = object
        .get(field)
        .ok_or_else(|| format!("the session entry is missing the `{field}` field"))?;
    serde_json::from_value(value.clone())
        .map_err(|_| format!("the session entry field `{field}` is malformed"))
}

/// Decode an optional field. `null` and an absent field both mean "not set".
fn optional<T: DeserializeOwned>(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<T>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| format!("the session entry field `{field}` is malformed")),
    }
}

/// Redacting [`fmt::Debug`].
///
/// An entry holds private user data (a message, its reasoning, tool arguments, a summary, a
/// label), so `Debug` reports the structural fields — id, parent, timestamp, kind name — and
/// never a payload value. Without this, `format!("{entry:?}")` would print message content.
impl fmt::Debug for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Entry")
            .field("id", &self.id)
            .field("parent_id", &self.parent_id)
            .field("timestamp", &self.timestamp)
            .field("kind", &self.kind)
            .field("extra_fields", &self.extra.len())
            .finish()
    }
}

/// Redacting [`fmt::Debug`] for the entry kind.
///
/// Only the kind is reported; message text, thinking, tool arguments, signatures, summaries,
/// replacements, labels and unrecognized payloads are never printed. [`Entry::extra`] and
/// [`EntryKind::Unrecognized`] are reported as counts through their container's `Debug`.
impl fmt::Debug for EntryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message { .. } => formatter.write_str("EntryKind::Message(<redacted>)"),
            Self::ModelChange { .. } => formatter.write_str("EntryKind::ModelChange(<redacted>)"),
            Self::ThinkingLevelChange { level } => {
                write!(formatter, "EntryKind::ThinkingLevelChange({level:?})")
            }
            Self::Usage { .. } => formatter.write_str("EntryKind::Usage(<redacted>)"),
            Self::Compaction { tokens_before, .. } => write!(
                formatter,
                "EntryKind::Compaction{{ tokens_before: {tokens_before}, <redacted> }}"
            ),
            Self::ContextEdit { .. } => formatter.write_str("EntryKind::ContextEdit(<redacted>)"),
            Self::Label { .. } => formatter.write_str("EntryKind::Label(<redacted>)"),
            Self::Unrecognized(value) => {
                write!(
                    formatter,
                    "EntryKind::Unrecognized(<redacted: {} fields>)",
                    value.as_object().map_or(0, Map::len)
                )
            }
        }
    }
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // An entry kind this version does not know is written back as the same JSON object, so
        // a rewrite preserves it. Key order is normalized; the object's fields are not.
        if let EntryKind::Unrecognized(value) = &self.kind {
            return value.serialize(serializer);
        }
        let tag = self.kind.tag().expect("a recognized kind always has a tag");
        let fields = self.extra.keys().filter(|key| !self.kind.owns(key)).count();
        let mut map = serializer.serialize_map(Some(
            3 + usize::from(self.parent_id.is_some()) + self.kind.payload_len() + fields,
        ))?;
        map.serialize_entry("type", tag)?;
        map.serialize_entry("id", &self.id)?;
        if let Some(parent_id) = &self.parent_id {
            map.serialize_entry("parent_id", parent_id)?;
        }
        map.serialize_entry("timestamp", &self.timestamp)?;
        self.kind.serialize_payload(&mut map)?;
        for (key, value) in &self.extra {
            // Unknown fields never overwrite the known ones.
            if !self.kind.owns(key) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(&value).map_err(de::Error::custom)
    }
}

impl Entry {
    /// Build an entry from one whole JSON object, keeping unknown fields.
    fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "a session entry must be a JSON object".to_string())?;
        let id = required(object, "id")?;
        let parent_id = optional(object, "parent_id")?;
        let timestamp = required(object, "timestamp")?;
        let kind = EntryKind::from_object(object)?;
        let extra = match kind.payload_keys() {
            // An unrecognized kind already holds the whole object.
            None => Map::new(),
            Some(keys) => object
                .iter()
                .filter(|(field, _)| {
                    let field = field.as_str();
                    !BASE_FIELDS.contains(&field) && !keys.contains(&field)
                })
                .map(|(field, value)| (field.clone(), value.clone()))
                .collect(),
        };
        Ok(Self {
            id,
            parent_id,
            timestamp,
            kind,
            extra,
        })
    }
}

/// Validate the entry graph: unique ids, existing parents, and no parent cycles.
pub(crate) fn validate_entries(entries: &[Entry]) -> Result<(), SessionError> {
    let mut index: HashMap<&EntryId, &Entry> = HashMap::with_capacity(entries.len());
    for entry in entries {
        if index.insert(&entry.id, entry).is_some() {
            return Err(SessionError::DuplicateEntryId);
        }
    }
    for entry in entries {
        let mut current = entry;
        let mut visited: HashSet<&EntryId> = HashSet::new();
        loop {
            if !visited.insert(&current.id) {
                return Err(SessionError::ParentCycle);
            }
            match &current.parent_id {
                None => break,
                Some(parent_id) => {
                    current = index.get(parent_id).ok_or(SessionError::DanglingParent)?;
                }
            }
        }
    }
    Ok(())
}
