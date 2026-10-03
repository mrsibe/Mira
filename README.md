<div align="center">

# Mira

**A simple enough, memory-aware open-source LLM chat client**

ChatGPT-inspired · Local-first · Lightweight · Easy to modify

[Features](#features) · [Why Mira](#why-mira) · [Quick Start](#quick-start) · [Tech Stack](#tech-stack) · [Privacy](#privacy) · [Docs](#docs) · [Fork It](#fork-it)

**[简体中文](README.zh-CN.md)** · English

</div>

---

## Why Mira

I needed a **simple enough** open-source LLM chat app with memory.

Existing solutions are either too heavy, lock the model and memory into a cloud service, or have codebases too complex to modify. So I built Mira, inspired by ChatGPT's design — **lightweight, supports custom models, and easy to modify yourself**.

If you also want a chat app that remembers what you said and belongs to you, fork it and make it your own.

## Features

- **Chat** — ChatGPT-style conversation UI with Markdown rendering and code highlighting
- **Long-term memory** — Automatically extracts memories from conversations and injects relevant context across chats; manual saved memories supported too
- **Multi-provider** — Any OpenAI-compatible endpoint (OpenAI, DeepSeek, Ollama, self-hosted gateways…). API keys stored in the OS credential vault
- **Projects** — Group conversations into projects; conversations in a project share context
- **Local storage** — Conversations, projects, and memory stay in a local SQLite file; API keys live in the OS credential vault. Chat content is sent only to the provider you configure — see [Privacy](#privacy)
- **i18n** — English / Chinese UI, English by default

## Screenshot

![Mira chat interface](screenshots/chat.png)

## Quick Start

### Download

Head to the [Releases](https://github.com/MrSibe/Mira/releases) page and download the installer for your platform:

| Platform                  | File                                                                 |
| ------------------------- | -------------------------------------------------------------------- |
| **Windows**               | `Mira_<version>_x64-setup.msi.zip` or `Mira_<version>_x64_en-US.msi` |
| **macOS (Intel)**         | `Mira_<version>_x64.dmg`                                             |
| **macOS (Apple Silicon)** | `Mira_<version>_aarch64.dmg`                                         |
| **Linux**                 | `Mira_<version>_amd64.deb` or `Mira_<version>_amd64.AppImage`        |

Run the installer and launch Mira.  
No command line or development tools required.

> The first time you open Mira, it will prompt you to configure an API provider.  
> You need an **OpenAI-compatible API key** to start chatting — bring your own key from OpenAI, DeepSeek, or any other compatible provider.

### Build from source

If you want to build Mira yourself (requires [Node.js](https://nodejs.org/) 22+, [pnpm](https://pnpm.io/) 11+, [Rust](https://www.rust-lang.org/) stable):

```bash
pnpm install
pnpm tauri build
```

Run these commands from the repository root. Output lands in
`target/release/bundle/`; the desktop source lives in `apps/mira-desktop`.

## Tech Stack

| Layer       | Tech                                          |
| ----------- | --------------------------------------------- |
| Frontend    | React 19 · TypeScript · TailwindCSS · Zustand |
| Desktop     | Tauri 2                                       |
| Backend     | Rust · mira-runtime → mira-agent → mira-ai    |
| Storage     | SQLite (local file)                           |
| Credentials | OS keyring                                    |
| i18n        | Lightweight built-in (en / zh)                |

## Privacy

Mira runs no backend of its own and uploads nothing to a Mira server. But
"local" does not mean "never leaves the machine": to get a reply, Mira sends
the current message, the conversation history sent with it, the system prompt,
and any retrieved memory or project context over the network to the
OpenAI-compatible provider you configured. A background memory pass may also
send the latest turn to your configured background model.

The durable records stay in local SQLite; selected content can be included in
those requests. API keys are stored in the OS credential vault and used to
authenticate provider requests. Update checks/downloads also contact the
configured GitHub updater/release endpoints. See [docs/security.md](docs/security.md)
for details; local database backups still contain private user data.

## Architecture

The diagram below is the **current** architecture. See
[docs/architecture.md](docs/architecture.md) for the canonical description of
the implemented Rust SDK integration and the remaining App Core/UI split.

```txt
React UI / Zustand
  ↓ invoke / events
Tauri Commands (Rust application coordination)
  ├── mira-runtime → mira-agent → mira-ai → configured provider
  ├── Memory extraction / retrieval
  ├── SQLite (persistent user data)
  └── OS keyring (credentials)
```

## Project Structure

```txt
crates/
├── mira-ai         # Portable protocol and optional OpenAI-compatible transport
├── mira-agent      # Operational agent state and bounded validated tools
└── mira-runtime    # Sessions, explicit model registry and injected context

apps/mira-desktop/
├── src             # React UI, Zustand store, IPC client and i18n
├── tests           # Vitest unit/integration fixtures
├── public          # Fonts and static assets
└── src-tauri/src
    ├── chat.rs         # Tauri application commands
    ├── inference.rs    # Desktop prompts and Runtime adapter
    ├── database.rs     # SQLite data layer
    ├── memory.rs       # Memory policies and planner fallback
    ├── cancellation.rs # Attempt-owned cancellation
    ├── secrets.rs      # OS credential store access
    └── types.rs        # Desktop IPC types
```

## Reusable Rust SDK

Mira Desktop is a consumer of three independently usable MIT crates (Rust 1.88+):

- [mira-ai](crates/mira-ai/README.md): provider-neutral messages and streams,
  with an optional OpenAI-compatible HTTP transport.
- [mira-agent](crates/mira-agent/README.md): agent state, validated sequential
  tools and bounded cancellation-safe runs.
- [mira-runtime](crates/mira-runtime/README.md): sessions, immutable model
  bindings and caller-owned credentials/context.

Agent and Runtime do not enable HTTP automatically. No package depends on
Tauri, SQLite or keyring; storage and application policy stay with the consumer.
The crates are implemented and tested locally, not published to a registry.

```bash
cargo +1.88.0 test --locked
cargo +1.88.0 run -p mira-runtime --example offline_session --locked
```

## Docs

- [Product Definition](PRODUCT.md) — what Mira is and is not
- [Architecture](docs/architecture.md) — current design and target direction
- [Design Contract](DESIGN.md) — tokens, layout, accessibility, error, and cancellation
- [Engineering & Ops](docs/engineering.md) — build, CI, and what is still planned
- [Testing](docs/testing.md) — offline SDK, SQLite and mocked IPC fixtures
- [Memory System](docs/memory-system.md)
- [Project Context](docs/project-context.md)
- [Security](docs/security.md)
- [Architecture Decision Records](docs/adr/)
- [Agent / Contributor Guide](AGENTS.md)

## Scope

v1 focuses on local single-user, plain chat, long-term memory, local SQLite storage, and multi-provider config.

**Not doing in Mira Desktop:** RAG, vector databases, tool calling, multi-user,
cloud sync. Portable tool support in the SDK does not enable tools in the app.

## Fork It

Mira's code is deliberately simple and readable. If you want your own LLM chat app, fork it and:

- Restyle the UI to your taste
- Wire up your own models or gateways
- Tweak the memory strategy
- Add whatever you need

PRs are welcome, but please open an issue first to discuss the direction.

## License

[MIT](LICENSE)
