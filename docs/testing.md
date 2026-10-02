# Testing

Mira has frontend, browser, Rust and compiled Pi runtime test layers. CI runs static checks, frontend unit/integration
and Rust tests on every pull request; browser smoke is local opt-in. No tests use a live model provider. Browser tests replace native
IPC; Rust tests still compile/link the native Tauri dependencies but do not
launch its desktop runtime.

## Commands

| Command                     | What it does                                                                   |
| --------------------------- | ------------------------------------------------------------------------------ |
| `pnpm format:check`         | Prettier check over the repo.                                                  |
| `pnpm lint`                 | ESLint (flat config) over `src`, `tests`, and the config files.                |
| `pnpm typecheck`            | `tsc --noEmit` for `src`, then `tsc -p tsconfig.tests.json` for tests/config.  |
| `pnpm test`                 | Vitest unit and integration tests, non-watch (`vitest run`).                   |
| `pnpm test:watch`           | Vitest in watch mode for local development.                                    |
| `pnpm runtime:test`         | Compile the Pi AI sidecar and run offline loopback provider/protocol fixtures. |
| `pnpm build`                | Typecheck plus the production Vite bundle (`dist/`).                           |
| `pnpm test:ui`              | Playwright browser smoke tests against the built bundle.                       |
| `cargo fmt --check`         | Rust formatting (run from `src-tauri`).                                        |
| `cargo check --all-targets` | Rust typecheck for the library, binary, and tests.                             |
| `cargo test`                | Rust unit tests (run from `src-tauri`).                                        |

`pnpm test:ui` serves the production build through `vite preview`, so run
`pnpm build` first. Playwright browsers are installed with
`pnpm exec playwright install --with-deps chromium`.

## Frontend unit and integration tests (Vitest)

Location: `tests/unit/**/*.test.ts` (config: `vitest.config.ts`, environment:
`jsdom`, setup: `tests/setup.ts`).

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

## Browser smoke tests (Playwright)

Location: `tests/ui/**/*.spec.ts` (config: `playwright.config.ts`, Chromium).

The specs load the real production bundle in Chromium and inject an explicit
mock of the Tauri IPC boundary before the app loads
(`tests/ui/tauriMock.ts`). The mock implements the subset of
`window.__TAURI_INTERNALS__` and the event plugin that the frontend calls, holds
`send_message` open, and exposes `window.__MIRA_TEST__` so a test can emit
`message_stream_delta` events and then settle the call.

Covered smoke paths:

1. The chat shell renders and bootstrap issues its IPC commands.
2. A message streams token deltas into the assistant bubble through the mocked
   event bridge and is replaced by the persisted result when `send_message`
   resolves.
3. Cancel ignores late deltas and returns to idle after the mocked request
   settles; this does not prove transport cancellation for a stalled provider.
4. New chat creates an empty local draft without persisting it prematurely.
5. Switching conversations replaces visible messages via the mocked IPC.

The mocked boundary is deliberate. These tests make **no claim** about native
persistence (SQLite), the system keyring, provider traffic, or app restart
behaviour; those remain manual/native verification.

## Rust tests

Location: `#[cfg(test)] mod tests` in `src-tauri/src/*.rs`.

- `database.rs` builds an in-memory SQLite database via migration and covers
  schema migration idempotency and version metadata, conversation and project
  lifecycle (create, rename, archive/restore, move, delete, project cascade),
  message ordering and reasoning persistence, rename validation, system prompt
  round-trip, and memory FTS search.
- `memory.rs` covers the heuristic planner, planner JSON normalization,
  duplicate detection, the sensitive-content filter, and the stale-memory
  cleanup pass.
- `model.rs` covers prompt assembly (memory, project context, 20-turn history
  window); `runtime.rs` covers private JSONL validation and child lifecycle.
- `cancellation.rs` covers the cancel flag lifecycle and its shared `Arc`
  visibility.
- `types.rs` covers the serialization contract for messages and memory patches.

Rust tests use in-memory SQLite and never touch the network, the keyring, or
provider APIs. `database::migrate_for_tests` is a `#[cfg(test)]`-only entry point
used by the memory tests to build a migrated database.

## Compiled Pi runtime tests

`pnpm runtime:test` builds the standalone host binary and runs
`runtime/tests/runtime.test.mjs`. Fixtures bind only to loopback with synthetic
credentials. They cover Chat Completions request/context compatibility, Unicode
text and thinking deltas, DeepSeek reasoning, planner sampling, SDK retry,
redacted authentication failures, empty replies, isolated concurrent requests,
invalid/oversized protocol input and abort before the first token (POSIX).
These tests exercise the real compiled Pi adapter, not live provider accounts.
They also pin logging isolation under inherited `OPENAI_LOG=info/debug` and
rejection of Retry-After waits that exceed the host's budget.
`cargo test --locked compiled_runtime -- --ignored` runs the Rust-to-compiled-Pi
loopback bridge integration (also in CI); it is explicitly ignored in standalone
Cargo runs until the sidecar has been built.
Run `pnpm runtime:build` before standalone Cargo checks so Tauri can find its
external binary. Tauri dev/build hooks do this automatically.

## CI

`.github/workflows/ci.yml` runs one **Verify** job on every pull request (including
stacked branches), and on pushes to `main`/`master`:

- Format, lint, typecheck, Vitest and one production build.
- Compiled Pi runtime and offline loopback protocol tests.
- Linux Tauri dependencies, `cargo fmt --check`, `cargo test --locked`.
  Tests already compile native targets, avoiding a duplicate `cargo check`.
  The frontend is built first because `tauri::generate_context!` embeds
  `frontendDist`.

No Chromium download or browser tests in CI. Keep `pnpm test:ui` for local UI
changes and pre-release smoke; it is not a PR merge gate. Multi-platform
packaging runs only on version tags in `cd.yml`.

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
- Fresh Rust builds need Tauri Linux packages and OpenSSL development files;
  CI installs them. A local run may need an administrator to provision these.
- Pi runtime fixtures verify Linux host transport behavior, not native desktop
  IPC, other platform packaging, OAuth or live model compatibility. The sidecar
  is sizeable; artifact-size validation remains part of release review.

## Adding tests

- Put frontend unit/integration tests in `tests/unit/` and name them after the
  module under test.
- Put browser smoke tests in `tests/ui/` and reuse `installTauriMock`; extend the
  mock's command table rather than mocking the store or components when a real
  integration path is intended.
- Put Rust tests in a `#[cfg(test)] mod tests` block in the module they cover,
  and reuse the in-memory database helper pattern.
