# Testing

Mira has frontend unit/integration, reusable Rust package and native desktop
Rust test layers. CI defines SDK/MSRV and frontend/native checks. No tests use
a live model provider. Vitest replaces native IPC; desktop Rust tests
compile/link Tauri dependencies but do not launch its desktop runtime. Browser automation has been removed;
rendered UI and native desktop acceptance require manual inspection.

## Commands

| Command                                      | What it does                                                    |
| -------------------------------------------- | --------------------------------------------------------------- |
| `pnpm format:check`                          | Prettier check over the repo.                                   |
| `pnpm lint`                                  | ESLint (flat config) over `src`, `tests`, and the config files. |
| `pnpm typecheck`                             | App-local `tsc --noEmit`, then `tsc -p tsconfig.tests.json`.    |
| `pnpm test`                                  | Vitest unit and integration tests, non-watch (`vitest run`).    |
| `pnpm test:watch`                            | Vitest in watch mode for local development.                     |
| `pnpm build`                                 | Typecheck plus Vite bundle (`apps/mira-desktop/dist/`).         |
| `cargo fmt --all -- --check`                 | Rust workspace formatting from root.                            |
| `cargo check -p mira --all-targets --locked` | Native desktop typecheck from root.                             |
| `cargo test -p mira --all-targets --locked`  | Native desktop tests from root.                                 |
| `cargo test --locked`                        | SDK default-member tests from root (no Tauri).                  |

## Reusable Rust packages

Run from the repository root; workspace `default-members` select only
`mira-ai`, `mira-agent` and `mira-runtime`, without building Tauri:

```bash
cargo +1.88.0 test --locked --offline
cargo +1.88.0 clippy --all-targets --locked --offline -- -D warnings
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 test -p mira-ai --no-default-features --locked --offline
cargo +1.88.0 run -p mira-runtime --example offline_session --locked --offline
```

Offline mode requires dependencies to have been fetched previously. These
commands also verify the packages' declared Rust 1.88 minimum version.

- AI fixtures cover portable protocol contracts, SSE fragmentation and Unicode,
  stream completion/cancellation, request encoding, tool deltas, error privacy,
  retry/cooldown bounds and provider isolation against loopback HTTP only.
- Agent fixtures cover validated sequential tools, bounded turn/call/time limits,
  cancellation under backpressure, interrupted-history repair, tool panics,
  request transforms and independent runs.
- Runtime fixtures cover explicit model routing, idle-only model/thinking/history
  changes, credential preparation/cancellation, independent sessions, deadline
  and drop cleanup, context priority and full rendered-character budgets,
  canonical prompt/history preservation and per-session metrics under a shared
  context manager. Fake providers, resolvers and context sources are injected.

These are library checks, not native desktop verification. They do not exercise
Tauri IPC, production SQLite, real keyring credentials or live providers. The
library packages and desktop integration passed independent acceptance reviews.
A standalone consumer outside the workspace also compiled
and ran the fake Runtime example on Rust 1.88 without native dependencies.

## Frontend unit and integration tests (Vitest)

Location: `apps/mira-desktop/tests/unit/**/*.test.ts`. Configuration and setup
are app-local `vitest.config.ts` and `tests/setup.ts` (`jsdom`). Root PNPM
commands forward to this app; `src/` paths below are app-relative.

- `store.test.ts` drives the real Zustand store against a mocked
  `src/core/tauriClient` and a mocked `@tauri-apps/api/event` channel. It covers
  bootstrap hydration and backend-unavailable fallback, draft conversation
  handling, archive/delete/select flows, active-model selection and delete
  fallback, and the send pipeline: optimistic messages, streamed content and
  reasoning deltas, cancel (late deltas ignored), and send failures.
- `tauriClient.test.ts` pins the IPC command names and argument shapes for the
  conversation, message, memory, and streaming commands.
- `fallbackMessage.test.ts`, `i18n.test.ts`, and `theme.test.ts` cover the local
  fallback message shape, locale/translation interpolation, and theme
  resolution and persistence.

These tests run the real store, i18n, and theme modules; only the Tauri IPC
boundary is replaced. They do not exercise SQLite, the system credential store,
or the Rust commands.

## Rust tests

Location: `#[cfg(test)] mod tests` in
`apps/mira-desktop/src-tauri/src/*.rs`.

- `database.rs` builds an in-memory SQLite database via migration and covers
  schema migration idempotency and version metadata, conversation and project
  lifecycle (create, rename, archive/restore, move, delete, project cascade),
  message ordering and reasoning persistence, rename validation, system prompt
  round-trip, and memory FTS search.
- `memory.rs` covers the heuristic planner, planner JSON normalization,
  duplicate detection, the sensitive-content filter, and the stale-memory
  cleanup pass.
- `inference.rs` uses fake providers and real loopback HTTP to pin exact desktop
  prompts, the last-20-before-filter window, legacy text-only history, DeepSeek
  fields, separate text/reasoning projection, ignored tool arguments, safe errors,
  callback cancellation and independent background inference. SDK tests own SSE
  framing/retry coverage; duplicate `model.rs` transport/tests were removed.
- `cancellation.rs` covers attempt identity, latched cancellation, stale
  registration refusal and predecessor guard cleanup without stealing a successor.
- `memory.rs` also verifies missing credentials and invalid URLs fall back to
  heuristic planning.
- `types.rs` covers the serialization contract for messages and memory patches.

Rust tests use in-memory SQLite and synthetic credentials. Inference fixtures
use loopback HTTP only, never external providers, production data or the keyring. `database::migrate_for_tests` is a `#[cfg(test)]`-only entry point
used by the memory tests to build a migrated database.

## CI

`.github/workflows/ci.yml` defines two jobs on pull requests and pushes to
`main`/`master`:

- **SDK (Rust 1.88)**: workspace default-member tests, Clippy with warnings
  denied, formatting, protocol-only AI tests and the offline Runtime example.
  No Node, GTK installation or desktop compilation is required.
- **Verify** depends on SDK and runs even if SDK fails, first refusing every
  non-success SDK result. It then runs frontend formatting, lint, typecheck,
  Vitest and build; native prerequisites; workspace formatting and explicit
  `cargo test -p mira --all-targets --locked`.
  The frontend build produces `apps/mira-desktop/dist` before
  `tauri::generate_context!` embeds it. Native tests already compile the targets,
  avoiding a duplicate CI `cargo check`.

Local equivalents passed, including frozen dependency installation and a linked
`pnpm tauri build --debug --no-bundle`. A one-off rendered browser inspection
with unavailable native IPC confirmed the chat fallback layout and About page's
MIT label. No browser suite or dependency was reintroduced; this is not native
window, keyring or restart verification. Remote Actions were not executed.

There are no automated browser tests. Rendered UI inspection and native desktop
smoke are manual checks. Multi-platform packaging runs only on version tags in
`cd.yml`.

## Known limitations

- `pnpm lint` currently reports warnings (not errors) for
  `@typescript-eslint/no-explicit-any` in the Markdown remark plugin. The rule is
  intentionally left at `warn` so the existing helper keeps its place in the
  gate; typing it is follow-up work.
- No coverage thresholds are enforced.
- No native/end-to-end verification of SQLite persistence, keyring access, or
  restart flows is automated yet. Before release, create a chat in the desktop
  app, send to a test provider, switch chats, quit/restart, and verify persisted
  messages and selected project. Do not call a browser reload a native restart.
- Native Rust builds need Tauri Linux packages. The SDK HTTP transport uses
  rustls and no longer requires OpenSSL; CI retains its existing native package
  provisioning. A local run may still need an administrator for GTK/WebKit.
- Native Clippy with warnings denied is not an app CI gate; inherited native
  lint findings remain. SDK Clippy is a fail-on-warning gate.
- Runtime task destruction during an active child tool run and the adapter's
  mid-preparation stale-registration branch are source-traced rather than
  deterministically tested end-to-end. State-level refusal/latch regressions
  exist; real Tauri cancellation persistence remains a manual acceptance check.
- Native window inspection, Windows/macOS builds, bundling, signing and updater
  artifacts were not verified in this implementation.

## Adding tests

- Put frontend tests in `apps/mira-desktop/tests/unit/` and name them after the
  module under test.
- Put Rust tests in a `#[cfg(test)] mod tests` block in the module they cover,
  and reuse the in-memory database helper pattern.
