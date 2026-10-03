//! Session and entry identifiers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::error::SessionError;

/// Longest accepted session id.
///
/// A session id becomes a file name, so it is bounded and restricted; see
/// [`SessionId::new`].
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Local counter that makes generated identifiers unique inside one process.
static GENERATED: AtomicU64 = AtomicU64::new(0);

fn generate(prefix: &str) -> String {
    let nanos = crate::now_unix_nanos();
    let sequence = GENERATED.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

/// Identifier of one journal.
///
/// The id names the journal's file, so it is validated before use: it must be non-empty, at
/// most [`MAX_SESSION_ID_LEN`] bytes, free of path separators, free of `..`, and free of
/// control characters. An id that passes [`SessionId::new`] can never escape the store
/// directory, so the store never has to sanitize a path at write time.
///
/// `SessionId::generate` is a convenience for callers without an id scheme of their own. It
/// is not a UUID and is not cryptographically random: it is unique per user on one machine,
/// which is all a local file name needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Validate `value` as a session id.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidSessionId`] when the value is empty, longer than
    /// [`MAX_SESSION_ID_LEN`] bytes, contains `/` or `\`, contains `..`, or contains a
    /// control character.
    pub fn new(value: impl Into<String>) -> Result<Self, SessionError> {
        let value = value.into();
        let usable = !value.is_empty()
            && value.len() <= MAX_SESSION_ID_LEN
            && !value.contains('/')
            && !value.contains('\\')
            && !value.contains("..")
            && !value.chars().any(char::is_control);
        if usable {
            Ok(Self(value))
        } else {
            Err(SessionError::InvalidSessionId)
        }
    }

    /// Generate a locally unique session id.
    pub fn generate() -> Self {
        Self(generate("s"))
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validating deserialization.
///
/// The derived `Deserialize` would build a [`SessionId`] directly, so an identifier read from
/// JSON could carry a path separator or `..` and then be interpolated into a file name. This
/// implementation routes the decoded string through [`SessionId::new`] instead, and the error
/// never echoes the offending value.
impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        SessionId::new(value)
            .map_err(|_| de::Error::custom("the session id is not usable as a file name"))
    }
}

/// Identifier of one journal entry, unique inside its journal.
///
/// Entry ids are compared for equality and written to the journal; a caller that persists
/// entries without a store ([`EntryId::new`]) supplies its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(String);

impl EntryId {
    /// Wrap a caller-supplied identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Generate a locally unique entry id.
    pub fn generate() -> Self {
        Self(generate("e"))
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
