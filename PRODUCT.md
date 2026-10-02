# Mira Product Definition

This document defines what Mira is, who it is for, and the boundaries that keep
it small. Architecture lives in [docs/architecture.md](docs/architecture.md);
the UI contract lives in [DESIGN.md](DESIGN.md).

## One-Line Definition

Mira is a lightweight, local-first, memory-aware ChatGPT-style desktop client
for a single local user.

## What Mira Is

- A desktop chat client with a ChatGPT-style conversation surface: Markdown
  rendering, code highlighting, streaming replies, and a stop control.
- **Memory-aware.** Mira extracts long-term facts from conversations and
  injects the relevant ones back into later requests, across conversations.
- **Local-first.** Conversations, messages, projects, memory facts, and model
  config metadata are stored in a local SQLite file. Credentials stay in the
  operating system credential store.
- **Provider-flexible.** Any OpenAI-compatible chat completions endpoint works
  (OpenAI, DeepSeek, Ollama, self-hosted gateways).
- **Fork-friendly.** The codebase is intentionally small and readable so a
  single developer can restyle it, re-point it, or change the memory strategy.

## What Mira Is Not

These are deliberate product boundaries, not current limitations to fix:

- **Not an IDE.** It does not edit files, run terminals, or manage a project
  workspace.
- **Not an agent orchestration platform.** It does not plan tool use, dispatch
  sub-agents, or run autonomous multi-step workflows.
- **Not a knowledge base or RAG product.** It does not ship a vector database,
  document ingestion, or an external retrieval service.
- **Not a team or cloud product.** No accounts, no cloud sync, no multi-user
  collaboration, no server-side storage of the user's data.

See [docs/adr/0005-extension-boundary.md](docs/adr/0005-extension-boundary.md)
for the recorded rationale.

## Audience

Individuals who want a chat client they own and can modify: people who care
that their conversation data stays on their own machine, who want to bring
their own model endpoint, and who prefer a small codebase over a large platform.

## Core Capabilities

| Capability       | Summary                                                                    |
| ---------------- | -------------------------------------------------------------------------- |
| Chat             | Streaming ChatGPT-style conversation with Markdown and code highlight      |
| Long-term memory | Automatic and user-saved facts, retrieved by relevance across chats        |
| Projects         | Group conversations; retrieve only relevant project context per message    |
| Providers        | Multiple OpenAI-compatible model configs, chat and background models       |
| Local storage    | SQLite file in the app data directory for durable app data                 |
| Credentials      | Current saves use the OS credential store; legacy SQLite values may remain |
| Interrupt        | A stop control that cancels the in-flight model stream                     |
| i18n             | English and Chinese UI, English by default                                 |
| Auto-update      | Updater checks on launch; downloaded update artifacts require signatures   |

## Product Principles

1. **Local-first and owned.** The user's durable data belongs on the user's
   machine. Model requests go to the configured provider; update checks/downloads
   contact the configured updater and release hosts. Local-first is not offline-only.
2. **Simple enough.** Prefer one clear mechanism over a configurable platform.
   Scope grows only when a real user need requires it.
3. **Readable and forkable.** Small modules, honest naming, no hidden magic.

## Privacy Statement

Mira does not run a backend service of its own and does not upload data to a
Mira server. However, "local-first" does not mean "never leaves the machine":

- The current user message, the conversation history sent with it, the system
  prompt, and any retrieved memory or project context are sent over the network
  to the OpenAI-compatible provider the user configured, because that provider
  generates the reply.
- A background memory pass may send the latest turn to the configured
  background model to extract facts.

The durable records of conversations, memories, projects and settings stay in
local SQLite; their selected contents may be included in the requests above.
Current saves store API keys in the OS credential store and use them to
authenticate the configured provider. Startup does not migrate/clear legacy
SQLite key values; old databases and all backups require sensitive-data handling. Updates also contact the configured GitHub updater/release
endpoints. Neither local SQLite nor automatic sensitive-memory filtering is a
privacy or encryption guarantee.

## Non-Goals

RAG, vector databases, tool calling, multi-user accounts, and cloud sync are
explicitly out of scope. See the boundary ADRs under [docs/adr/](docs/adr/) and
the scope section of [docs/architecture.md](docs/architecture.md).
