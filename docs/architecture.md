# Mira Architecture

Mira is a local-first ChatGPT-style desktop client. This is the canonical
architecture document. It has two clearly separated parts:

- **Current architecture (as shipped)** — what the code in this repository
  actually does today.
- **Approved target architecture (not yet implemented)** — the agreed direction
  for a future app-core/runtime split. The phase-1 Pi AI gateway adapter is
  implemented; application coordination and the full App Core split remain pending.
  Its private protocol is recorded in [ADR 0006](adr/0006-pi-ai-sidecar.md).

Product boundaries are defined in [../PRODUCT.md](../PRODUCT.md); the UI
contract is in [../DESIGN.md](../DESIGN.md).

## Current Architecture (As Shipped)

```txt
React UI (presentation)
  │  invoke commands / listen to events
  ▼
Tauri 2 desktop shell (src-tauri, Rust)
  │
  ├── chat.rs        Tauri commands: send_message, conversations, projects,
  │                  memories, model configs, system prompt, cleanup
  ├── model.rs       Mira context assembly and model adapter
  ├── runtime.rs     private JSONL child-process bridge to Pi AI
  ├── memory.rs      memory planner, retrieval, cleanup, sensitive filtering
  ├── secrets.rs     OS credential store access
  ├── database.rs    SQLite schema, migrations, queries, FTS5 indexes
  └── cancellation.rs cancel flag for the in-flight stream
  ├── Local durable state: SQLite file
  ├── Credentials: OS keyring
  └── Pi AI sidecar (runtime/): user-configured OpenAI-compatible provider
```

### Frontend (Presentation)

- React 19 + TypeScript + Tailwind CSS v4 + Zustand, built with Vite.
- `src/components` — app shell, sidebar/conversation list, composer, message
  bubbles, Markdown renderer, window title bar, shared UI primitives.
- `src/pages` — route-level `ChatPage` and `SettingsPage`.
- `src/store` — Zustand store plus streaming and fallback-message helpers.
- `src/core` — Tauri command types (`types.ts`) and the `invoke` wrappers
  (`tauriClient.ts`).
- `src/i18n` — lightweight built-in i18n (`en`, `zh`).
- `src/utils` — shared helpers such as `cn` and theme application.

The frontend currently also coordinates optimistic messages and stream listeners
in the Zustand store. Durable app data lives on the Rust side. Moving application
coordination behind an Application API is part of the target split, not a claim
that the current store is already presentation-only.

### Desktop Shell And IPC

- Tauri 2 hosts the webview and the Rust backend. Commands are registered in
  `src-tauri/src/lib.rs`.
- The webview calls commands through `invoke` (see `src/core/tauriClient.ts`).
- The backend pushes updates back to the webview as events:
  - `message_stream_delta` — streaming assistant content and reasoning deltas.
  - `memories_changed` — emitted after a background memory pass writes facts.

### Backend Modules

| Module            | Responsibility                                                               |
| ----------------- | ---------------------------------------------------------------------------- |
| `chat.rs`         | Tauri command handlers for chat, conversations, projects, memories, settings |
| `model.rs`        | Mira context construction and Pi runtime adapter                             |
| `runtime.rs`      | Private versioned JSONL, bounded process I/O, cancellation and child cleanup |
| `memory.rs`       | Memory planner, retrieval, cleanup, sensitive-content filtering              |
| `database.rs`     | SQLite schema, migrations, queries, FTS5 trigram indexes                     |
| `secrets.rs`      | Read/write/delete API keys in the OS credential store                        |
| `cancellation.rs` | Process-wide cancel flag for the active stream                               |
| `types.rs`        | Shared Rust types serialized to the frontend                                 |

### Model Gateway

- Rust prepares context and passes it, with the keyring-resolved credential,
  over private stdin to a per-request standalone Pi AI process (`runtime/`).
- Existing configs still target `{base_url}/chat/completions`. Pi's adapter owns
  HTTP, SSE parsing, reasoning and SDK retries (two retries, 45s HTTP timeout).
- Versioned JSONL text/thinking events are mapped to `message_stream_delta`.
  Rust owns bounded framing, process cleanup and cancellation even while stalled.
- Memory planner inference uses the same bridge; extraction rules and fallback
  heuristics remain Mira's responsibility.
- No provider access from the webview, tools, Pi sessions or credential files.
  Native provider APIs/catalogs and OAuth are deferred. See
  [ADR 0006](adr/0006-pi-ai-sidecar.md) for protocol and packaging constraints.

### Memory

- After a reply is stored, a background pass asks the selected background model
  for structured JSON facts (`memory.rs`). If planner inference fails, it falls
  back to deterministic heuristics.
- Retrieved memories are injected into the system prompt as bounded private
  context. See [memory-system.md](memory-system.md) and
  [adr/0004-bounded-automatic-memory.md](adr/0004-bounded-automatic-memory.md).

### Storage And Credentials

- SQLite file `mira.sqlite3` in the app data directory.
- Tables include `schema_meta`, `projects`, `conversations`, `messages`,
  `model_configs`, `memories`, and `app_settings`.
- `schema_meta.schema_version` tracks the schema version (currently `4`), and a
  separate FTS schema version tracks the full-text index shape.
- Memory retrieval and project-message relevance use SQLite FTS5 virtual tables
  with the `trigram` tokenizer.
- **Current credential writes use the keyring, not SQLite.** New provider saves
  write `model_configs.api_key` as `NULL`; the key lives in the OS credential
  store through `secrets.rs` (service `mira`). Initialization does not migrate or
  clear pre-existing legacy SQLite key values, so old databases may still
  contain credentials and must be treated as sensitive. Provider listings expose a masked indicator such as `******`;
  `get_model_api_key` can return the actual key for explicit settings edits, so
  frontend memory must still be treated as sensitive.
- The webview content security policy limits `connect-src` to the app and the
  updater endpoints; model traffic originates from the bundled Pi sidecar process.

### Current Scope Boundary (MVP)

- Single local user.
- Local conversations, projects, model configs, and memory.
- OpenAI-compatible chat completions.
- SQLite for durable app data.
- System credential storage for API keys.
- No cloud sync, multi-user account system, vector database, or external RAG
  service.

### Data Ownership

- `saved` memories are user-managed. The user can create, edit, and delete them.
- `chat_history` and `project` memories are maintained by the automatic memory
  planner. The user can delete them from settings.
- Conversations can be archived, restored, moved into or out of projects, and
  deleted.
- Project context is retrieved by relevance for the current message instead of
  blindly injecting recent project messages.

## Approved Target Architecture (Not Implemented)

The agreed direction separates presentation, app logic, inference, and native
capabilities into distinct layers:

```txt
Mira UI (React / Zustand presentation)
  │ Application API
  ▼
Mira App Core (conversation, project, memory, settings)
  ├── Pi Runtime (inference, sessions, model/provider adapter)
  └── Native services (SQLite, keyring, filesystem, OS integration)
```

Layer responsibilities and hard boundaries:

- **UI** is presentation only. It renders state and forwards user intent; it
  holds no durable state and owns no inference or session logic.
- **Mira App Core** is the application layer. It coordinates the UI, the
  runtime, and durable storage, and it owns the app's business rules.
- **Pi Runtime** owns inference and session handling. It is **not** the durable
  source of truth: session and inference state are operational, not the
  canonical record.
- **Native services** provide desktop capabilities (window chrome, updater,
  credential store, filesystem). The Tauri native layer is the desktop shell,
  **not** the future orchestration layer.
- **Durable source of truth** remains local storage: the SQLite database for app
  data and the OS keyring for credentials. SQLite continues to hold no API keys.
- **Dependency direction:** UI → Application API → App Core. App Core uses
  separate runtime and native-service adapters; native persistence is not a
  downstream stage of the model loop. UI must not access SQLite or providers
  directly, Pi must not own Mira's canonical persistence, and Tauri must not
  become the future agent orchestration layer.
- **Pi reuse scope:** evaluate `pi-ai` → `pi-agent-core` → AgentSession APIs and
  the extension/skill ecosystem behind Mira's runtime adapter. Dependencies,
  versions, desktop packaging, permissions, cancellation and events require
  a separate integration design. Reusing that ecosystem does not authorize
  tools, file editing or autonomous workflows in the product.

This section defines the full target's layer responsibilities, not a claim that
application coordination has moved out of Rust/Zustand. The gateway migration's
process model and private protocol are defined in [ADR 0006](adr/0006-pi-ai-sidecar.md).
Provider catalogs/OAuth and the full Application API/App Core remain pending.

## Frontend Structure Reference

- `src/components` contains shared app UI such as the shell, sidebar, composer,
  and message renderer.
- `src/pages` contains route-level pages.
- `src/core` contains Tauri command types and invoke wrappers.
- `src/store` contains Zustand state and store helpers.

## Related Documents

- [../PRODUCT.md](../PRODUCT.md) — what Mira is and is not.
- [../DESIGN.md](../DESIGN.md) — design, layout, accessibility, error, and
  cancellation contract.
- [memory-system.md](memory-system.md) — memory types and flows.
- [project-context.md](project-context.md) — project context retrieval.
- [security.md](security.md) — credential and destructive-action notes.
- [engineering.md](engineering.md) — build, CI, and operational plan.
- [adr/](adr/) — architecture decision records.
