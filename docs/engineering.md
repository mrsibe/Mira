# Engineering And Operations Plan

This document describes how Mira is built, checked, and released today, and
what is still missing. It separates **what exists** from **what is planned**, so
nothing planned is mistaken for done.

Contributor-facing commands also live in [../AGENTS.md](../AGENTS.md).

## Local Commands (Existing)

| Command                                     | What it does                                   |
| ------------------------------------------- | ---------------------------------------------- |
| `pnpm dev`                                  | Start the Vite dev server for the frontend     |
| `pnpm format`                               | Format the repo with Prettier                  |
| `pnpm format:check`                         | Verify formatting without writing              |
| `pnpm typecheck`                            | Typecheck app, tests and test configs          |
| `pnpm lint`                                 | ESLint frontend/test checks                    |
| `pnpm test`                                 | Vitest unit/integration tests                  |
| `pnpm build`                                | Type-check and build the production frontend   |
| `pnpm tauri`                                | Run the Tauri CLI (`tauri dev`, `tauri build`) |
| `cargo test --locked`                       | Test reusable SDK packages from root           |
| `cargo test -p mira --all-targets --locked` | Test native desktop from root                  |

Root frontend commands forward to `@mira/desktop` in `apps/mira-desktop`.
Formatting and linting operate on the repository. From
`apps/mira-desktop/src-tauri`, `cargo test --locked` runs native tests (for
example in `memory.rs` and `database.rs`). From the repository root,
`cargo test --locked` selects the workspace's reusable library default members
instead and does not build Tauri.

## Reusable Rust Packages (Implemented)

`crates/mira-ai`, `crates/mira-agent` and `crates/mira-runtime` contain independent
MIT packages, API documentation, offline tests/examples and Pi attribution.
They declare Rust 1.88 as their minimum version. All three packages passed
independent acceptance reviews. Desktop chat and background memory both consume
them through the application-owned `inference.rs` adapter.

Local package checks, after fetching dependencies:

```bash
cargo +1.88.0 test --locked --offline
cargo +1.88.0 clippy --all-targets --locked --offline -- -D warnings
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 run -p mira-runtime --example offline_session --locked --offline
```

The root `Cargo.lock` is canonical for libraries and desktop. Library consumers
can inject a provider without enabling AI's optional HTTP transport; no Tauri,
SQLite or keyring dependency is required. Publication is not part of this work
and requires separate authorization.

## Continuous Integration (Existing)

`.github/workflows/ci.yml`:

- Triggers on pushes to `main`/`master` and all pull requests (including stacked
  infrastructure branches). Cancels superseded runs via a `ci-` concurrency group.
- Both jobs use `ubuntu-latest` and a 25-minute timeout.
- **SDK (Rust 1.88)** installs Rust 1.88 with Clippy/rustfmt and checks root
  default-member tests, warnings-denied Clippy, formatting, protocol-only AI and
  the offline Runtime example. It installs no Node or native desktop packages.
- **Verify** depends on SDK, uses `always()` and first fails unless the SDK
  result is exactly success, so skipped/cancelled/failed SDK checks cannot turn
  the required Verify check into a passing skipped job.
- Verify uses Node 22, pnpm `11.1.3` and frozen dependencies: formatting → lint
  → typecheck → Vitest → frontend build → Rust formatting and explicit
  `cargo test -p mira --all-targets --locked`. Native tests compile their targets,
  so a separate CI `cargo check` would duplicate work. Frontend output is
  `apps/mira-desktop/dist`.
- `pnpm/action-setup` reads the pinned package manager and caches its store;
  Rust compilation artifacts use the root workspace `target/` cache. Native
  dependencies are installed only in Verify.
- Browser automation has been removed; CI does not download browsers or run
  a preview server. Frontend interaction and native desktop smoke remain manual.

See [testing.md](testing.md) for assertions and limitations. The **Verify** check
is required evidence before merge; branch-protection rules must use its name if
enforced remotely. No gate uses `continue-on-error`. Local equivalent checks
passed; remote Actions and branch-protection settings were not exercised.

## Continuous Delivery (Existing)

`.github/workflows/cd.yml`:

- Triggers on `v*` tags, with `contents: write` permission.
- Builds a release matrix: `windows-latest`, `macos-latest`, `ubuntu-22.04`
  (60-minute timeout).
- Installs Node 22, the pinned pnpm and Rust stable, caches the pnpm store/Rust
  target, and installs native dependencies on Linux only. Packaging stays out of
  PR CI; there are no release-time lint/unit/browser-test duplicates.
- Runs `tauri-apps/tauri-action@v0` with `projectPath: apps/mira-desktop` to build
  and publish **drafts** with generated assets. Cargo caching uses root `target/`.
  The matrix, bundling and signing have not been run locally; no release was
  triggered as part of this implementation.
- `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are read
  from repository secrets. With `createUpdaterArtifacts` enabled, a valid
  updater signing key is required for signed updater artifacts; missing secrets
  must not be treated as a successfully signed release. Updater signatures are
  separate from OS code signing/notarization.
- `createUpdaterArtifacts` is enabled in `apps/mira-desktop/src-tauri/tauri.conf.json`, and the
  updater endpoint points at
  `https://raw.githubusercontent.com/MrSibe/Mira/main/updater/latest.json`.

## Planned / Not Yet Implemented

These are requirements for upcoming engineering PRs. None exist in the repo
today, and none should be described as available.

- **Further native static analysis.** SDK Clippy is a warnings-denied CI gate;
  native application Clippy has inherited lint findings and is not a fail gate.
  Existing Markdown helper `any` usage stays visible as lint warnings.
- **Native end-to-end tests.** Vitest mocks native IPC and cannot prove OS
  keyring, live-provider behavior or persistent desktop restart.
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

The baseline now covers memory selection/filtering, conversation lifecycle,
provider selection/IPC configuration, SSE framing, cancellation state and
in-memory SQLite schema initialization/idempotency. Old-version migration
fixtures and application prompt-token limits remain follow-up work. Reusable
Rust packages now have offline event/error/cancellation and transport fixtures;
desktop adapter fixtures add exact prompt parity, event projection, cancellation
identity and memory fallback. Workspace/MSRV and native gates are configured in
CI. These checks do not prove real Tauri IPC persistence or GUI restart behavior.
Local frozen installation, frontend build, native tests and
`pnpm tauri build --debug --no-bundle` passed. Avoid live-provider calls or
OS-keyring access in automated tests.

Manually inspect UI smoke cases: new chat, send, stream, switch conversation.
Vitest checks store/IPC wiring, not rendered behavior or SQLite durability.
Native restart → conversation still exists remains a desktop acceptance test.
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
