//! Durability, repair and integrity rules of the JSONL file backend.

mod support;

use std::fs;
use std::io;

use mira_session::{
    migrate_journal, Entry, EntryId, EntryKind, JsonlFileStore, MemoryStore, RecoveryNotice,
    Session, SessionError, SessionHeader, SessionId, SessionStore, MAX_ENTRY_BYTES,
    SESSION_VERSION,
};

use support::{message_text, user_message, TempDir};

fn session_id() -> SessionId {
    SessionId::new("durability").expect("a valid session id")
}

fn texts(session: &Session<impl SessionStore>) -> Vec<String> {
    session
        .active_messages()
        .expect("the path resolves")
        .iter()
        .map(message_text)
        .collect()
}

#[test]
fn an_append_is_on_disk_and_leaves_no_temporary_file() {
    let dir = TempDir::new("append");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("written"))
        .expect("appended");

    // Read the file through a different handle while the session is still open: the entry is
    // there (the append is followed by fsync, which cannot be observed from the test process).
    let written = fs::read_to_string(dir.journal(&id)).expect("the journal is readable");
    assert!(written.ends_with('\n'), "an append terminates its line");
    let last = written.lines().next_back().expect("the journal has a line");
    let entry: Entry = serde_json::from_str(last).expect("the last line is an entry");
    assert_eq!(
        entry.kind,
        EntryKind::Message {
            message: user_message("written")
        }
    );
    assert!(
        dir.names_containing(".tmp-").is_empty(),
        "no temporary file is left behind"
    );
    assert_eq!(
        dir.names_containing(".lock"),
        vec![format!("{id}.jsonl.lock")]
    );
}

#[test]
fn a_stray_temporary_file_is_ignored_and_cleaned_only_while_the_lock_is_held() {
    let dir = TempDir::new("stray");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("kept"))
        .expect("appended");

    // Debris a crashed run could have left next to the journal.
    let stray = dir.path().join(format!("{id}.jsonl.tmp-999-0"));
    fs::write(&stray, b"{\"type\":\"message\",\"id\":\"half").expect("the stray file is written");

    // A second writer cannot take the lock, so it must not clean anything up either.
    let mut other = JsonlFileStore::new(dir.path());
    let error = other.open(&id).expect_err("the lock is held");
    assert!(matches!(error, SessionError::Locked), "{error}");
    assert!(stray.exists(), "cleanup requires the lock");

    drop(session);
    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert!(!stray.exists(), "the next lock holder removes the debris");
    assert_eq!(texts(&reopened), ["kept".to_string()]);
    assert_eq!(
        reopened.entries().len(),
        1,
        "the debris was never read as an entry"
    );
}

#[test]
fn a_torn_valid_fragment_loads_with_a_notice_and_the_next_append_keeps_the_file_valid() {
    let dir = TempDir::new("torn-valid");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("one"))
        .expect("appended");
    session
        .append_message(user_message("two"))
        .expect("appended");
    drop(session);

    // A crash between "write the line" and "write the newline".
    let text = dir.journal_text(&id);
    let torn = text
        .strip_suffix('\n')
        .expect("the journal ends with a newline");
    dir.write_journal(&id, torn.as_bytes());

    let mut reopened =
        Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(
        reopened.recovery_notice(),
        Some(&RecoveryNotice::MissingTrailingNewline)
    );
    assert_eq!(texts(&reopened), ["one".to_string(), "two".to_string()]);

    reopened
        .append_message(user_message("three"))
        .expect("appended");
    drop(reopened);

    let repaired = dir.journal_text(&id);
    assert!(repaired.ends_with('\n'));
    assert!(
        !repaired.contains("\n\n"),
        "the repair adds no blank line: {repaired}"
    );
    assert_eq!(repaired.lines().count(), 4, "header plus three entries");

    let clean = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(clean.recovery_notice(), None, "the file is no longer torn");
    assert_eq!(
        texts(&clean),
        ["one".to_string(), "two".to_string(), "three".to_string()]
    );
}

#[test]
fn a_torn_incomplete_fragment_is_quarantined_while_the_lock_is_held() {
    let dir = TempDir::new("torn-invalid");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("good"))
        .expect("appended");
    drop(session);

    let fragment = br#"{"type":"message","id":"e-9","parent_id":"e-1","timestamp":9,"message":{"type":"user","con"#;
    dir.append_journal(&id, fragment);

    let session = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let notice = session.recovery_notice().expect("a repair is reported");
    let RecoveryNotice::QuarantinedTail { quarantine_path } = notice else {
        panic!("expected a quarantined tail, got {notice:?}");
    };
    assert!(
        quarantine_path
            .file_name()
            .expect("the quarantine file has a name")
            .to_string_lossy()
            .starts_with(&format!("{id}.jsonl.corrupt-")),
        "the quarantine file is a sibling of the journal: {quarantine_path:?}"
    );
    assert_eq!(
        fs::read(quarantine_path).expect("the quarantine file exists"),
        fragment,
        "the fragment is preserved whole"
    );
    let journal = dir.journal_text(&id);
    assert!(
        !journal.contains("e-9"),
        "the fragment is gone from the journal: {journal}"
    );
    assert_eq!(texts(&session), ["good".to_string()]);
    assert_eq!(session.entries().len(), 1);
    drop(session);

    // The repaired journal is valid and no longer reports a notice.
    let clean = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(clean.recovery_notice(), None);

    // Junk that is not JSON at all takes the same path.
    let other = SessionId::new("junk").expect("a valid session id");
    let mut second = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(other.clone()),
    )
    .expect("the journal is created");
    second
        .append_message(user_message("kept"))
        .expect("appended");
    drop(second);
    dir.append_journal(&other, b"not json at all");
    let repaired =
        Session::open(JsonlFileStore::new(dir.path()), &other).expect("the journal opens");
    assert!(matches!(
        repaired.recovery_notice(),
        Some(RecoveryNotice::QuarantinedTail { .. })
    ));
    assert_eq!(texts(&repaired), ["kept".to_string()]);
}

#[test]
fn a_quarantine_file_is_private_and_never_left_as_a_journal() {
    // The quarantine file is created next to the journal, so it must be as private as the
    // journal itself, and the journal keeps its own name.
    let dir = TempDir::new("quarantine-permissions");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("kept"))
        .expect("appended");
    drop(session);
    dir.append_journal(&id, b"{\"type\":");

    let session = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let Some(RecoveryNotice::QuarantinedTail { quarantine_path }) = session.recovery_notice()
    else {
        panic!("the tail is quarantined");
    };
    assert!(quarantine_path.exists());
    assert_eq!(dir.names_containing(".corrupt-").len(), 1);
    assert_eq!(
        dir.names_containing(".tmp-").len(),
        0,
        "no temporary file remains"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(quarantine_path)
            .expect("the quarantine file is readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the quarantine file is private");
    }
}

#[test]
fn middle_file_corruption_reports_the_line_number_and_never_the_content() {
    let dir = TempDir::new("corrupt-middle");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("one"))
        .expect("appended");
    session
        .append_message(user_message("two"))
        .expect("appended");
    drop(session);

    let text = dir.journal_text(&id);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "header plus two entries");
    let corrupt = format!(
        "{}\n{}\n{{\"type\":\"message\",\"message\":\"SENTINEL-PRIVATE-CONTENT\n",
        lines[0], lines[1]
    );
    dir.write_journal(&id, corrupt.as_bytes());

    let error =
        Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("the line is corrupt");
    match &error {
        SessionError::CorruptLine { line } => assert_eq!(*line, 3),
        other => panic!("expected a corrupt middle line, got {other:?}"),
    }
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains("SENTINEL"),
        "an error never echoes journal content: {rendered}"
    );
    assert_eq!(
        dir.journal_text(&id),
        corrupt,
        "a corrupt middle line is reported, not repaired"
    );
    assert!(
        !dir.lock(&id).exists(),
        "a refused open releases its lock again"
    );

    // Valid JSON that is not an entry is corruption too.
    let second = SessionId::new("not-an-entry").expect("a valid session id");
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(second.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("one"))
        .expect("appended");
    drop(session);
    let text = dir.journal_text(&second);
    let lines: Vec<&str> = text.lines().collect();
    dir.write_journal(
        &second,
        format!("{}\n{}\n{{\"id\":\"e-2\"}}\n", lines[0], lines[1]).as_bytes(),
    );
    let error = Session::open(JsonlFileStore::new(dir.path()), &second).expect_err("corrupt");
    assert!(
        matches!(error, SessionError::CorruptLine { line: 3 }),
        "{error:?}"
    );
}

#[test]
fn an_oversized_entry_is_refused_before_it_is_written() {
    let dir = TempDir::new("oversized");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    let before = dir.journal_text(&id);

    let huge = "x".repeat(MAX_ENTRY_BYTES);
    let error = session
        .append_message(user_message(&huge))
        .expect_err("the entry is too large");
    match error {
        SessionError::EntryTooLarge { bytes } => assert!(bytes > MAX_ENTRY_BYTES),
        other => panic!("expected an oversized entry error, got {other:?}"),
    }
    assert_eq!(dir.journal_text(&id), before, "nothing was written");
    assert!(
        session.entries().is_empty(),
        "nothing was remembered either"
    );

    // The in-memory backend enforces the same bound.
    let mut memory = Session::create(MemoryStore::new(), SessionHeader::new(session_id()))
        .expect("the journal is created");
    let error = memory
        .append_message(user_message(&huge))
        .expect_err("the entry is too large");
    assert!(
        matches!(error, SessionError::EntryTooLarge { .. }),
        "{error:?}"
    );
}

#[test]
fn an_unsupported_header_version_is_a_typed_error() {
    let dir = TempDir::new("version");
    for (version, id) in [(2u32, "newer"), (0u32, "older")] {
        let id = SessionId::new(id).expect("a valid session id");
        let header = format!(
            "{{\"type\":\"session\",\"version\":{version},\"id\":\"{id}\",\"timestamp\":1}}\n"
        );
        dir.write_journal(&id, header.as_bytes());
        let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("unsupported");
        match error {
            SessionError::UnsupportedVersion { found, supported } => {
                assert_eq!(found, version);
                assert_eq!(supported, SESSION_VERSION);
            }
            other => panic!("expected an unsupported version error, got {other:?}"),
        }
    }
}

#[test]
fn the_migration_hook_is_idempotent_and_a_no_op_at_version_one() {
    let id = session_id();
    let header = SessionHeader::at(id, 1);
    let entries = vec![Entry::new(
        EntryId::new("e-1"),
        None,
        2,
        EntryKind::Label {
            target_id: EntryId::new("e-1"),
            label: Some("one".to_string()),
        },
    )];

    let (migrated_header, migrated_entries) =
        migrate_journal(header.clone(), entries.clone()).expect("v1 needs no migration");
    assert_eq!(migrated_header, header);
    assert_eq!(migrated_entries, entries);
    assert_eq!(migrated_header.version, SESSION_VERSION);

    let (again_header, again_entries) =
        migrate_journal(migrated_header.clone(), migrated_entries.clone())
            .expect("the hook is idempotent");
    assert_eq!(again_header, migrated_header);
    assert_eq!(again_entries, migrated_entries);

    let mut future = header;
    future.version = 77;
    let error = migrate_journal(future, entries).expect_err("a newer version is refused");
    assert!(
        matches!(
            error,
            SessionError::UnsupportedVersion {
                found: 77,
                supported: SESSION_VERSION
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_dangling_parent_is_a_typed_error() {
    let dir = TempDir::new("dangling");
    let id = SessionId::new("raw").expect("a valid session id");
    let journal = format!(
        "{}\n{}\n",
        support::HEADER,
        r#"{"type":"message","id":"e-1","parent_id":"missing","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one"}]}}"#
    );
    dir.write_journal(&id, journal.as_bytes());

    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("dangling parent");
    assert!(matches!(error, SessionError::DanglingParent), "{error:?}");
    assert!(
        !dir.lock(&id).exists(),
        "the refused open released its lock"
    );
}

#[test]
fn a_parent_cycle_is_a_typed_error() {
    let dir = TempDir::new("cycle");
    let id = SessionId::new("raw").expect("a valid session id");
    let journal = format!(
        "{}\n{}\n{}\n",
        support::HEADER,
        r#"{"type":"model_change","id":"e-1","parent_id":"e-2","timestamp":2,"provider":"p","model":"m"}"#,
        r#"{"type":"model_change","id":"e-2","parent_id":"e-1","timestamp":3,"provider":"p","model":"m"}"#
    );
    dir.write_journal(&id, journal.as_bytes());

    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("cycle");
    assert!(matches!(error, SessionError::ParentCycle), "{error:?}");
}

#[test]
fn a_duplicate_entry_id_is_a_typed_error() {
    let dir = TempDir::new("duplicate");
    let id = SessionId::new("raw").expect("a valid session id");
    let journal = format!(
        "{}\n{}\n{}\n",
        support::HEADER,
        r#"{"type":"model_change","id":"e-1","timestamp":2,"provider":"p","model":"m"}"#,
        r#"{"type":"model_change","id":"e-1","parent_id":"e-2","timestamp":3,"provider":"p","model":"m"}"#
    );
    dir.write_journal(&id, journal.as_bytes());

    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("duplicate id");
    assert!(matches!(error, SessionError::DuplicateEntryId), "{error:?}");
}

#[test]
fn opening_a_missing_journal_is_a_typed_io_error() {
    let dir = TempDir::new("missing");
    let id = session_id();
    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("no such journal");
    match error {
        SessionError::Io(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        other => panic!("expected a not-found io error, got {other:?}"),
    }
    assert!(!dir.lock(&id).exists(), "a missing journal takes no lock");
}

#[test]
fn a_journal_without_a_header_is_not_a_journal() {
    let dir = TempDir::new("no-header");
    let id = session_id();
    dir.write_journal(&id, b"");
    assert!(matches!(
        Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("empty"),
        SessionError::EmptyJournal
    ));

    let torn_header = SessionId::new("torn-header").expect("a valid session id");
    dir.write_journal(&torn_header, b"{\"type\":\"session\",\"version\":");
    assert!(matches!(
        Session::open(JsonlFileStore::new(dir.path()), &torn_header).expect_err("torn header"),
        SessionError::EmptyJournal
    ));

    let not_a_header = SessionId::new("not-a-header").expect("a valid session id");
    dir.write_journal(&not_a_header, b"{\"type\":\"message\"}\n");
    assert!(matches!(
        Session::open(JsonlFileStore::new(dir.path()), &not_a_header).expect_err("not a header"),
        SessionError::MalformedHeader
    ));
}

#[cfg(unix)]
#[test]
fn journal_files_are_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("permissions");
    // A nested root, so the store creates the directory itself.
    let root = dir.path().join("nested").join("sessions");
    let id = session_id();
    let journal = root.join(format!("{id}.jsonl"));
    let lock = root.join(format!("{id}.jsonl.lock"));
    let names = |path: &std::path::Path| -> Vec<String> {
        fs::read_dir(path)
            .expect("the directory is readable")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    };

    let mut session = Session::create(JsonlFileStore::new(&root), SessionHeader::new(id.clone()))
        .expect("the journal is created");
    session
        .append_message(user_message("kept"))
        .expect("appended");

    let mode = |path: &std::path::Path| {
        fs::metadata(path)
            .expect("the path exists")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&root), 0o700, "the journal directory is private");
    assert_eq!(mode(&journal), 0o600, "the journal is private");
    assert_eq!(mode(&lock), 0o600, "the lock file is private");

    // A rewrite goes through a temporary file that is renamed over the journal, so the journal
    // keeps its restrictive mode and no temporary file survives.
    drop(session);
    let mut store = JsonlFileStore::new(&root);
    let opened = store.open(&id).expect("the journal opens");
    store
        .rewrite(&opened.entries)
        .expect("the rewrite succeeds");
    assert!(
        names(&root).iter().all(|name| !name.contains(".tmp-")),
        "no temporary file survives: {:?}",
        names(&root)
    );
    assert_eq!(mode(&journal), 0o600, "a rewrite keeps the journal private");
    drop(store);
    assert!(!lock.exists(), "the lock is released with the store");
}

#[test]
fn a_journal_header_naming_another_id_is_refused_without_touching_another_file() {
    let dir = TempDir::new("id-mismatch");
    let id = SessionId::new("real").expect("a valid session id");
    // The file is `real.jsonl`, but its header claims to be `other`.
    let header = r#"{"type":"session","version":1,"id":"other","timestamp":1}"#;
    let entry = r#"{"type":"message","id":"e-1","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one"}]}}"#;
    dir.write_journal(&id, format!("{header}\n{entry}\n").as_bytes());
    let before = dir.journal_text(&id);

    let error = Session::open(JsonlFileStore::new(dir.path()), &id).expect_err("the id mismatches");
    assert!(
        matches!(error, SessionError::MismatchedJournalId),
        "{error:?}"
    );
    assert_eq!(
        dir.journal_text(&id),
        before,
        "the requested journal is untouched"
    );
    assert!(
        !dir.path().join("other.jsonl").exists(),
        "no other journal is created"
    );
    assert!(
        !dir.path().join("other.jsonl.lock").exists(),
        "no other journal is locked"
    );
    assert!(
        !dir.lock(&id).exists(),
        "the requested lock is released again"
    );
}

#[test]
fn an_unsupported_version_is_refused_before_any_repair_or_entry_interpretation() {
    let dir = TempDir::new("version-early");

    // (a) A version-2 journal whose second line is a complete but malformed entry: it must be
    // reported as an unsupported version, not as corruption, and must not be rewritten.
    let malformed = SessionId::new("v2-malformed").expect("a valid session id");
    let malformed_bytes = b"{\"type\":\"session\",\"version\":2,\"id\":\"v2-malformed\",\"timestamp\":1}\n{\"type\":\"message\",\"id\":\n";
    dir.write_journal(&malformed, malformed_bytes);
    let error = Session::open(JsonlFileStore::new(dir.path()), &malformed)
        .expect_err("the version is unsupported");
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
    assert_eq!(
        dir.journal_text(&malformed).as_bytes(),
        malformed_bytes,
        "an unsupported journal is left byte-for-byte unchanged"
    );

    // (b) A version-2 journal with a torn tail: it must not be quarantined or repaired.
    let torn = SessionId::new("v2-torn").expect("a valid session id");
    let torn_bytes = b"{\"type\":\"session\",\"version\":2,\"id\":\"v2-torn\",\"timestamp\":1}\n{\"type\":\"message\"";
    dir.write_journal(&torn, torn_bytes);
    let error = Session::open(JsonlFileStore::new(dir.path()), &torn)
        .expect_err("the version is unsupported");
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
    assert_eq!(
        dir.journal_text(&torn).as_bytes(),
        torn_bytes,
        "the torn unsupported journal is not repaired"
    );
    assert!(
        dir.names_containing(".corrupt-").is_empty(),
        "nothing is quarantined"
    );
    assert!(
        !dir.lock(&torn).exists() && !dir.lock(&malformed).exists(),
        "no lock is left behind"
    );
}

#[test]
fn create_refuses_an_unsupported_header_version_in_both_backends() {
    let dir = TempDir::new("create-version");
    let id = SessionId::new("wrong-version").expect("a valid session id");
    let mut header = SessionHeader::new(id.clone());
    header.version = 2;

    let error = Session::create(JsonlFileStore::new(dir.path()), header.clone())
        .expect_err("the file backend refuses an unsupported version");
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
    assert!(
        !dir.journal(&id).exists(),
        "no journal file is created for an unsupported version"
    );

    let error = Session::create(MemoryStore::new(), header)
        .expect_err("the in-memory backend refuses an unsupported version");
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

#[test]
fn cleanup_leaves_a_user_file_that_only_starts_with_the_temporary_prefix() {
    let dir = TempDir::new("temp-grammar");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user_message("kept"))
        .expect("appended");
    drop(session);

    let generated = dir.path().join(format!("{id}.jsonl.tmp-999-0"));
    let user_file = dir.path().join(format!("{id}.jsonl.tmp-backup"));
    fs::write(&generated, b"crash debris").expect("the debris is written");
    fs::write(&user_file, b"user data").expect("the user file is written");

    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert!(
        !generated.exists(),
        "a file matching the generated grammar is cleaned"
    );
    assert!(
        user_file.exists(),
        "a file that only starts with the prefix is left alone"
    );
    assert_eq!(texts(&reopened), ["kept".to_string()]);
}
