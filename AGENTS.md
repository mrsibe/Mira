# AGENTS.md

Guidance for automated agents and contributors working in this repository.

## What This Repo Is

Mira is a lightweight, local-first, memory-aware ChatGPT-style desktop client
(React + Tauri 2 + Rust + SQLite). Read [PRODUCT.md](PRODUCT.md) before changing
scope, and [docs/architecture.md](docs/architecture.md) before touching the
backend. The product boundary is deliberately narrow: not an IDE, not an agent
orchestration platform, not a knowledge base.

Hard constraints:

- **Never store API keys in SQLite.** Keys live in the OS credential store
  (`src-tauri/src/secrets.rs`); the database holds only provider metadata.
- UI must not access SQLite or model providers directly. Today persistence and
  model I/O are in Rust, with application coordination in the Zustand store.
- Target direction: UI → Application API → Mira App Core → separate Pi runtime
  and native-service adapters. React owns presentation, Pi owns inference/session
  execution, Tauri owns native capabilities, SQLite owns persistent user data.
  Do not relocate logic or implement the runtime migration opportunistically.
- Do not add runtime dependencies for documentation-only changes.

## Requirements

- Node.js 22+
- pnpm 11+ (the repo pins `pnpm@11.1.3`)
- Rust stable (for Tauri builds)

## Commands (Existing)

| Command             | What it does                                   |
| ------------------- | ---------------------------------------------- |
| `pnpm dev`          | Start the Vite dev server                      |
| `pnpm format`       | Format with Prettier                           |
| `pnpm format:check` | Check formatting without writing               |
| `pnpm typecheck`    | Type-check with `tsc --noEmit`                 |
| `pnpm build`        | Type-check and build the production frontend   |
| `pnpm tauri`        | Run the Tauri CLI (`tauri dev`, `tauri build`) |
| `cargo test`        | Run Rust unit tests in `src-tauri`             |

Prefer verifying a change with `pnpm format:check`, `pnpm typecheck`, and
`pnpm build` (and `cargo test` for Rust changes).

## Commands (Planned — Not Available Yet)

The following scripts do **not** exist in `package.json` today. They are
requirements for an upcoming engineering PR (see
[docs/engineering.md](docs/engineering.md)) and must not be run as if present:

- `pnpm lint` — frontend linting (no linter configured yet).
- `pnpm test` — frontend unit tests (not implemented).
- `pnpm test:ui` — frontend UI tests (not implemented).

## Repository Layout

```txt
src                 React frontend (components, pages, store, core, i18n, utils)
src-tauri/src       Rust backend (chat, model, memory, database, secrets, cancellation)
src-tauri           Tauri config, Cargo manifest, icons
docs                Architecture, memory, project context, security, engineering, ADRs
.github/workflows   ci.yml (checks) and cd.yml (release)
public              Fonts and static assets
updater             Released updater manifest
```

## Conventions

- **Formatting:** Prettier (`.prettierrc.json`: 2-space indent, 80 columns,
  double quotes, semicolons, `lf`). Prefer `pnpm exec prettier --write <paths>`
  for changed files; `pnpm format` formats the entire repo.
- **TypeScript:** `strict`, `noUnusedLocals`, and `noUnusedParameters` are on;
  do not leave unused symbols.
- **Naming:** one spelling per concept; tests named for the source they cover.
- **Docs:** keep them accurate to the code. Distinguish shipped behavior from
  planned behavior, and never describe unimplemented features as done.

## Before Finishing

- Inspect the diff and preserve unrelated user changes.
- Run the narrowest relevant tests, `pnpm format:check`, `pnpm typecheck`, and
  `pnpm build`. For Rust, run `cargo fmt --manifest-path src-tauri/Cargo.toml
--check`, `cargo check --manifest-path src-tauri/Cargo.toml --locked`, and
  `cargo test --manifest-path src-tauri/Cargo.toml --locked` when available.
- After the engineering baseline lands, also run its lint/unit/UI checks as
  appropriate. Never label a mocked browser smoke test as native verification.
- Update affected contracts/ADRs for intentional scope or boundary changes.
  Explain failures and environment limitations rather than weakening gates.
- Do not log API keys, authorization headers, raw prompts, memories or messages.
  No commits, pushes, releases or external actions without explicit permission.

## Docs Index

- [PRODUCT.md](PRODUCT.md) — product definition and non-goals.
- [DESIGN.md](DESIGN.md) — tokens, layout, accessibility, error, cancellation.
- [docs/architecture.md](docs/architecture.md) — current and target architecture.
- [docs/engineering.md](docs/engineering.md) — build, CI, and operational plan.
- [docs/memory-system.md](docs/memory-system.md) — memory types and flows.
- [docs/project-context.md](docs/project-context.md) — project context retrieval.
- [docs/security.md](docs/security.md) — credential and destructive-action notes.
- [docs/adr/](docs/adr/) — architecture decision records.
