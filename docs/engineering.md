# Engineering And Operations Plan

This document describes how Mira is built, checked, and released today, and
what is still missing. It separates **what exists** from **what is planned**, so
nothing planned is mistaken for done.

Contributor-facing commands also live in [../AGENTS.md](../AGENTS.md).

## Local Commands (Existing)

| Command             | What it does                                   |
| ------------------- | ---------------------------------------------- |
| `pnpm dev`          | Start the Vite dev server for the frontend     |
| `pnpm format`       | Format the repo with Prettier                  |
| `pnpm format:check` | Verify formatting without writing              |
| `pnpm typecheck`    | `tsc --noEmit` type check                      |
| `pnpm build`        | Type-check and build the production frontend   |
| `pnpm tauri`        | Run the Tauri CLI (`tauri dev`, `tauri build`) |
| `cargo test`        | Run the Rust unit tests in `src-tauri`         |

`cargo test` runs the existing `#[cfg(test)]` tests in `src-tauri/src` (for
example in `memory.rs` and `database.rs`).

## Continuous Integration (Existing)

`.github/workflows/ci.yml`:

- Triggers on push and pull requests to `main` and `master`.
- Cancels superseded runs via a `ci-` concurrency group.
- On `ubuntu-latest` (15-minute timeout):
  1. Install Node 22 and pnpm `11.1.3`.
  2. Cache the pnpm store, then `pnpm install --frozen-lockfile`.
  3. `pnpm format:check`.
  4. `pnpm typecheck`.
  5. `pnpm build`.

CI does **not** currently run a frontend linter, frontend tests, or any Rust
check. See the gaps below.

## Continuous Delivery (Existing)

`.github/workflows/cd.yml`:

- Triggers on `v*` tags, with `contents: write` permission.
- Builds a release matrix: `windows-latest`, `macos-latest`, `ubuntu-22.04`
  (60-minute timeout).
- Installs Node 22, pnpm, Rust stable, caches the Rust target, and installs the
  Linux webkit/gtk dependencies.
- Runs `tauri-apps/tauri-action@v0` to build and publish. Releases are published
  as **drafts** with generated assets.
- `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are read
  from repository secrets. With `createUpdaterArtifacts` enabled, a valid
  updater signing key is required for signed updater artifacts; missing secrets
  must not be treated as a successfully signed release. Updater signatures are
  separate from OS code signing/notarization.
- `createUpdaterArtifacts` is enabled in `src-tauri/tauri.conf.json`, and the
  updater endpoint points at
  `https://raw.githubusercontent.com/MrSibe/Mira/main/updater/latest.json`.

## Planned / Not Yet Implemented

These are requirements for upcoming engineering PRs. None exist in the repo
today, and none should be described as available.

- **Frontend linting.** No `lint` script or linter config is present.
- **Automated tests.** No `test` or `test:ui` script is present. Rust unit tests
  exist but run only when invoked manually; they are not in CI.
- **Rust checks in CI.** `cargo fmt --check`, `cargo clippy`, and `cargo test`
  are not part of `.github/workflows/ci.yml`.
- **Structured logging and observability.** The backend currently returns error
  strings to the frontend; there is no structured logging, log level control, or
  local log retention story.
- **Database migration strategy.** `schema_meta.schema_version` and the FTS
  schema version exist, but forward-migration rules, downgrade behavior, and
  backup-before-migrate are not documented or enforced.
- **Release rigor.** Code signing, notarization (macOS), and publishing the
  updater manifest to `updater/latest.json` are manual and only partially
  automated; the CD workflow publishes drafts but does not enforce signing.
- **Secrets handling in CI.** Only the Tauri signing key is wired; there is no
  documented policy for other CI secrets.

## Validation Priorities

The test/CI baseline should cover memory selection/filtering, conversation
lifecycle, provider configuration, stream parsing, cancellation state and SQLite
schema initialization/migration. Once a Pi adapter exists, add contract tests
for its events, errors and cancellation before switching the default runtime.
Avoid live-provider calls or OS-keyring access in automated tests.

Keep a few UI smoke cases: new chat, send, stream, switch conversation. A mocked
Tauri browser harness can prove UI wiring, not SQLite durability. Native
restart → conversation still exists remains a separate desktop acceptance test.
No arbitrary coverage targets, Storybook, or extra services are required.

## Migration Safety (Required For Future Schema Changes)

Preserve user databases; never delete a database to repair initialization.
Changes need idempotent forward migration, transaction/failure recovery tests,
and fixtures for supported older schemas. Specify backup and downgrade behavior
before releasing a destructive migration. Current version metadata alone is not
proof of transactional or downgrade-safe migration.

## Local Logging Contract (Implementation Pending)

Introduce one bounded local diagnostic sink with level, module, operation,
request/correlation ID and safe error category. Plan rotation/retention and a
user-reviewed diagnostic export before enabling persistent logs. Never record
API keys, authorization headers, prompts, message text, memory facts, provider
response bodies or unnecessary identifying paths. Logging failure must not
break chat. Current error-string propagation does not meet this contract; this
PR adds requirements, not a logging implementation.

## Release Acceptance (Manual Until Automated)

Before publishing a tag, verify supported installers (including Intel/Apple
Silicon architecture coverage), updater signatures and platform signing policy,
and test update/restart against a previous released version. Review version
consistency, asset URLs, signatures and `updater/latest.json`; the current CD
workflow does not update that manifest. Keep drafts unpublished until those
checks pass. Do not generate/rotate signing secrets or publish a release as part
of ordinary development tasks.

## Operational Boundaries

- Mira runs no first-party backend service; there is nothing for the team to
  operate server-side.
- All durable state is on the user's machine (SQLite + OS keyring), so release
  quality is about packaging, migration safety, and updater integrity rather
  than uptime.
