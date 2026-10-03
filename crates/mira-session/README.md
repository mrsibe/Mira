# mira-session

Append-only JSONL session journal with an injectable storage seam for Mira.

Requires Rust 1.88 or newer. This minimum applies to the independently consumed package, not
just the desktop workspace.

`mira-session` sits next to `mira-agent` in the Mira library stack
(`mira-runtime` → `mira-agent` → `mira-ai`, see
[ADR 0006](../../docs/adr/0006-rust-native-runtime.md)) and owns one thing: the durable log of
a conversation. It has no Tauri, SQLite, keyring, provider, network or application-path
dependency, it never reads the environment, and it never picks a storage location for you.

## Scope

Shipped here (P0):

- The entry model — `SessionHeader`, `Entry`, `EntryKind` — with lossless storage of
  `mira_ai::Message` turns, model and thinking-level changes, usage reports, compaction
  summaries, context edits and labels.
- A `SessionStore` seam with a JSONL file backend (`JsonlFileStore`) and an in-memory backend
  (`MemoryStore`) for tests. Consumers depend on the trait, never on a path.
- `create`, `open` (with an optional recovery notice), `append`, `rewrite`, `active_path`
  (leaf to root through `parent_id`) and the message projection over that path with
  `context_edit` applied.
- Corruption handling: a torn tail is repaired or quarantined, a corrupt middle line, a
  dangling parent, a parent cycle, a duplicate entry id, an oversized entry and an unsupported
  header version are typed errors.

Not shipped here (and deliberately not stubbed):

- **Compaction projection.** A `compaction` entry is accepted, stored and preserved through a
  rewrite, but the projection ignores it: it does not shorten the context yet.
- **Branch switching.** The active branch is the entry most recently appended; there is no API
  to select another leaf or to fork.
- **Structured context edits.** A replacement is text and is projected as a user turn.
- **Provider adapters and transcript transforms**, **desktop integration** (SQLite index,
  migration of existing rows, Tauri commands), **attachments** and **multi-process or remote
  session transport**.

## Quick start

```rust
use mira_session::{EntryId, JsonlFileStore, Session, SessionHeader, SessionId};

let store = JsonlFileStore::new("/path/to/sessions");
let id = SessionId::new("demo")?;
let mut session = Session::create(store, SessionHeader::new(id))?;

let user = session.append_message(mira_ai::Message::User(
    mira_ai::UserMessage::text("hello"),
))?;
session.append_context_edit(user, Some("hello, reshaped".to_string()))?;

// The projection applies the edit; the stored turns never change.
let messages = session.active_messages()?;
# Ok::<(), mira_session::SessionError>(())
```

`examples/offline_journal.rs` (`cargo run -p mira-session --example offline_journal`) writes a
journal to a temporary directory, reloads it from disk and prints the projection. It makes no
network requests.

## Wire shape

One JSON object per line, `snake_case` field names. The first line is the header, every later
line is an entry with a flattened base (`type`, `id`, `parent_id`, `timestamp`) followed by the
fields of its kind:

```json
{"type":"session","version":1,"id":"demo","timestamp":1759478400,"cwd":"/work"}
{"type":"message","id":"e-1","timestamp":1759478401,"message":{"type":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"context_edit","id":"e-2","parent_id":"e-1","timestamp":1759478402,"target_id":"e-1","replacement":"hello, reshaped"}
```

Absent optional fields and unknown extra fields are accepted on read, so an extension inside
the same format version still loads here; a journal whose header names another format version
is rejected rather than interpreted. An unknown **entry kind** is kept as
`EntryKind::Unrecognized` holding the original JSON object so a rewrite preserves it. Field
names are Mira's own, and byte-level interoperability with Pi session files is not a goal.

**What a rewrite preserves.** Unknown entry kinds and unknown **top-level** entry fields
survive a rewrite; unknown fields **nested inside a known payload** (a `mira_ai::Message`, its
provenance, a content block, `usage`) do **not**, and neither do unknown **header** fields. A
payload-shape change therefore requires a `SESSION_VERSION` bump rather than an additive field.
The header version is validated immediately after the header is decoded, before entries are
interpreted and before any repair or rewrite.

## Operations

| Operation                  | Behavior                                                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------- |
| `Session::create`          | New journal; fails with `AlreadyExists` instead of overwriting.                             |
| `Session::open`            | Reloads the journal and takes its single-writer lock; returns an optional `RecoveryNotice`. |
| `Session::append_*`        | One entry per call, durable before it returns; the new entry's parent is the current leaf.  |
| `Session::active_path`     | The active branch, leaf to root, through `parent_id`; validated.                            |
| `Session::active_messages` | Root-to-leaf turns of that branch with `context_edit` applied (omit or replace with text).  |

## Durability and safety

Each rule below is stricter than Pi's JSONL storage, which disables `fsync`, has no
cross-process lock, sets no restrictive permissions and skips malformed lines silently
(see [ADR 0007](../../docs/adr/0007-bounded-pi-parity.md)). Each has a test in `tests/`.

- **Single writer.** Creating or opening a journal takes an exclusive lock file (`<id>.jsonl.lock`)
  next to it, held for the store's lifetime; a second writer gets the typed `Locked` error.
  Rust 1.88 has no `std::fs::File::try_lock`, so the lock is an exclusive-create file that
  records its owner (`LockOwner { pid, timestamp, token }`) rather than an OS advisory lock. It
  is never stolen automatically: a lock left by a crashed process is released only by an
  explicit `break_stale_lock` call, after inspecting `lock_owner`. `break_stale_lock` refuses a
  marker whose recorded owner is the calling process, and a store releases the marker on drop
  only when its own unique token is still recorded, so it cannot delete a replacement writer's
  marker. The tradeoff is deliberate — a marker file cannot distinguish "live writer" from
  "crashed writer", so recovery keeps its documented **no-live-writer precondition** and the
  crate refuses to guess otherwise.
- **Durable writes.** Every append is followed by `fsync` of the file; a create or rewrite is
  followed by `fsync` of the containing directory (unix).
- **Atomic rewrite.** A rewrite writes a temporary file in the same directory and renames it
  over the journal, so no reader can observe a half-written journal. Temporary files are never
  left behind, and one left by a crashed run is removed only while the lock is held. Only the
  exact generated grammar `<id>.jsonl.tmp-<decimal pid>-<hex sequence>` is removed, so that
  prefix is reserved and a user file such as `<id>.jsonl.tmp-backup` is left alone.
- **Restrictive permissions (unix).** The journal directory is created `0700`; the journal, its
  lock file, its quarantine file and its temporary file are created `0600`.
- **Torn tail.** The final fragment is examined while the lock is held. A complete entry
  without a trailing newline is loaded with a `MissingTrailingNewline` notice, and the next
  append terminates it before writing. An incomplete fragment is moved to
  `<id>.jsonl.corrupt-<timestamp>` (a fragment larger than the encoded entry bound counts as
  incomplete) and the good prefix is rewritten atomically, reported as a `QuarantinedTail`
  notice. The quarantine copy is made durable before the journal is replaced and is retained
  if the repair fails, because it may then be the only copy. Nothing is truncated silently;
  `open` does write to the file, but only to repair a torn tail. A journal with an invalid
  header or an unparsable complete entry is rejected and left byte-identical.
- **Middle-file corruption.** A complete line that is not a valid entry fails the open with
  `CorruptLine { line }`: the physical line number, never the line's content.
- **Entry bound.** One encoded entry may not exceed `MAX_ENTRY_BYTES` (8 MiB, roughly a 6 MiB
  image payload once base64 and surrounding text are counted); larger entries are refused with
  `EntryTooLarge` before they are written. The bound is per entry: the whole journal is read
  into memory with `fs::read`, and there is no total-journal or header bound.
- **`parent_id` integrity.** A dangling parent, a parent cycle and a duplicate entry id are
  typed errors at open and at projection time.
- **Version and migration.** A header version this reader does not know is `UnsupportedVersion`,
  checked immediately after the header is decoded and before any repair. `migrate_journal` is
  the idempotent upgrade hook; at v1 it is the identity function, and a future version adds an
  arm there instead of changing a reader.
- **Write failures.** A failed append or rewrite that may have changed the file marks the open
  journal as needing a reopen: later appends and rewrites return `NeedsReopen` until the store
  is dropped and the journal reopened. The lock stays held until `Drop`, and the reopen drives
  the same torn-tail repair.

## Privacy

A journal is private user data: prompts, model output, tool arguments and provider signatures.
This crate never logs anything, never writes message content into an error, and never echoes a
journal line. Credentials are outside the journal's type surface entirely —
`mira_ai::Credential` implements neither `Serialize` nor `Deserialize` and has a redacting
`Debug`.

## Testing

All tests are offline and use only temporary directories created by the test support module
(no new test dependency). Coverage includes every corruption, locking and durability rule
above, the projection rules, `Unrecognized` preservation through a rewrite, the compaction
entry round trip, the migration hook, and the `SessionId` validation rules.

```bash
cargo test -p mira-session                       # unit + integration
cargo clippy -p mira-session --all-targets
cargo run -p mira-session --example offline_journal
cargo package --list -p mira-session             # verify published contents
```

## License

MIT (see `LICENSE`). This crate follows the session-journal format ideas of the MIT-licensed
Pi project; `NOTICE` reproduces Pi's copyright and license. Dependencies keep their own
licenses.
