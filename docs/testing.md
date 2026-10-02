# Testing

Mira has three automated test layers plus the static gates that CI runs on every
pull request. No tests use a live model provider. Browser tests replace native
IPC; Rust tests still compile/link the native Tauri dependencies but do not
launch its desktop runtime.

## Commands

| Command                     | What it does                                                                  |
| --------------------------- | ----------------------------------------------------------------------------- |
| `pnpm format:check`         | Prettier check over the repo.                                                 |
| `pnpm lint`                 | ESLint (flat config) over `src`, `tests`, and the config files.               |
| `pnpm typecheck`            | `tsc --noEmit` for `src`, then `tsc -p tsconfig.tests.json` for tests/config. |
| `pnpm test`                 | Vitest unit and integration tests, non-watch (`vitest run`).                  |
| `pnpm test:watch`           | Vitest in watch mode for local development.                                   |
| `pnpm build`                | Typecheck plus the production Vite bundle (`dist/`).                          |
| `pnpm test:ui`              | Playwright browser smoke tests against the built bundle.                      |
| `cargo fmt --check`         | Rust formatting (run from `src-tauri`).                                       |
| `cargo check --all-targets` | Rust typecheck for the library, binary, and tests.                            |
| `cargo test`                | Rust unit tests (run from `src-tauri`).                                       |

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
- `model.rs` covers SSE event framing and separators, content/reasoning
  separation, malformed payload tolerance, prompt assembly (memory, project
  context, 20-turn history window), DeepSeek reasoning request fields, and retry
  helpers.
- `cancellation.rs` covers the cancel flag lifecycle and its shared `Arc`
  visibility.
- `types.rs` covers the serialization contract for messages and memory patches.

Rust tests use in-memory SQLite and never touch the network, the keyring, or
provider APIs. `database::migrate_for_tests` is a `#[cfg(test)]`-only entry point
used by the memory tests to build a migrated database.

## CI

`.github/workflows/ci.yml` runs three jobs on pull requests to `main`/`master`:

- **frontend** — format, lint, typecheck, Vitest, production build.
- **ui-smoke** — Playwright Chromium install, production build, `pnpm test:ui`.
- **rust** — Linux Tauri system dependencies, `cargo fmt --check`,
  `cargo check --all-targets`, `cargo test`. The frontend is built first because
  `tauri::generate_context!` embeds `frontendDist`.

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
- There is no Pi adapter yet; its event/error/cancellation contract tests are
  required before the runtime migration.

## Adding tests

- Put frontend unit/integration tests in `tests/unit/` and name them after the
  module under test.
- Put browser smoke tests in `tests/ui/` and reuse `installTauriMock`; extend the
  mock's command table rather than mocking the store or components when a real
  integration path is intended.
- Put Rust tests in a `#[cfg(test)] mod tests` block in the module they cover,
  and reuse the in-memory database helper pattern.
