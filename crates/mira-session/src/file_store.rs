//! The JSONL file backend: one journal per file, one writer per journal.
//!
//! # Durability policy
//!
//! The journal is private user data, so the backend is stricter than a plain buffered writer:
//!
//! - **Single writer.** Opening or creating a journal takes an exclusive lock file next to it.
//!   The lock is held until the store is dropped; a second writer gets
//!   [`SessionError::Locked`](crate::SessionError::Locked). It is never stolen automatically,
//!   and a lock left behind by a crashed process is released only by an explicit call to
//!   [`JsonlFileStore::break_stale_lock`].
//! - **Durable appends.** Every append is followed by `fsync` of the journal file, and a create
//!   or rewrite is followed by `fsync` of the containing directory, so the entry and its
//!   directory entry both survive a crash.
//! - **Atomic rewrite.** A rewrite writes a temporary file in the same directory and renames it
//!   over the journal. A temporary file is never left behind, and one left by a crashed run is
//!   cleaned up only while the lock is held. Cleanup removes only names matching the exact
//!   generated grammar `<id>.jsonl.tmp-<decimal pid>-<hex sequence>`; that prefix is reserved,
//!   so a user file such as `<id>.jsonl.tmp-backup` is left alone.
//! - **Restrictive permissions (unix).** The journal directory is created `0700` and every file
//!   this crate creates (`<id>.jsonl`, its lock file, its quarantine file, its temporary file)
//!   is created `0600`.
//! - **Validate before repairing.** The header is decoded and validated first: its id must equal
//!   the requested id and its version must be supported. Only then are entries interpreted and
//!   any repair or rewrite attempted, so an unsupported or mismatched journal is never changed.
//! - **Repair that never races a writer.** A torn tail is handled while the lock is held: a
//!   complete-but-unterminated final entry is loaded and terminated before the next append,
//!   and an incomplete fragment — including one larger than the encoded entry bound — is moved
//!   to `<id>.jsonl.corrupt-<timestamp>` before the good prefix is rewritten atomically. The
//!   quarantine copy is made durable before the journal is replaced, and it is retained (never
//!   deleted) if the repair fails, because by then it may be the only copy of the fragment.
//! - **Failures force a reopen.** A write or rewrite that fails after it may have changed the
//!   file marks the open journal as needing a reopen: every later append or rewrite returns
//!   [`SessionError::NeedsReopen`](crate::SessionError::NeedsReopen) until the store is dropped
//!   and the journal reopened. The lock stays held until `Drop`.
//! - **Corruption is an error, not a skipped line.** A complete line that is not a valid entry
//!   fails the open with the physical line number in
//!   [`SessionError::CorruptLine`](crate::SessionError::CorruptLine). The line's content is
//!   never part of the error, and nothing is logged.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::entry::{Entry, SessionHeader};
use crate::error::SessionError;
use crate::ids::SessionId;
use crate::store::{OpenedJournal, RecoveryNotice, SessionStore};
use crate::{MAX_ENTRY_BYTES, SESSION_VERSION};

/// Suffix of a journal file.
const JOURNAL_SUFFIX: &str = ".jsonl";

/// Distinguishes temporary files created by one process.
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Distinguishes one lock acquisition from the next, including in the same process.
static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A unique token for one lock acquisition.
///
/// The token is written into the marker file so a store can prove on drop that the marker it
/// created is still its own. Equality is what matters, not the token's internal structure.
fn new_lock_token() -> String {
    let sequence = LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{sequence:x}-{:x}",
        std::process::id(),
        crate::now_unix_nanos()
    )
}

/// A filesystem failure a test injects because the operating system cannot be made to fail
/// deterministically on every platform (a directory `fsync`, a partial append, a failed
/// append `fsync`). Compiled only for tests, so no production path carries test-only state.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InjectedFailure {
    /// `atomic_write` fails syncing the directory after its `rename` has replaced the journal.
    SyncAfterRename,
    /// An append writes a partial line and then fails.
    AppendWrite,
    /// An append writes the whole line but fails its `fsync`.
    AppendSync,
    /// `atomic_write` targets a fixed, already-existing temporary path.
    ExistingTemporary,
}

/// Who created a journal lock file.
///
/// Recorded so a human can decide whether a lock left behind belongs to a live process before
/// calling [`JsonlFileStore::break_stale_lock`]. It is informational and best-effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockOwner {
    /// Process id that created the lock.
    pub pid: u32,
    /// Unix timestamp in seconds when the lock was created.
    pub timestamp: i64,
    /// Token unique to one acquisition, compared for equality on release.
    ///
    /// `Drop` removes the marker file only when the token it reads back equals the token of
    /// its own acquisition, so a marker replaced by another writer (for example after an
    /// external [`JsonlFileStore::break_stale_lock`]) is never deleted by the old writer.
    #[serde(default)]
    pub token: String,
}

/// A JSONL journal store rooted at one directory.
///
/// The directory and the session id decide the file name (`<id>.jsonl`); nothing else about
/// the file system is part of this crate's contract, and no path leaves the store.
#[derive(Debug)]
pub struct JsonlFileStore {
    root: PathBuf,
    open: Option<OpenState>,
    #[cfg(test)]
    injected: Option<InjectedFailure>,
}

/// The journal a store currently has selected, and the exclusive access it holds.
#[derive(Debug)]
struct OpenState {
    /// The validated requested id, which every later path is derived from.
    ///
    /// The file's header is checked against this id on open; the header is never used to build
    /// a path, so a header that named another id could not redirect a write.
    id: SessionId,
    header: SessionHeader,
    /// Held for as long as this store has the journal open; dropping it releases the lock.
    _lock: LockFile,
    /// Whether the journal currently ends with a line terminator.
    trailing_newline: bool,
    /// Set when a write or rewrite failed and may have left the file in an unknown state.
    ///
    /// Every later append/rewrite returns [`SessionError::NeedsReopen`] until the store is
    /// dropped and the journal reopened. The lock stays held until `Drop`.
    needs_reopen: bool,
}

impl JsonlFileStore {
    /// A store rooted at `root`. The directory is created on first use with `0700` (unix).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            open: None,
            #[cfg(test)]
            injected: None,
        }
    }

    /// A store that fails a specific filesystem step, for deterministic tests.
    #[cfg(test)]
    fn with_failure(mut self, failure: InjectedFailure) -> Self {
        self.injected = Some(failure);
        self
    }

    /// The directory this store keeps journals in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Release a lock file left behind by a process that is no longer running.
    ///
    /// # Safety
    ///
    /// This is the only way the crate releases another writer's lock, and it does not check
    /// whether the recorded owner is alive. Call it only after inspecting
    /// [`JsonlFileStore::lock_owner`] and establishing that no writer is running; breaking a
    /// live writer's lock lets two writers append to the same journal.
    ///
    /// Returns whether a lock file existed. Calling it while *this* store holds the lock, or
    /// while the recorded owner is this process, is refused with [`SessionError::Locked`],
    /// because such a lock is not stale.
    pub fn break_stale_lock(&self, id: &SessionId) -> Result<bool, SessionError> {
        if self.open.as_ref().is_some_and(|open| &open.id == id) {
            return Err(SessionError::Locked);
        }
        // A marker whose recorded owner is this process belongs to a live writer in this
        // process, even if it is not this store. Refuse rather than remove it.
        if let Some(owner) = self.lock_owner(id)? {
            if owner.pid == std::process::id() {
                return Err(SessionError::Locked);
            }
        }
        let path = self.lock_path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(SessionError::Io(error)),
        }
    }

    /// Owner metadata of the lock file, when the journal is locked and the metadata is
    /// readable. Best-effort: an unreadable lock file still blocks a second writer.
    pub fn lock_owner(&self, id: &SessionId) -> Result<Option<LockOwner>, SessionError> {
        let path = self.lock_path(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionError::Io(error)),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    fn journal_path(&self, id: &SessionId) -> PathBuf {
        self.root.join(format!("{id}{JOURNAL_SUFFIX}"))
    }

    fn lock_path(&self, id: &SessionId) -> PathBuf {
        self.root.join(format!("{id}{JOURNAL_SUFFIX}.lock"))
    }

    fn quarantine_path(&self, id: &SessionId) -> PathBuf {
        let nanos = crate::now_unix_nanos();
        self.root
            .join(format!("{id}{JOURNAL_SUFFIX}.corrupt-{nanos:x}"))
    }

    /// A temporary path next to the journal, so `rename` stays inside one directory.
    fn temporary_path(&self, journal: &Path) -> Result<PathBuf, SessionError> {
        let name = journal
            .file_name()
            .ok_or(SessionError::InvalidSessionId)?
            .to_string_lossy()
            .into_owned();
        #[cfg(test)]
        if self.injected == Some(InjectedFailure::ExistingTemporary) {
            return Ok(self.root.join(format!("{name}.tmp-existing")));
        }
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .root
            .join(format!("{name}.tmp-{}-{sequence:x}", std::process::id())))
    }

    fn ensure_root(&self) -> Result<(), SessionError> {
        if self.root.is_dir() {
            return Ok(());
        }
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.root)?;
        Ok(())
    }

    /// Remove temporary files a crashed run left behind. Only called while the lock is held.
    ///
    /// Only the exact grammar this crate generates is removed —
    /// `<id>.jsonl.tmp-<decimal pid>-<hex sequence>` — so the reserved temporary name space is
    /// narrow and a user file that merely starts with `<id>.jsonl.tmp-` (for example
    /// `<id>.jsonl.tmp-backup`) is left alone.
    fn clean_stray_temporaries(&self, id: &SessionId) {
        let prefix = format!("{id}{JOURNAL_SUFFIX}.tmp-");
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            if is_generated_temporary(&entry.file_name().to_string_lossy(), &prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn journal_bytes(
        &self,
        header: &SessionHeader,
        entries: &[Entry],
    ) -> Result<Vec<u8>, SessionError> {
        let mut bytes = encode_line(header)?;
        for entry in entries {
            let line = entry.encode()?;
            if line.len() > MAX_ENTRY_BYTES {
                return Err(SessionError::EntryTooLarge { bytes: line.len() });
            }
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
        }
        Ok(bytes)
    }

    /// Write `bytes` to `path` through a temporary file in the same directory.
    ///
    /// A temporary file is removed only when *this* call created it: if exclusive creation
    /// fails because the path already exists, that file is not this operation's and is left
    /// alone. After `rename`, an injected directory-sync failure is the only way a test can
    /// make this return an error with the journal already replaced.
    fn atomic_write(&self, path: &Path, bytes: &[u8]) -> Result<(), SessionError> {
        let temporary = self.temporary_path(path)?;
        let mut file = match create_private(&temporary) {
            Ok(file) => file,
            Err(error) => return Err(SessionError::Io(error)),
        };
        if let Err(error) = write_and_sync(&mut file, bytes) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(SessionError::Io(error));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(SessionError::Io(error));
        }
        if let Some(parent) = path.parent() {
            #[cfg(test)]
            if self.injected == Some(InjectedFailure::SyncAfterRename) {
                return Err(SessionError::Io(io::Error::other(
                    "injected directory sync failure after rename",
                )));
            }
            sync_directory(parent)?;
        }
        Ok(())
    }

    /// Append `bytes` to an open journal file and make them durable.
    ///
    /// Under `cfg(test)` a failure can be injected here because a real partial write or a
    /// failed `fsync` cannot be produced deterministically on every platform.
    fn append_bytes(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.injected == Some(InjectedFailure::AppendWrite) {
            let partial = bytes.len() / 2;
            let _ = file.write_all(&bytes[..partial]);
            return Err(io::Error::other("injected append write failure"));
        }
        file.write_all(bytes)?;
        #[cfg(test)]
        if self.injected == Some(InjectedFailure::AppendSync) {
            return Err(io::Error::other("injected append sync failure"));
        }
        file.sync_all()
    }

    /// Mark the open journal as needing a reopen after a failed write or rewrite.
    fn mark_needs_reopen(&mut self) {
        if let Some(open) = self.open.as_mut() {
            open.needs_reopen = true;
        }
    }

    /// Read the journal, repairing a torn tail while the lock is held.
    ///
    /// The header is decoded and validated **first**: its id must equal the requested id and
    /// its version must be supported. Only then are entries interpreted and any repair or
    /// rewrite attempted, so an unsupported or mismatched journal is never rewritten.
    ///
    /// A fragment that is quarantined is made durable (the directory is synced) before the
    /// journal is replaced, and the quarantine copy is never deleted on a repair failure: once
    /// the journal has been replaced it may be the only remaining copy of the fragment.
    fn read_and_repair(
        &self,
        id: &SessionId,
        path: &Path,
    ) -> Result<(SessionHeader, Vec<Entry>, Option<RecoveryNotice>, bool), SessionError> {
        let bytes = fs::read(path)?;

        // Decode the header line before anything else, so an unsupported version or a
        // mismatched id is rejected before entries are read and before any repair.
        let Some(position) = bytes.iter().position(|byte| *byte == b'\n') else {
            // No complete first line: an empty file or a torn header is not a journal.
            return Err(SessionError::EmptyJournal);
        };
        let header: SessionHeader = serde_json::from_slice(&bytes[..position])
            .map_err(|_| SessionError::MalformedHeader)?;
        if &header.id != id {
            return Err(SessionError::MismatchedJournalId);
        }
        if header.version != SESSION_VERSION {
            return Err(SessionError::UnsupportedVersion {
                found: header.version,
                supported: SESSION_VERSION,
            });
        }

        let mut entries: Vec<Entry> = Vec::new();
        let mut line_number = 1usize;
        let mut rest: &[u8] = &bytes[position + 1..];

        while let Some(position) = rest.iter().position(|byte| *byte == b'\n') {
            let line = &rest[..position];
            rest = &rest[position + 1..];
            line_number += 1;
            if line.len() > MAX_ENTRY_BYTES {
                return Err(SessionError::EntryTooLarge { bytes: line.len() });
            }
            entries.push(
                serde_json::from_slice(line)
                    .map_err(|_| SessionError::CorruptLine { line: line_number })?,
            );
        }

        if rest.is_empty() {
            return Ok((header, entries, None, true));
        }

        // The file does not end with a newline, so `rest` is an unterminated final fragment.
        // A complete entry there is kept and terminated before the next append.
        if rest.len() <= MAX_ENTRY_BYTES {
            if let Ok(entry) = serde_json::from_slice::<Entry>(rest) {
                entries.push(entry);
                return Ok((
                    header,
                    entries,
                    Some(RecoveryNotice::MissingTrailingNewline),
                    false,
                ));
            }
        }

        // The fragment is incomplete. Keep it in quarantine before repairing the journal, and
        // make the quarantine copy durable before the journal is replaced.
        let quarantine_path = self.quarantine_path(id);
        write_private(&quarantine_path, rest)?;
        sync_directory(&self.root)?;
        let repaired = self.journal_bytes(&header, &entries)?;
        // Never delete the quarantine copy on a repair failure: the rename may have succeeded
        // before the failure, so the journal may no longer hold the fragment.
        self.atomic_write(path, &repaired)?;
        Ok((
            header,
            entries,
            Some(RecoveryNotice::QuarantinedTail { quarantine_path }),
            true,
        ))
    }
}

impl SessionStore for JsonlFileStore {
    fn create(&mut self, header: &SessionHeader) -> Result<(), SessionError> {
        // Validate the supported version before creating anything, so a caller cannot write a
        // journal this reader would refuse to open.
        if header.version != SESSION_VERSION {
            return Err(SessionError::UnsupportedVersion {
                found: header.version,
                supported: SESSION_VERSION,
            });
        }
        self.ensure_root()?;
        let path = self.journal_path(&header.id);
        let lock = LockFile::acquire(self.lock_path(&header.id))?;
        self.clean_stray_temporaries(&header.id);
        if path.exists() {
            return Err(SessionError::AlreadyExists);
        }
        let bytes = encode_line(header)?;
        let mut file = create_private(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                SessionError::AlreadyExists
            } else {
                SessionError::Io(error)
            }
        })?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        sync_directory(&self.root)?;
        self.open = Some(OpenState {
            id: header.id.clone(),
            header: header.clone(),
            _lock: lock,
            trailing_newline: true,
            needs_reopen: false,
        });
        Ok(())
    }

    fn open(&mut self, id: &SessionId) -> Result<OpenedJournal, SessionError> {
        self.ensure_root()?;
        let path = self.journal_path(id);
        if !path.exists() {
            return Err(SessionError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "the session journal does not exist",
            )));
        }
        let lock = LockFile::acquire(self.lock_path(id))?;
        self.clean_stray_temporaries(id);
        let (header, entries, notice, trailing_newline) = self.read_and_repair(id, &path)?;
        let (header, entries) = migrate_journal(header, entries)?;
        self.open = Some(OpenState {
            id: id.clone(),
            trailing_newline,
            header: header.clone(),
            _lock: lock,
            needs_reopen: false,
        });
        Ok(OpenedJournal {
            header,
            entries,
            notice,
        })
    }

    fn append(&mut self, entry: &Entry) -> Result<(), SessionError> {
        let (path, needs_terminator) = match self.open.as_ref() {
            Some(open) if open.needs_reopen => return Err(SessionError::NeedsReopen),
            Some(open) => (self.journal_path(&open.id), !open.trailing_newline),
            None => return Err(SessionError::NotOpen),
        };
        let line = entry.encode()?;
        if line.len() > MAX_ENTRY_BYTES {
            return Err(SessionError::EntryTooLarge { bytes: line.len() });
        }
        let mut bytes = Vec::with_capacity(line.len() + 2);
        if needs_terminator {
            // Terminate the entry the reader accepted but the file never closed.
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&line);
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(SessionError::Io)?;
        // From here on a failure may have changed the file, so the journal must be reopened
        // before the next write instead of appending onto an unknown tail.
        if let Err(error) = self.append_bytes(&mut file, &bytes) {
            drop(file);
            self.mark_needs_reopen();
            return Err(SessionError::Io(error));
        }
        drop(file);
        if let Some(open) = self.open.as_mut() {
            open.trailing_newline = true;
        }
        Ok(())
    }

    fn rewrite(&mut self, entries: &[Entry]) -> Result<(), SessionError> {
        let Some(open) = self.open.as_ref() else {
            return Err(SessionError::NotOpen);
        };
        if open.needs_reopen {
            return Err(SessionError::NeedsReopen);
        }
        let header = open.header.clone();
        let path = self.journal_path(&open.id);
        let bytes = self.journal_bytes(&header, entries)?;
        if let Err(error) = self.atomic_write(&path, &bytes) {
            self.mark_needs_reopen();
            return Err(error);
        }
        if let Some(open) = self.open.as_mut() {
            open.trailing_newline = true;
        }
        Ok(())
    }
}

/// Migrate a loaded journal to [`SESSION_VERSION`], idempotently.
///
/// Applying this function twice to a journal produces the same journal. At v1 it is the
/// identity function: the crate's only version is the one it writes, and a journal below that
/// version does not exist. A future format version adds an arm here rather than changing a
/// reader, and the arm must keep the entries already understood.
///
/// Migration works on the values that were read; it does not rewrite the file. A journal with
/// an unknown version is rejected by the reader with
/// [`SessionError::UnsupportedVersion`].
pub fn migrate_journal(
    header: SessionHeader,
    entries: Vec<Entry>,
) -> Result<(SessionHeader, Vec<Entry>), SessionError> {
    if header.version == SESSION_VERSION {
        return Ok((header, entries));
    }
    // No other version exists yet, and a file written by a newer format must not be
    // interpreted as if it were v1.
    Err(SessionError::UnsupportedVersion {
        found: header.version,
        supported: SESSION_VERSION,
    })
}

/// Create a file that cannot be read or written by other users (unix: `0600`).
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create a private file with `bytes` and make it durable.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create_private(path)?;
    write_and_sync(&mut file, bytes)
}

/// Write `bytes` to an already-open file and make them durable.
fn write_and_sync(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

/// Whether `name` is a temporary file this crate generated for `prefix`.
///
/// The generated grammar is exactly `<id>.jsonl.tmp-<decimal pid>-<hex sequence>`, so a file
/// that merely starts with the prefix (for example `<id>.jsonl.tmp-backup`) is not removed.
fn is_generated_temporary(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some((pid, sequence)) = rest.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !sequence.is_empty()
        && sequence.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Make a directory entry durable.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), SessionError> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// Directories are not opened for `fsync` on Windows; the rename is still atomic there.
#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), SessionError> {
    Ok(())
}

/// Encode one JSONL line, terminator included.
fn encode_line<T: Serialize>(value: &T) -> Result<Vec<u8>, SessionError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| SessionError::InvalidEntry)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A lock file, held for the lifetime of the store that acquired it.
///
/// The lock is an exclusive-create file: the first writer creates it, and any later writer
/// sees [`io::ErrorKind::AlreadyExists`] and gets [`SessionError::Locked`]. The file records
/// its owner so a human can judge whether it is stale. Rust 1.88 has no
/// `std::fs::File::try_lock`, so this is deliberately a file marker rather than an OS advisory
/// lock; the tradeoff is documented on [`JsonlFileStore`].
#[derive(Debug)]
struct LockFile {
    path: PathBuf,
    /// This acquisition's token, compared against the marker on drop.
    token: String,
    _file: File,
}

impl LockFile {
    fn acquire(path: PathBuf) -> Result<Self, SessionError> {
        let token = new_lock_token();
        let owner = LockOwner {
            pid: std::process::id(),
            timestamp: crate::now_unix_seconds(),
            token: token.clone(),
        };
        let bytes = serde_json::to_vec(&owner).map_err(|_| SessionError::InvalidEntry)?;
        let mut file = match create_private(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(SessionError::Locked)
            }
            Err(error) => return Err(SessionError::Io(error)),
        };
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(Self {
            path,
            token,
            _file: file,
        })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // Remove the marker only when it still holds this acquisition's token. If another
        // writer replaced it (for example after an external break), the old store must not
        // delete the new writer's marker, or a third writer could then acquire the lock while
        // the second still holds it.
        if let Ok(bytes) = fs::read(&self.path) {
            if let Ok(owner) = serde_json::from_slice::<LockOwner>(&bytes) {
                if owner.token == self.token && !self.token.is_empty() {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;
    use crate::ids::EntryId;

    /// A private directory that removes itself when the test ends.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = crate::now_unix_nanos();
            let path = std::env::temp_dir().join(format!(
                "mira-session-unit-{label}-{}-{nanos:x}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("the test directory is creatable");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn message_entry(id: &str, text: &str) -> Entry {
        Entry::new(
            EntryId::new(id),
            None,
            2,
            EntryKind::Message {
                message: mira_ai::Message::User(mira_ai::UserMessage::text(text)),
            },
        )
    }

    fn quarantines(dir: &TempDir) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir.path())
            .expect("the directory is readable")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().contains(".corrupt-"))
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn a_failed_post_rename_sync_keeps_the_quarantine_copy() {
        let dir = TempDir::new("quarantine-retained");
        let id = SessionId::new("durable").expect("a valid session id");
        let journal = dir.path().join(format!("{id}.jsonl"));

        let mut store = JsonlFileStore::new(dir.path());
        store
            .create(&SessionHeader::new(id.clone()))
            .expect("the journal is created");
        drop(store);

        // A torn fragment that is not valid JSON: it takes the quarantine path.
        let fragment: &[u8] = br#"{"type":"message","id":"e-9","timestamp":9,"mess"#;
        {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(&journal)
                .expect("the journal is openable");
            file.write_all(fragment).expect("the fragment is written");
        }

        let mut failing =
            JsonlFileStore::new(dir.path()).with_failure(InjectedFailure::SyncAfterRename);
        let error = failing
            .open(&id)
            .expect_err("the repair fails after the journal was replaced");
        assert!(matches!(error, SessionError::Io(_)), "{error:?}");

        let quarantines = quarantines(&dir);
        assert_eq!(quarantines.len(), 1, "the quarantine copy is retained");
        assert_eq!(
            fs::read(&quarantines[0]).expect("the quarantine file is readable"),
            fragment,
            "the only copy of the fragment survived the failed repair"
        );

        let journal_bytes = fs::read(&journal).expect("the journal is readable");
        assert!(
            !journal_bytes
                .windows(fragment.len())
                .any(|window| window == fragment),
            "the renamed journal no longer holds the fragment"
        );
    }

    #[test]
    fn an_append_write_failure_requires_a_reopen() {
        let dir = TempDir::new("append-write");
        let id = SessionId::new("writer").expect("a valid session id");
        let mut store = JsonlFileStore::new(dir.path()).with_failure(InjectedFailure::AppendWrite);
        store
            .create(&SessionHeader::new(id.clone()))
            .expect("the journal is created");
        let entry = message_entry("e-1", "one");

        let error = store.append(&entry).expect_err("the append fails");
        assert!(matches!(error, SessionError::Io(_)), "{error:?}");

        let error = store
            .append(&entry)
            .expect_err("the journal must be reopened");
        assert!(matches!(error, SessionError::NeedsReopen), "{error:?}");
        let error = store
            .rewrite(&[])
            .expect_err("the journal must be reopened");
        assert!(matches!(error, SessionError::NeedsReopen), "{error:?}");
        drop(store);

        let mut reopened = JsonlFileStore::new(dir.path());
        let opened = reopened.open(&id).expect("the journal reopens");
        assert!(
            matches!(opened.notice, Some(RecoveryNotice::QuarantinedTail { .. })),
            "the partial fragment is quarantined on reopen: {:?}",
            opened.notice
        );
        assert!(opened.entries.is_empty());
    }

    #[test]
    fn an_append_sync_failure_requires_a_reopen_and_the_entry_is_on_disk() {
        let dir = TempDir::new("append-sync");
        let id = SessionId::new("syncer").expect("a valid session id");
        let mut store = JsonlFileStore::new(dir.path()).with_failure(InjectedFailure::AppendSync);
        store
            .create(&SessionHeader::new(id.clone()))
            .expect("the journal is created");
        let entry = message_entry("e-1", "one");

        let error = store.append(&entry).expect_err("the sync fails");
        assert!(matches!(error, SessionError::Io(_)), "{error:?}");
        let error = store
            .append(&entry)
            .expect_err("the journal must be reopened");
        assert!(matches!(error, SessionError::NeedsReopen), "{error:?}");
        drop(store);

        let mut reopened = JsonlFileStore::new(dir.path());
        let opened = reopened.open(&id).expect("the journal reopens");
        assert_eq!(
            opened.entries.len(),
            1,
            "the entry that reached the file before the sync failure is loaded"
        );
        assert_eq!(opened.notice, None);
    }

    #[test]
    fn a_failed_temporary_creation_does_not_delete_an_existing_file() {
        let dir = TempDir::new("temp-collision");
        let id = SessionId::new("collision").expect("a valid session id");
        let mut store =
            JsonlFileStore::new(dir.path()).with_failure(InjectedFailure::ExistingTemporary);
        store
            .create(&SessionHeader::new(id.clone()))
            .expect("the journal is created");

        let temporary = dir.path().join(format!("{id}.jsonl.tmp-existing"));
        fs::write(&temporary, b"pre-existing").expect("the colliding file is written");

        let error = store
            .rewrite(&[])
            .expect_err("the temporary cannot be created");
        assert!(matches!(error, SessionError::Io(_)), "{error:?}");
        assert_eq!(
            fs::read(&temporary).expect("the colliding file is readable"),
            b"pre-existing",
            "a file this operation did not create is left alone"
        );
    }
}
