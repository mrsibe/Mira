# Pi Parity Plan

**Status: accepted direction, nothing implemented yet.**
[ADR 0007](adr/0007-bounded-pi-parity.md) records the decision that limits this
plan to library parity plus bounded client capabilities. Phases P0, P0.5, P1,
P2 and P3 are approved work; P4 (platform capabilities) and P5 (additional
clients) are out of scope and need a new ADR. Sections 1–4 are investigation;
sections 5–7 are the approved shape and its risks. Nothing in this document
describes shipped behavior.

The goal: make Mira a Pi-like client in Rust, roughly "Pi reimplemented in
Rust, plus `mira-desktop` as the client" — within the bounded scope above.

## 1. Verified reference baseline

Pi is 13 packages and about 170,000 lines of TypeScript in `src` (611 test
files), at revision `a276dabe5`. Mira's library stack is about 8,300 lines of
Rust source plus 7,600 lines of Rust tests.

| Pi package                                 | src lines      | Role                                                                 |
| ------------------------------------------ | -------------- | -------------------------------------------------------------------- |
| `coding-agent`                             | 84,918         | Product layer: session, tools, extensions, skills, CLI/TUI/RPC modes |
| `ai`                                       | 26,305         | LLM API, provider adapters, message contracts, model catalog         |
| `tui`                                      | 19,277         | Terminal rendering library (components, differential rendering)      |
| `durable`                                  | 17,662         | Durable conversation/task/document runtime and storage backends      |
| `chord`                                    | 8,817          | Replicated-state/RPC runtime underneath protocol/client/server       |
| `mcp`                                      | 3,167          | MCP client                                                           |
| `agent`                                    | 2,513          | Agent loop, state, queues, transport abstraction                     |
| `server` / `codemode` / `client` / `evals` | 0.9k–2.5k each | Session router, sandboxed tool execution, client, evals              |
| `protocol` / `telemetry`                   | 869 / 935      | CBOR session protocol, telemetry contracts                           |

Existing Mira equivalents:

| Mira                            | src lines | Closest Pi layer                             |
| ------------------------------- | --------- | -------------------------------------------- |
| `crates/mira-ai`                | 4,257     | `ai` (single OpenAI-compatible adapter only) |
| `crates/mira-agent`             | 2,080     | `agent`                                      |
| `crates/mira-runtime`           | 1,978     | part of the session glue                     |
| `apps/mira-desktop/src-tauri`   | 4,513     | `coding-agent` application host              |
| `apps/mira-desktop/src` + tests | 5,294     | the client UI (no Pi equivalent package)     |

## 2. Correcting the `pi-tui` analogy

`pi-tui` is a **terminal rendering library**, not the client. Its own
`package.json` describes it as a UI library with differential rendering, it
exports components and rendering primitives, and it has no executable entry
point. Pi's client is `coding-agent`, whose interactive mode alone is 21,641
lines.

Pi's shipped embedding surfaces are the in-process SDK and the JSONL RPC/print
modes. The CBOR `protocol` + `client` + `server` packages are **experimental**:
they are `devDependencies` of `coding-agent` and referenced only from
`src/experimental/*`.

So the honest mapping is:

- `mira-desktop` corresponds to Pi's `coding-agent` application host.
- A hypothetical reusable UI/client library would correspond to `pi-tui`.
  Mira has no such split today: the React components, the Zustand store and
  the Tauri IPC client all live inside the app.
- Pi's remote/multi-client transport has no Mira equivalent, and Pi itself does
  not treat it as stable.

## 3. What "Pi parity" would actually require

Port/reuse candidates, ordered by value per unit of risk:

**Worth porting (library-shaped, no product-boundary change)**

1. Transcript transform invariants from `ai/utils/transform-messages.ts`:
   preserve thinking signatures for the same model, strip them across models,
   synthesize tool results for orphaned tool calls, drop failed/aborted
   assistant turns. Mira has the fields but not the replay rules.
2. The JSONL session journal with `id`/`parentId` entries, version header and
   idempotent migration (`coding-agent`'s `session-manager.ts`, 2,013 lines),
   plus Pi's atomic-commit storage spine from `durable`.
3. Additional provider adapters (`openai-responses`, then
   `anthropic-messages`) with signature replay, so `mira-ai` is not
   OpenAI-completions only.
4. Context compaction and branch summarization
   (`coding-agent/core/compaction`, 1,671 lines) once the session model exists.

**Later, only with a decided trust model**

5. Declarative content layers: prompt templates and skills are text, and are
   the cheap half of Pi's extension story.
6. MCP as an explicit, user-configured tool provider.

**Deliberately out of scope**

7. `coding-agent/core/extensions` (4,959 lines) and `core/tools` (4,584):
   Pi loads extensions with `jiti` in-process, with **full OS permissions and
   no sandbox**, and project trust only decides whether protected resources are
   loaded, not what they may do. Shipping anything comparable in Mira requires a
   real capability/trust/data-access/install design first.
8. `codemode` (sandboxed JavaScript execution), `chord`, `evals`, the OAuth
   family, and vendor-specific adapters (Bedrock, Azure, Copilot, Cloudflare).
9. A line-by-line `pi-tui` port. A Rust TUI would be written against
   `ratatui`/`crossterm`; Pi's native clipboard/Kitty-image/keyboard extensions
   are low-value for a GUI-first product.

## 4. Target layout

Keeping the existing crate boundaries (which the reviews accepted) and adding
one new layer for durability:

```txt
apps/mira-desktop            client application: Tauri shell + React UI + SQLite indexes
  -> mira-runtime            operational session composition (no IO, unchanged boundary)
       -> mira-agent         agent loop, validated tools, cancellation repair
            -> mira-ai       message/provider/stream contracts + adapters
       -> mira-session       NEW: session journal, entry records, storage seam
```

`mira-session` owns the append-only entry model, a `SessionStore` trait, and a
JSONL backend. This must not go into `mira-runtime`, whose documented contract
is explicitly "no durable state, no queue". Pi makes the same split: `agent`
stays small (2,513) and durability lives in `durable` (17,662).

`mira-runtime` consumes an injected transcript store rather than owning files,
preserving the rule that libraries take no SQLite, keyring or application paths.

Multi-client transport (`mira-protocol`/`mira-client`) is **not** part of this
plan. In-process clients sharing one runtime cover the webview today. The
existing architecture target (Application API / App Core) is the cheaper route
if multiple in-process frontends appear.

## 5. JSONL session journal design

Following Pi's format, with the safety gaps fixed rather than copied.

**Format.** First line is a session header
(`{type:"session",version,id,timestamp,cwd,parent_session?}`), then one JSON
object per line with a common base (`type,id,parent_id,timestamp`). Initial
entry types: `message`, `model_change`, `thinking_level_change`, `usage`,
`compaction`, `context_edit`, `label`. Messages carry structured content
blocks, the producing model, stop reason and reported usage, so replay is
lossless.

**Two guarantees to adopt from Pi.**

- Append-only history: compaction and context edits add entries; they never
  rewrite or delete earlier messages. The model sees a projection of the
  active branch, not every stored branch.
- Versioned header with idempotent migration on load.

**Six things not to copy.**

- No `fsync` today; Pi's JSONL storage also defaults `fsync` off.
- No file lock, while rewrites truncate in place, so concurrent writers can
  interleave or destroy data.
- No explicit `0600` file / `0700` directory permissions.
- Malformed middle lines are silently skipped; a corrupted turn can vanish.
- The active leaf is derived from "the last physical line", so any trailing
  metadata entry silently becomes the resume point.
- A read path appends a newline to repair a torn tail, which can race a writer.

**Mira rules.** Single writer per session, guarded by an exclusive marker lock
file: Rust 1.88 has no advisory file lock, so the marker is created
exclusively, records its owner, is released on drop, and a marker left by a
crashed writer is released only by an explicit recovery call that first checks
the recorded owner. Appends are followed by `fsync`; rewrites go through a
temp file that is `fsync`ed before `rename`, followed by a directory `fsync`.
Files are explicitly restrictive (directory `0700`, files `0600`). A torn tail
is quarantined and the quarantine copy is made durable _before_ the journal is
replaced, and is retained if the repair fails; a valid final entry that merely
lacks its newline is kept and terminated. Middle-file corruption is an error,
not a skipped line. The parent graph is validated on load; the active leaf is
the last appended entry, and projection selects conversation turns only, so a
metadata entry appended through a session cannot change what the model sees. A
journal loaded from elsewhere is not covered by that guarantee: an arbitrary
trailing entry with another parent can select another branch, so an explicit
leaf or branch pointer is deferred to P3, when branching exists.

**Single source of truth.** The JSONL journal becomes canonical for
conversation content. SQLite keeps projects, memories, settings, provider
metadata and credentials-by-reference, and it may keep a _rebuildable_ index
(conversation list, titles, archive flags, FTS) derived from the journal. The
current `messages` table must not stay a second authority for content; the
migration exports existing rows into journals without deleting the originals
and marks legacy assistant turns as text-only rather than inventing reasoning.

### 5.1 P0 contract

`mira-session` (new crate, Rust 1.88, MIT, no Tauri/SQLite/keyring/app path
dependency):

- Types: `SessionId`, `EntryId`, `SessionHeader`, `Entry`, `EntryKind`,
  `SessionError`.
- `EntryKind`: the entry types above plus `Unrecognized(Value)`, so a reader
  that meets a newer entry kind preserves it instead of dropping it.
- Storage seam: a `SessionStore` trait with a JSONL file backend and an
  in-memory backend for tests. Consumers depend on the trait, never on a
  concrete path.
- Operations: `create`, `open` (entries plus an optional recovery notice),
  `append`, `active_path` (leaf to root through `parent_id`), and a message
  projection over that path with `context_edit` applied.
- Field names are Mira's own `snake_case`; the log version is explicit and
  migration is idempotent. Byte-level interoperability with Pi session files is
  not a goal and must not be claimed.
- Serializing the `mira-ai` protocol types is part of P0 because the journal
  stores them. `Credential` must never become serializable.
- Compatibility rule, stated precisely because the implementation cannot
  preserve more: unknown fields are tolerated on read; unknown entry kinds and
  unknown top-level entry fields survive a rewrite; unknown fields nested inside
  a known payload (a message, its provenance, a content block, usage) do not.
  Therefore any change to the payload shape requires a `SESSION_VERSION` bump,
  and an unknown version is rejected immediately after the header is decoded,
  before entries are interpreted and before any repair or rewrite. A reader
  never rewrites a journal whose version it does not understand.

### 5.2 P0.5 desktop integration and migration

The desktop keeps its current behavior while the journal becomes the content
source of truth. Nothing is deleted at any step.

**Layout.** One journal per conversation under the application data directory,
`sessions/<conversation-id>.jsonl`, with directory `0700` and files `0600`. The
identifier is already a generated id, and `mira-session` rejects path
separators, `..` and control characters, so an id cannot escape the directory.
Pi groups sessions by working directory; Mira does not, because a conversation
is not bound to a workspace.

**What moves and what stays.** The journal holds the conversation content: user
and assistant turns in order, model and thinking-level changes, reported usage,
and later compaction and context edits. SQLite keeps projects, memories,
settings, provider metadata and credentials-by-reference, and keeps the
`conversations` row as an index entry (title, archive flag, timestamps,
project) plus a rebuildable `messages`/`messages_fts` index.

**Dual write is temporary.** The sequence is: write the journal first, then
update the index; add a rebuild-from-journal path that reproduces the index;
switch reads to journal plus index; keep the legacy tables until one release
has passed verification. Content is never written to the index first, so the
index can only lag the journal, never lead it. A lagging index is reported and
rebuilt, not repaired by hand.

**No behavior change in this phase.** Legacy assistant turns carry text and,
for display, their stored reasoning. They are recorded with unknown-model
provenance and, exactly as today, the model-visible projection for them stays
text-only: a thinking block without a provider signature is never replayed.
Signature-aware replay arrives in P2, not here. The prompt assembly, the
message window and the memory flow stay as they are until P1 changes them
deliberately.

**Migration.** A dry run reports, per conversation, how many turns would be
written and what cannot be represented losslessly; the user sees the summary
before anything is written. Existing rows are exported into journals without
the originals being removed or rewritten, so rollback is "stop reading the
journal", not a data restore. Legacy assistant turns are marked rather than
invented up: no reasoning field is fabricated and no signature is invented.

**Deletion and archiving.** Archiving is an index-only flag. Deleting a
conversation removes its journal file and its index rows together, with the
file deletion attempted first so an interrupted delete leaves an orphan file
that the next scan can report rather than a journal row without a file.

**Failure windows.** A journal append that succeeds while the index write
fails leaves a rebuildable index, reported to the caller. A journal write that
fails leaves the index untouched. Credentials are not part of any of these
steps, and no journal write ever contains a credential, an authorization
header or a request body.

**Attachments.** Images are image blocks inside the journal, bounded by
`MAX_ENTRY_BYTES`. If a future image exceeds that bound, the blob must move to
a content-addressed file next to the journal with only a reference stored; the
journal must not silently truncate or drop an attachment.

**Acceptance.** Reopening a conversation after a restart reproduces it exactly;
a cancelled turn stores only the user message; a journal survives an abrupt
process kill with at most the last append lost; a corrupted tail is quarantined
and reported; no secret appears in any journal; the legacy tables remain intact
and sufficient for rollback.

## 6. Phased roadmap

Each phase is independently shippable and reversible. P0–P3 are the approved
direction; P4 and P5 are excluded by
[ADR 0007](adr/0007-bounded-pi-parity.md).

**P0 — Session journal foundation.** Two parts, in order: (a) lossless
serialization derives for the `mira-ai` protocol types with round-trip tests;
(b) the new `mira-session` crate per §5.1 with corruption handling and recovery
tests. No desktop or SQLite changes in this phase.

**P0.5 — Desktop integration and migration.** Journal the desktop's
conversations, derive the SQLite indexes from the journal, migrate existing rows
without deleting them, and update [PRODUCT.md](../PRODUCT.md) and
[architecture.md](architecture.md) to describe the shipped behavior.
Acceptance: reopening a conversation after restart reproduces it; cancelled
turns store the user message only; no secret reaches a journal; the legacy
tables remain intact until verification passes.

**P1 — Lossless messages end to end.** Structured content blocks, model
provenance, stop reason and usage persisted and replayed, plus the transcript
transform invariants from §3.1 applied at the model boundary with tests.

**P2 — Provider breadth and signature replay.** `openai-responses`, then
`anthropic-messages`, with same-model signature replay and cross-model
stripping, plus a compat switch so no existing endpoint receives new fields.

**P3 — Compaction and branching.** Summary entries with a retained boundary,
projection-aware context building, and regeneration/branch UI on top. This is
what turns the journal into a real Pi-style session.

**P4 (out of scope, requires a new ADR) — Platform capabilities.** Tools,
extensions, skills, MCP. Requires a capability model, a trust boundary that
governs _actions_ (not just loading), an isolation decision, and an install
story. Excluded by [ADR 0005](adr/0005-extension-boundary.md) and
[ADR 0007](adr/0007-bounded-pi-parity.md).

**P5 (out of scope) — Additional clients.** JSONL RPC mode for headless use, or
a `ratatui` client. Revisit only if a real need appears.

Rough effort, from the ported line counts: P0, P0.5 and P1 are each a few
thousand lines of Rust including tests; P2 is 3,000–5,000; P3 is 1,500–2,500;
P4 is 5,000+ before any trust model, and P5 is 10,000+ if a TUI is written.
Full parity with Pi's 170,000 lines is not a realistic target, and the largest
Pi package is the one ADR 0007 excludes.

## 7. Risks

- Scope: the rejected half of Pi is also its most distinctive half; "Pi-like"
  without tools is really "Pi's session and provider stack".
- Signature/replay bugs cause multi-turn provider failures and are spread
  across encode, decode and transform.
- Cross-medium atomicity: journal, SQLite index and keyring writes cannot be
  one transaction; each failure window must be stated and tested.
- Migration: user data exists today in SQLite; any journal rollout needs a
  dry-run, verification and a preserved copy.
- Pi's own durability defaults (no fsync, no lock) are not a quality bar to
  inherit; copying the format must not mean copying the guarantees.

## 8. Explicit non-goals

No vector search or RAG. No tools, extensions, subagents or MCP. No cloud sync,
accounts or multi-user server. No line-by-line `pi-tui` port and no terminal
client. No `chord`/CBOR remote protocol until a real multi-process need exists.

## 9. Decisions recorded

1. **Product boundary.** Bounded parity: Pi-parity libraries plus a client with
   sessions, compaction, branching and attachments; no tools, extensions,
   subagents or MCP. Recorded in [ADR 0007](adr/0007-bounded-pi-parity.md);
   [ADR 0005](adr/0005-extension-boundary.md) stays in force.
2. **Session truth.** JSONL is canonical for conversation content; SQLite keeps
   application data and a rebuildable index.

Still open, and not blocking P0–P3:

3. **Client structure.** Keep one application, or split a reusable UI/client
   library out of `mira-desktop` (the `pi-tui` role)?
4. **Additional clients.** Desktop-only for now; P5 is out of scope.
5. **Provider scope.** Which native adapters to add after
   `openai-responses`/`anthropic-messages` is chosen during P2.
