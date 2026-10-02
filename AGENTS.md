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
| `pnpm typecheck`    | Type-check app, tests and test configs         |
| `pnpm lint`         | ESLint frontend and tests                      |
| `pnpm test`         | Vitest unit/integration tests, non-watch       |
| `pnpm test:ui`      | Built-frontend Chromium smoke with mocked IPC  |
| `pnpm build`        | Type-check and build the production frontend   |
| `pnpm tauri`        | Run the Tauri CLI (`tauri dev`, `tauri build`) |
| `cargo test`        | Run Rust unit tests in `src-tauri`             |

Run `pnpm build` before `pnpm test:ui`. Install its browser with
`pnpm exec playwright install --with-deps chromium`. See [docs/testing.md](docs/testing.md)
for fixtures, native prerequisites and coverage limitations. No tests may use a
live provider, production/user database or actual keyring credentials.

## Repository Layout

```txt
src                 React frontend (components, pages, store, core, i18n, utils)
src-tauri/src       Rust backend (chat, model, memory, database, secrets, cancellation)
src-tauri           Tauri config, Cargo manifest, icons
tests               Vitest unit/integration and mocked-IPC Playwright smoke
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
- Run the narrowest relevant tests, `pnpm format:check`, `pnpm lint`,
  `pnpm typecheck`, `pnpm test`, and `pnpm build`. UI changes also need
  `pnpm test:ui` and rendered inspection.
- For Rust, run `cargo fmt --check`, `cargo check --all-targets --locked`, and
  `cargo test --locked` from `src-tauri` when native prerequisites are available.
  Never label a mocked browser smoke test as native verification.
- Update affected contracts/ADRs for intentional scope or boundary changes.
  Explain failures and environment limitations rather than weakening gates.
- Do not log API keys, authorization headers, raw prompts, memories or messages.
  No commits, pushes, releases or external actions without explicit permission.

## Docs Index

- [PRODUCT.md](PRODUCT.md) — product definition and non-goals.
- [DESIGN.md](DESIGN.md) — tokens, layout, accessibility, error, cancellation.
- [docs/architecture.md](docs/architecture.md) — current and target architecture.
- [docs/engineering.md](docs/engineering.md) — build, CI, and operational plan.
- [docs/testing.md](docs/testing.md) — test layers, commands and limitations.
- [docs/memory-system.md](docs/memory-system.md) — memory types and flows.
- [docs/project-context.md](docs/project-context.md) — project context retrieval.
- [docs/security.md](docs/security.md) — credential and destructive-action notes.
- [docs/adr/](docs/adr/) — architecture decision records.
