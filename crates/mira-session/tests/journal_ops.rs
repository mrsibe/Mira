//! Journal operations: record kinds, the active path, the projection, and the wire shape.

mod support;

use mira_ai::{
    Api, AssistantContent, AssistantMessage, AssistantSource, InputContent, Message,
    ReasoningLevel, StopReason, TextBlock, ThinkingBlock, ToolCall, ToolResultMessage, Usage,
};
use mira_session::{
    Entry, EntryId, EntryKind, JsonlFileStore, MemoryStore, Session, SessionError, SessionHeader,
    SessionId, SessionStore,
};
use serde_json::json;

use support::{assistant_message, message_text, user_message, TempDir};

fn session_id() -> SessionId {
    SessionId::new("ops").expect("a valid session id")
}

#[test]
fn header_wire_shape_is_stable() {
    let id = SessionId::new("demo").expect("a valid session id");
    let header = SessionHeader::at(id, 1_759_478_400);
    assert_eq!(
        serde_json::to_string(&header).expect("the header serializes"),
        r#"{"type":"session","version":1,"id":"demo","timestamp":1759478400}"#
    );

    let header = header.with_cwd("/work");
    assert_eq!(
        serde_json::to_string(&header).expect("the header serializes"),
        r#"{"type":"session","version":1,"id":"demo","timestamp":1759478400,"cwd":"/work"}"#
    );

    let parent = SessionId::new("demo-parent").expect("a valid session id");
    let header = header.with_parent_session(parent);
    assert_eq!(
        serde_json::to_string(&header).expect("the header serializes"),
        r#"{"type":"session","version":1,"id":"demo","timestamp":1759478400,"cwd":"/work","parent_session":"demo-parent"}"#
    );

    // Absent optional fields load as `None`.
    let restored: SessionHeader = serde_json::from_str(support::HEADER).expect("the header loads");
    assert_eq!(restored.id.as_str(), "raw");
    assert_eq!(restored.version, 1);
    assert_eq!(restored.cwd, None);
    assert_eq!(restored.parent_session, None);
}

#[test]
fn entry_wire_shape_is_stable() {
    let entry = Entry::new(
        EntryId::new("e-1"),
        None,
        1_759_478_401,
        EntryKind::Message {
            message: user_message("hello"),
        },
    );
    assert_eq!(
        serde_json::to_string(&entry).expect("the entry serializes"),
        r#"{"type":"message","id":"e-1","timestamp":1759478401,"message":{"type":"user","content":[{"type":"text","text":"hello"}]}}"#
    );

    let child = Entry::new(
        EntryId::new("e-2"),
        Some(EntryId::new("e-1")),
        1_759_478_402,
        EntryKind::ContextEdit {
            target_id: EntryId::new("e-1"),
            replacement: Some("hello, reshaped".to_string()),
        },
    );
    assert_eq!(
        serde_json::to_string(&child).expect("the entry serializes"),
        r#"{"type":"context_edit","id":"e-2","parent_id":"e-1","timestamp":1759478402,"target_id":"e-1","replacement":"hello, reshaped"}"#
    );
}

#[test]
fn every_metadata_kind_round_trips() {
    let kinds = vec![
        EntryKind::ModelChange {
            provider: "deepseek".to_string(),
            model: "deepseek-reasoner".to_string(),
        },
        EntryKind::ThinkingLevelChange {
            level: ReasoningLevel::High,
        },
        EntryKind::Usage {
            kind: "turn".to_string(),
            provider: "deepseek".to_string(),
            model: "deepseek-reasoner".to_string(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                cached_input_tokens: Some(3),
                reasoning_tokens: Some(4),
            },
        },
        EntryKind::Compaction {
            summary: "earlier turns summarized".to_string(),
            first_kept_entry_id: EntryId::new("e-1"),
            tokens_before: 512,
        },
        EntryKind::ContextEdit {
            target_id: EntryId::new("e-1"),
            replacement: None,
        },
        EntryKind::Label {
            target_id: EntryId::new("e-1"),
            label: Some("attachment question".to_string()),
        },
    ];

    for (index, kind) in kinds.into_iter().enumerate() {
        let entry = Entry::new(
            EntryId::new(format!("e-{index}")),
            Some(EntryId::new("e-0")),
            42,
            kind,
        );
        let json = serde_json::to_string(&entry).expect("the entry serializes");
        let restored: Entry = serde_json::from_str(&json).expect("the entry deserializes");
        assert_eq!(restored, entry, "round trip changed {json}");
    }
}

#[test]
fn a_stored_assistant_turn_round_trips_losslessly() {
    let dir = TempDir::new("lossless");
    let id = session_id();
    let message = Message::Assistant(AssistantMessage {
        source: AssistantSource {
            api: Api::OpenAiCompletions,
            provider: "example-provider".to_string(),
            model: "example-model".to_string(),
            response_model: Some("example-model-2026-01-01".to_string()),
            response_id: Some("resp_1".to_string()),
        },
        content: vec![
            AssistantContent::Text(TextBlock::new("visible")),
            AssistantContent::Thinking(ThinkingBlock {
                thinking: "reasoning".to_string(),
                signature: Some("sig-abc".to_string()),
                redacted: true,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "lookup".to_string(),
                arguments_raw: "{\"q\":\"mir".to_string(),
                arguments: None,
                signature: Some("call-sig".to_string()),
            }),
        ],
        stop_reason: StopReason::ToolUse,
        raw_stop_reason: Some("tool_calls".to_string()),
        usage: Some(Usage {
            input_tokens: 1,
            output_tokens: 2,
            cached_input_tokens: Some(3),
            reasoning_tokens: Some(4),
        }),
        error_message: None,
    });

    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(message.clone())
        .expect("the turn is appended");
    drop(session);

    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(
        reopened.active_messages().expect("the path resolves"),
        [message]
    );
}

#[test]
fn image_parts_and_tool_results_round_trip() {
    let dir = TempDir::new("parts");
    let id = session_id();
    let user = Message::User(mira_ai::UserMessage {
        content: vec![
            InputContent::text("what is this?"),
            InputContent::image("image/png", "aGVsbG8="),
        ],
    });
    let result = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "lookup".to_string(),
        content: vec![
            InputContent::text("result"),
            InputContent::image("image/jpeg", "d29ybGQ="),
        ],
        is_error: true,
    });

    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    session
        .append_message(user.clone())
        .expect("the turn is appended");
    session
        .append_message(result.clone())
        .expect("the result is appended");
    drop(session);

    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(
        reopened.active_messages().expect("the path resolves"),
        [user, result]
    );
}

#[test]
fn active_path_walks_parent_links_leaf_to_root() {
    let mut session = Session::create(MemoryStore::new(), SessionHeader::new(session_id()))
        .expect("the journal is created");
    let first = session
        .append_message(user_message("one"))
        .expect("appended");
    let second = session
        .append_message(assistant_message("p", "m", "two"))
        .expect("appended");
    let third = session
        .append_message(user_message("three"))
        .expect("appended");

    let path = session.active_path().expect("the path resolves");
    let ids: Vec<&str> = path.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, [third.as_str(), second.as_str(), first.as_str()]);
    assert_eq!(session.leaf_id(), Some(&third));
    assert_eq!(session.entries().len(), 3);
}

#[test]
fn the_projection_applies_context_edits_in_order() {
    let mut session = Session::create(MemoryStore::new(), SessionHeader::new(session_id()))
        .expect("the journal is created");
    let user = session
        .append_message(user_message("original"))
        .expect("appended");
    let assistant = session
        .append_message(assistant_message("p", "m", "answer"))
        .expect("appended");

    // Nothing edited yet: both turns are projected.
    assert_eq!(
        texts(&session),
        ["original".to_string(), "answer".to_string()]
    );

    // A replacement keeps the position, a text-only edit becomes a user turn.
    session
        .append_context_edit(user.clone(), Some("reshaped".to_string()))
        .expect("the edit is appended");
    assert_eq!(
        texts(&session),
        ["reshaped".to_string(), "answer".to_string()]
    );

    // Omission removes the turn from the projection without deleting the entry.
    session
        .append_context_edit(user, None)
        .expect("the edit is appended");
    assert_eq!(texts(&session), ["answer".to_string()]);
    assert_eq!(session.entries().len(), 4);

    // A label is stored, and never changes the projected turns.
    let label = session
        .append_label(assistant, Some("kept".to_string()))
        .expect("the label is appended");
    assert_eq!(texts(&session), ["answer".to_string()]);
    assert_eq!(
        session.active_path().expect("the path resolves").len(),
        5,
        "the label is on the active branch even though it is not a turn"
    );
    assert!(session
        .active_path()
        .expect("the path resolves")
        .iter()
        .any(|entry| entry.id == label));
}

#[test]
fn the_projection_follows_the_active_branch_only() {
    // A hand-written journal with a fork: `e-2` hangs off `e-1` but the leaf `e-3` does not
    // descend from it, so `e-2` and the edit that targets it are not projected.
    let dir = TempDir::new("branch");
    let id = SessionId::new("raw").expect("a valid session id");
    let journal = [
        support::HEADER,
        r#"{"type":"message","id":"e-1","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"root"}]}}"#,
        r#"{"type":"message","id":"e-2","parent_id":"e-1","timestamp":3,"message":{"type":"user","content":[{"type":"text","text":"other branch"}]}}"#,
        r#"{"type":"message","id":"e-3","parent_id":"e-1","timestamp":4,"message":{"type":"user","content":[{"type":"text","text":"leaf"}]}}"#,
        r#"{"type":"context_edit","id":"e-4","parent_id":"e-3","timestamp":5,"target_id":"e-2","replacement":null}"#,
    ]
    .join("\n");
    dir.write_journal(&id, format!("{journal}\n").as_bytes());

    let session = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let path = session.active_path().expect("the path resolves");
    let ids: Vec<&str> = path.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, ["e-4", "e-3", "e-1"]);
    assert_eq!(
        texts(&session),
        ["root".to_string(), "leaf".to_string()],
        "the off-branch turn and the edit that targets it are not projected"
    );
    assert_eq!(session.entries().len(), 4);
}

#[test]
fn compaction_is_stored_and_preserved_but_not_projected() {
    // P0 accepts a compaction entry and never drops it; applying it to the context is a later
    // phase, so the projected turns are unchanged.
    let dir = TempDir::new("compaction");
    let id = session_id();
    let mut session = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect("the journal is created");
    let first = session
        .append_message(user_message("old turn"))
        .expect("appended");
    session
        .append_message(assistant_message("p", "m", "old answer"))
        .expect("appended");
    let before = texts(&session);

    session
        .append_compaction("summary of the old turns", first.clone(), 1234)
        .expect("the compaction is appended");
    assert_eq!(texts(&session), before);
    drop(session);

    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    let compaction = reopened
        .entries()
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Compaction {
                summary,
                first_kept_entry_id,
                tokens_before,
            } => Some((summary.clone(), first_kept_entry_id.clone(), *tokens_before)),
            _ => None,
        })
        .expect("the compaction entry is preserved");
    assert_eq!(
        compaction,
        ("summary of the old turns".to_string(), first, 1234)
    );
    assert_eq!(texts(&reopened), before);
}

#[test]
fn an_unrecognized_entry_kind_survives_a_rewrite() {
    let dir = TempDir::new("unrecognized");
    let id = SessionId::new("raw").expect("a valid session id");
    let future = r#"{"type":"future_kind","id":"e-2","parent_id":"e-1","timestamp":3,"payload":{"a":[1,2]},"note":"kept"}"#;
    let journal = format!(
        "{}\n{}\n{future}\n",
        support::HEADER,
        raw_user_entry("e-1", None, 2, "one")
    );
    dir.write_journal(&id, journal.as_bytes());

    let mut store = JsonlFileStore::new(dir.path());
    let opened = store.open(&id).expect("the journal opens");
    assert_eq!(opened.entries.len(), 2);
    let unrecognized = opened.entries[1].clone();
    let EntryKind::Unrecognized(value) = &unrecognized.kind else {
        panic!("a future entry kind is preserved as Unrecognized");
    };
    assert_eq!(
        value,
        &serde_json::from_str::<serde_json::Value>(future).expect("the fixture is valid JSON")
    );
    assert!(
        unrecognized.extra.is_empty(),
        "the whole object is kept once"
    );

    store
        .rewrite(&opened.entries)
        .expect("the rewrite succeeds");
    drop(store);

    let rewritten = dir.journal_text(&id);
    let future_value: serde_json::Value =
        serde_json::from_str(future).expect("the fixture is valid JSON");
    let preserved = rewritten
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .any(|value| value == future_value);
    assert!(
        preserved,
        "the unknown entry is written back with all of its fields: {rewritten}"
    );
    let reopened = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(reopened.entries().len(), 2);
    assert_eq!(reopened.entries()[1].kind, unrecognized.kind);
}

#[test]
fn unknown_extra_fields_on_a_known_kind_survive_a_rewrite() {
    let dir = TempDir::new("extra-fields");
    let id = SessionId::new("raw").expect("a valid session id");
    let extended = r#"{"type":"message","id":"e-1","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one"}]},"future_field":{"nested":true}}"#;
    dir.write_journal(&id, format!("{}\n{extended}\n", support::HEADER).as_bytes());

    let mut store = JsonlFileStore::new(dir.path());
    let opened = store.open(&id).expect("the journal opens");
    assert_eq!(opened.entries.len(), 1);
    assert_eq!(
        opened.entries[0].extra.get("future_field"),
        Some(&json!({ "nested": true }))
    );

    store
        .rewrite(&opened.entries)
        .expect("the rewrite succeeds");
    drop(store);
    let rewritten = dir.journal_text(&id);
    assert!(
        rewritten.contains(r#""future_field":{"nested":true}"#),
        "unknown extra fields are preserved: {rewritten}"
    );
}

#[test]
fn a_minimal_entry_loads_with_its_optionals_absent() {
    let dir = TempDir::new("minimal");
    let id = SessionId::new("raw").expect("a valid session id");
    let minimal = [
        support::HEADER,
        r#"{"type":"message","id":"e-1","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one"}]}}"#,
        r#"{"type":"thinking_level_change","id":"e-2","parent_id":"e-1","timestamp":3,"level":"high"}"#,
        r#"{"type":"context_edit","id":"e-3","parent_id":"e-2","timestamp":4,"target_id":"e-1"}"#,
    ]
    .join("\n");
    dir.write_journal(&id, format!("{minimal}\n").as_bytes());

    let session = Session::open(JsonlFileStore::new(dir.path()), &id).expect("the journal opens");
    assert_eq!(session.entries().len(), 3);
    assert_eq!(
        session.entries()[1].kind,
        EntryKind::ThinkingLevelChange {
            level: ReasoningLevel::High
        }
    );
    assert_eq!(
        session.entries()[2].kind,
        EntryKind::ContextEdit {
            target_id: EntryId::new("e-1"),
            replacement: None,
        }
    );
}

#[test]
fn session_ids_are_validated_before_they_name_a_file() {
    for rejected in [
        "",
        "a/b",
        "a\\b",
        "..",
        "a..b",
        "../escape",
        "a\u{7}b",
        &"x".repeat(mira_session::MAX_SESSION_ID_LEN + 1),
    ] {
        assert!(
            matches!(
                SessionId::new(rejected),
                Err(SessionError::InvalidSessionId)
            ),
            "{rejected:?} must be rejected"
        );
    }
    for accepted in [
        "s-1",
        "0f9a-2b",
        &"x".repeat(mira_session::MAX_SESSION_ID_LEN),
    ] {
        assert!(
            SessionId::new(accepted).is_ok(),
            "{accepted:?} must be accepted"
        );
    }
    let generated = SessionId::generate();
    assert_eq!(
        SessionId::new(generated.as_str()).expect("a generated id is valid"),
        generated
    );
}

#[test]
fn creating_an_existing_journal_never_overwrites_it() {
    let dir = TempDir::new("exists");
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
    let before = dir.journal_text(&id);

    let error = Session::create(
        JsonlFileStore::new(dir.path()),
        SessionHeader::new(id.clone()),
    )
    .expect_err("creating the same id twice fails");
    assert!(matches!(error, SessionError::AlreadyExists), "{error}");
    assert_eq!(dir.journal_text(&id), before);
}

#[test]
fn a_trait_object_is_a_valid_backend() {
    // Consumers depend on the seam, not on a backend: a boxed store works everywhere a
    // concrete one does, and no path is part of the API.
    let store: Box<dyn SessionStore> = Box::new(MemoryStore::new());
    let mut session =
        Session::create(store, SessionHeader::new(session_id())).expect("the journal is created");
    session
        .append_message(user_message("boxed"))
        .expect("appended");
    assert_eq!(texts(&session), ["boxed".to_string()]);
}

#[test]
fn create_open_and_append_require_a_selected_journal() {
    let mut store = MemoryStore::new();
    let entry = Entry::new(
        EntryId::new("e-1"),
        None,
        1,
        EntryKind::Message {
            message: user_message("one"),
        },
    );
    assert!(matches!(
        store.append(&entry).expect_err("no journal is selected"),
        SessionError::NotOpen
    ));
    assert!(matches!(
        store
            .rewrite(&[entry.clone()])
            .expect_err("no journal is selected"),
        SessionError::NotOpen
    ));
    assert!(matches!(
        store
            .open(&SessionId::new("missing").expect("a valid session id"))
            .expect_err("nothing was created"),
        SessionError::NotOpen
    ));

    store
        .create(&SessionHeader::new(session_id()))
        .expect("the journal is created");
    store.append(&entry).expect("the entry is appended");
    store.rewrite(&[]).expect("the rewrite succeeds");
    let opened = store.open(&session_id()).expect("the journal opens");
    assert!(opened.entries.is_empty());
}

fn texts(session: &Session<impl SessionStore>) -> Vec<String> {
    session
        .active_messages()
        .expect("the path resolves")
        .iter()
        .map(message_text)
        .collect()
}

fn raw_user_entry(id: &str, parent: Option<&str>, timestamp: i64, text: &str) -> String {
    let parent = match parent {
        Some(parent) => format!(r#","parent_id":"{parent}""#),
        None => String::new(),
    };
    format!(
        r#"{{"type":"message","id":"{id}"{parent},"timestamp":{timestamp},"message":{{"type":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn session_id_deserialization_validates_like_the_constructor() {
    // The derived transparent `Deserialize` would build a `SessionId` directly, so a validated
    // id and a deserialized id must accept exactly the same strings.
    for rejected in [
        "",
        "a/b",
        "a\\b",
        "..",
        "a..b",
        "../escape",
        "a\u{7}b",
        &"x".repeat(mira_session::MAX_SESSION_ID_LEN + 1),
    ] {
        let json = serde_json::to_string(rejected).expect("a string serializes");
        let error = serde_json::from_str::<SessionId>(&json).expect_err("the id must be rejected");
        let rendered = format!("{error} {error:?}");
        assert!(
            rejected.is_empty() || !rendered.contains(rejected),
            "the serde error must not echo the offending value: {rendered}"
        );
    }

    let valid = SessionId::new("s-1").expect("a valid session id");
    let json = serde_json::to_string(&valid).expect("the id serializes");
    assert_eq!(
        serde_json::from_str::<SessionId>(&json).expect("a valid id round-trips"),
        valid
    );
}

#[test]
fn extra_fields_survive_only_at_the_top_level_pinning_the_nested_limitation() {
    // Characterization test for the documented compatibility rule: a same-version journal that
    // carries unknown fields loads successfully, top-level entry extras survive a rewrite, and
    // nested payload extras and header extras do not.
    let dir = TempDir::new("nested-extras");
    let id = SessionId::new("raw").expect("a valid session id");
    let header =
        r#"{"type":"session","version":1,"id":"raw","timestamp":1,"future_header":"dropped"}"#;
    let entry = r#"{"type":"message","id":"e-1","timestamp":2,"message":{"type":"user","content":[{"type":"text","text":"one","future_block_field":true}],"future_message_field":{"nested":[]}},"future_entry_field":1}"#;
    dir.write_journal(&id, format!("{header}\n{entry}\n").as_bytes());

    let mut store = JsonlFileStore::new(dir.path());
    let opened = store
        .open(&id)
        .expect("a same-version journal with unknown fields loads");
    assert_eq!(opened.entries.len(), 1);
    store
        .rewrite(&opened.entries)
        .expect("the rewrite succeeds");
    drop(store);

    let rewritten = dir.journal_text(&id);
    assert!(
        rewritten.contains("future_entry_field"),
        "unknown top-level entry fields survive a rewrite: {rewritten}"
    );
    assert!(
        !rewritten.contains("future_header"),
        "unknown header fields do not survive a rewrite: {rewritten}"
    );
    assert!(
        !rewritten.contains("future_message_field") && !rewritten.contains("future_block_field"),
        "unknown nested payload fields do not survive a rewrite: {rewritten}"
    );
}

#[test]
fn journal_debug_never_prints_message_content() {
    const SENTINEL: &str = "SENTINEL-private-journal-content";
    let id = session_id();
    let mut store = MemoryStore::new();
    store
        .create(&SessionHeader::new(id.clone()))
        .expect("the journal is created");
    let entry = Entry::new(
        EntryId::new("e-1"),
        None,
        1,
        EntryKind::Message {
            message: user_message(SENTINEL),
        },
    );
    store.append(&entry).expect("the entry is appended");

    let rendered = format!("{entry:?}");
    assert!(
        !rendered.contains(SENTINEL),
        "Entry Debug must redact: {rendered}"
    );
    let rendered = format!("{:?}", entry.kind);
    assert!(
        !rendered.contains(SENTINEL),
        "EntryKind Debug must redact: {rendered}"
    );
    let rendered = format!("{store:?}");
    assert!(
        !rendered.contains(SENTINEL),
        "MemoryStore Debug must redact: {rendered}"
    );

    let opened = store.open(&id).expect("the journal opens");
    let rendered = format!("{opened:?}");
    assert!(
        !rendered.contains(SENTINEL),
        "OpenedJournal Debug must redact: {rendered}"
    );
    drop(opened);

    let session = Session::open(store, &id).expect("the journal opens");
    let rendered = format!("{session:?}");
    assert!(
        !rendered.contains(SENTINEL),
        "Session Debug must redact: {rendered}"
    );
    for entry in session.entries() {
        let rendered = format!("{entry:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "Entry Debug must redact: {rendered}"
        );
    }
}
