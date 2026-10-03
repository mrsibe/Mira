# Mira Architecture

Mira is a local-first ChatGPT-style desktop client. This is the canonical
architecture document. It distinguishes the current implementation from the
remaining Application API / App Core presentation split. The independently
usable Rust AI/Agent/Runtime packages and desktop inference integration are
implemented and reviewed; see [ADR 0006](adr/0006-rust-native-runtime.md).
This describes repository code, not a published release.

Product boundaries are defined in [../PRODUCT.md](../PRODUCT.md); the UI
contract is in [../DESIGN.md](../DESIGN.md).

## Current Architecture (Implemented)

```txt
React UI (presentation)
  │  invoke commands / listen to events
  ▼
Tauri 2 desktop shell (apps/mira-desktop/src-tauri, Rust)
  │
  ├── chat.rs        Tauri commands: send_message, conversations, projects,
  │                  memories, model configs, system prompt, cleanup
  ├── inference.rs   application prompts and SDK event/error adapter
  │                    └── mira-runtime → mira-agent → mira-ai → provider
  ├── memory.rs      memory planner, retrieval, cleanup, sensitive filtering
  ├── secrets.rs     OS credential store access
  ├── database.rs    SQLite schema, migrations, queries, FTS5 indexes
  └── cancellation.rs attempt-owned runtime cancellation token
  ├── Local durable state: SQLite file
  ├── Credentials: OS keyring
  └── Network (mira-ai): user-configured OpenAI-compatible provider
```

### Frontend (Presentation)

All frontend paths in this section are relative to `apps/mira-desktop/`.

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
  `apps/mira-desktop/src-tauri/src/lib.rs`.
- The webview calls commands through `invoke` (see
  `apps/mira-desktop/src/core/tauriClient.ts`).
- The backend pushes updates back to the webview as events:
  - `message_stream_delta` — streaming assistant content and reasoning deltas.
  - `memories_changed` — emitted after a background memory pass writes facts.

### Backend Modules

| Module            | Responsibility                                                                   |
| ----------------- | -------------------------------------------------------------------------------- |
| `chat.rs`         | Tauri command handlers for chat, conversations, projects, memories, settings     |
| `inference.rs`    | Desktop prompt/history/compatibility policies and Runtime event/error mapping    |
| `memory.rs`       | Memory planner, retrieval, cleanup, sensitive-content filtering                  |
| `database.rs`     | SQLite schema, migrations, queries, FTS5 trigram indexes                         |
| `secrets.rs`      | Read/write/delete API keys in the OS credential store                            |
| `cancellation.rs` | Foreground attempt identity, latched cancellation and runtime token registration |
| `types.rs`        | Shared Rust types serialized to the frontend                                     |

### Model Gateway

- Both chat and background memory use per-run `mira-runtime` sessions. The
  desktop supplies already assembled prompts, explicit credentials and no tools.
  Sessions are operational only; SQLite remains canonical.
- `mira-ai` sends `{base_url}/chat/completions` with bearer auth and `stream: true`
  using Rust `reqwest` with rustls. Labels never select another provider API.
- The adapter preserves the Chinese system prompt, project/memory formatting,
  last-20-message window and text-only historical assistant replay. DeepSeek
  reasoner/v4 chat models retain thinking/high; memory uses temperature `0.0`
  without reasoning. `stream_options` is omitted for existing endpoint parity.
- Text and reasoning deltas stay separate in `message_stream_delta`. Tool-call
  argument deltas are not displayed. Terminal outcomes, not EOF, determine
  success; nonempty truncated answers remain valid.
- Connect timeout is 15s, response-header timeout 45s, with at most three setup
  attempts and a finite 300s run budget. Retry/cooldown and SSE bounds are owned
  by AI; provider prose is replaced by safe error categories.
- Foreground cancellation is push-driven, including header/body/retry waits.
  Attempt identity prevents stale registration from stealing a successor's
  cancellation. Cancelled turns retain the saved user but persist no partial
  assistant and schedule no memory pass. Background sessions are independent.
- Background planning now collects SSE instead of a nonstreaming response.
  Malformed SSE fails safely instead of being silently skipped; incompatible
  planner endpoints use the existing heuristic fallback.

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
  updater endpoints; model traffic originates from the Rust process.

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

## Reusable Packages (Implemented And Used By Desktop)

The root Cargo workspace defaults to the three library packages, so library
consumers and tests do not build Tauri:

- `crates/mira-ai` — portable messages, provider and bounded stream contracts;
  an optional OpenAI-compatible HTTP transport with offline loopback fixtures.
- `crates/mira-agent` — one operational transcript, bounded sequential tool
  execution, validated arguments, request transforms and cancellation repair.
- `crates/mira-runtime` — sessions, immutable explicit model bindings,
  caller-owned credential resolution and context providers. Context composition
  preserves canonical history and budgets all injected Unicode characters;
  usage belongs to each session, even when context providers are shared.

Agent and Runtime disable AI's default HTTP feature. They accept an injected
provider and have no Tauri, SQLite or keyring dependency. Each package contains
its own README, offline tests/example, MIT license and Pi attribution notice.
All three packages passed offline checks and independent acceptance reviews.
The desktop's chat and memory paths both consume these packages. Their public
APIs do not depend on application prompts, database entities or native services.
The desktop lives in `apps/mira-desktop`; library package metadata supports
independent consumers, but publication has not been performed.

## Remaining Target: Application API / App Core Split

The agreed direction separates presentation, app logic, inference, and native
capabilities into distinct layers:

```txt
Mira UI (React / Zustand presentation)
  │ Application API
  ▼
Mira App Core (conversation, project, memory, settings)
  ├── mira-runtime → mira-agent → mira-ai (independent Rust packages)
  └── Native services (SQLite, keyring, filesystem, OS integration)
```

Layer responsibilities and hard boundaries:

- **UI** is presentation only. It renders state and forwards user intent; it
  holds no durable state and owns no inference or session logic.
- **Mira App Core** is the application layer. It coordinates the UI, the
  runtime, and durable storage, and it owns the app's business rules.
- **Rust runtime packages** own inference and session handling. They are **not** the durable
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
  directly, runtime packages must not own Mira's canonical persistence, and Tauri must not
  become the future agent orchestration layer.
- **Pi reference scope:** translate selected AI and Agent contracts and offline
  tests into idiomatic Rust, preserving attribution. Do not depend on Pi's
  TypeScript runtime, CLI, extensions or coding tools. Generic tool execution
  in the library does not enable tools or autonomous workflows in Mira Desktop.
- **Package isolation:** lower layers know no desktop entities, database schema,
  keyring service names or application prompts. Context and credentials are
  supplied explicitly by the consumer.

[ADR 0006](adr/0006-rust-native-runtime.md) records the approved package design,
phased migration and validation contract. Library implementation and desktop
inference integration are complete. The separate Application API / App Core
presentation boundary shown above is still a target, not an implemented API.

## Frontend Structure Reference

Paths below are relative to `apps/mira-desktop/`.

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
