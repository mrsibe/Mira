//! Single-writer locking: acquisition, refusal, release and the documented recovery path.

mod support;

use std::fs;

use mira_session::{
    JsonlFileStore, LockOwner, Session, SessionError, SessionHeader, SessionId, SessionStore,
};

use support::{user_message, TempDir};

fn session_id() -> SessionId {
    SessionId::new("locking").expect("a valid session id")
}

fn create_journal(dir: &TempDir, id: &SessionId) {
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("kept"))
        .expect("appended");
}

#[test]
fn a_second_writer_gets_a_typed_locked_error() {
    let dir = TempDir::new("second-writer");
    let id = session_id();
    let _first = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");

    let mut second = JsonlFileStore::new(dir.path());
    let error = second.open(&id).expect_err("the journal is locked");
    assert!(matches!(error, SessionError::Locked), "{error}");
    assert_eq!(
        error.to_string(),
        "another writer holds the session journal lock"
    );

    let error = JsonlFileStore::new(dir.path())
        .create(&SessionHeader::new(id.clone()))
        .expect_err("creating is locked too");
    assert!(matches!(error, SessionError::Locked), "{error}");
}

#[test]
fn dropping_the_store_releases_the_lock() {
    let dir = TempDir::new("release");
    let id = session_id();
    create_journal(&dir, &id);
    assert!(
        !dir.lock(&id).exists(),
        "a finished writer removes its lock file"
    );

    let first = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert!(dir.lock(&id).exists(), "an open journal holds the lock");
    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("locked");
    assert!(matches!(error, SessionError::Locked), "{error}");

    drop(first);
    assert!(!dir.lock(&id).exists());
    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the lock is free");
    assert_eq!(reopened.entries().len(), 1);
}

#[test]
fn the_lock_records_its_owner_while_it_is_held() {
    let dir = TempDir::new("owner");
    let id = session_id();
    let store = JsonlFileStore::new(dir.path());
    let session = Session::create(store, SessionHeader::new(id.clone())).expect("created");

    let owner = JsonlFileStore::new(dir.path())
        .lock_owner(&id)
        .expect("the lock file is readable")
        .expect("the journal is locked");
    assert_eq!(owner.pid, std::process::id());
    assert!(owner.timestamp > 0, "the owner metadata records a time");

    drop(session);
    assert_eq!(
        JsonlFileStore::new(dir.path())
            .lock_owner(&id)
            .expect("no lock file is not an error"),
        None
    );
}

#[test]
fn a_lock_left_by_a_crashed_writer_needs_an_explicit_recovery() {
    let dir = TempDir::new("stale");
    let id = session_id();
    create_journal(&dir, &id);

    // A lock file left behind by a process that is no longer running. It is never stolen
    // automatically, because a marker file cannot be distinguished from a live writer's.
    fs::write(dir.lock(&id), br#"{"pid":999999,"timestamp":1}"#)
        .expect("the stale lock is written");
    let mut store = JsonlFileStore::new(dir.path());
    let error = store.open(&id).expect_err("a stale lock still blocks");
    assert!(matches!(error, SessionError::Locked), "{error}");
    assert_eq!(
        store.lock_owner(&id).expect("metadata is readable"),
        Some(LockOwner {
            pid: 999_999,
            timestamp: 1,
            token: String::new(),
        })
    );

    // The documented recovery path: inspect the owner, then release it explicitly.
    assert!(store.break_stale_lock(&id).expect("the lock is released"));
    assert!(!dir.lock(&id).exists());
    let session = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(session.entries().len(), 1);

    // Nothing to break now.
    assert!(!JsonlFileStore::new(dir.path())
        .break_stale_lock(&SessionId::new("unlocked").expect("a valid session id"))
        .expect("an absent lock is not an error"));
}

#[test]
fn a_store_refuses_to_break_a_live_lock_held_elsewhere_in_this_process() {
    let dir = TempDir::new("same-process-break");
    let id = session_id();
    create_journal(&dir, &id);

    // This store holds the lock; a different store in the same process must not remove it.
    let holder = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let owner = JsonlFileStore::new(dir.path())
        .lock_owner(&id)
        .expect("metadata is readable")
        .expect("the journal is locked");
    assert_eq!(owner.pid, std::process::id());
    assert!(!owner.token.is_empty(), "the marker records a unique token");

    let other = JsonlFileStore::new(dir.path());
    assert!(matches!(
        other
            .break_stale_lock(&id)
            .expect_err("a live same-process lock is not stale"),
        SessionError::Locked
    ));
    assert!(dir.lock(&id).exists(), "the live marker survives");

    drop(holder);
    assert!(!dir.lock(&id).exists(), "the holder removes its own marker");
}

#[test]
fn an_old_store_does_not_delete_a_marker_that_replaced_its_own() {
    let dir = TempDir::new("replaced-marker");
    let id = session_id();
    create_journal(&dir, &id);

    // The first writer holds the lock.
    let first = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let first_owner = JsonlFileStore::new(dir.path())
        .lock_owner(&id)
        .expect("metadata is readable")
        .expect("the journal is locked");

    // Simulate an external break: the marker is removed, then a second writer acquires it.
    fs::remove_file(dir.lock(&id)).expect("the marker is removed");
    let second = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the lock is free");
    let second_owner = JsonlFileStore::new(dir.path())
        .lock_owner(&id)
        .expect("metadata is readable")
        .expect("the journal is locked again");
    assert_ne!(
        first_owner.token, second_owner.token,
        "each acquisition records its own token"
    );

    // Dropping the old store must not delete the new writer's marker.
    drop(first);
    assert!(
        dir.lock(&id).exists(),
        "the replacement marker survives the old store's drop"
    );
    assert!(matches!(
        Session::open(JsonlFileStore::new(dir.path()), &id)
            .expect_err("the second writer still holds the lock"),
        SessionError::Locked
    ));

    drop(second);
    assert!(!dir.lock(&id).exists());
}

#[test]
fn a_store_refuses_to_break_its_own_lock() {
    let dir = TempDir::new("own-lock");
    let id = session_id();
    create_journal(&dir, &id);
    assert!(!dir.lock(&id).exists());

    // This store takes the lock itself, so its lock is not stale.
    let mut store = JsonlFileStore::new(dir.path());
    store.open(&id).expect("the journal opens");
    assert!(dir.lock(&id).exists());
    assert!(matches!(
        store
            .break_stale_lock(&id)
            .expect_err("the lock is held by this store"),
        SessionError::Locked
    ));
    assert!(dir.lock(&id).exists(), "the lock was not removed");
    assert!(matches!(
        JsonlFileStore::new(dir.path())
            .open(&id)
            .expect_err("still locked"),
        SessionError::Locked
    ));
}

#[test]
fn a_refused_or_missing_open_leaves_no_lock_behind() {
    let dir = TempDir::new("no-lock");
    let id = session_id();
    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("no such journal");
    assert!(matches!(error, SessionError::Io(_)), "{error:?}");
    assert!(!dir.lock(&id).exists(), "a missing journal takes no lock");

    let missing = SessionId::new("stale-parent").expect("a valid session id");
    dir.write_journal(
        &missing,
        format!(
            "{{\"type\":\"session\",\"version\":1,\"id\":\"stale-parent\",\"timestamp\":1}}\n{}\n",
            r#"{"type":"message","id":"e-1","parent_id":"gone","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one"}]}}"#
        )
        .as_bytes(),
    );
    let error = Session::open(JsonlFileStore::new(dir.path()), &missing)
        .expect_err("the entry graph is invalid");
    assert!(matches!(error, SessionError::DanglingParent), "{error:?}");
    assert!(
        !dir.lock(&missing).exists(),
        "a refused open releases the lock it took"
    );
}
