# ADR 0001: Use Tauri 2 With a React/TypeScript Frontend

- **Status:** Accepted
- **Date:** 2026-10-02 (retroactive record of the shipped v0.x architecture)

## Context

Mira is a lightweight, local-first desktop chat client. It needs a native
window, filesystem and credential access, and a custom UI, while staying small
enough for one person to read and fork. A bundled-browser stack such as
Electron would ship a large runtime for a small app.

## Decision

Build Mira as a Tauri 2 desktop application:

- React 19 + TypeScript + Tailwind CSS + Zustand for the presentation layer.
- A Rust backend for commands, networking, storage, and credentials (originally
  `src-tauri`, now `apps/mira-desktop/src-tauri` under ADR 0006).
- The frontend calls the backend through Tauri `invoke`, and the backend pushes
  streaming updates back as events.

## Alternatives

Electron bundles its browser/runtime, increasing packaging surface. A web-only
client cannot provide the same local native storage/keyring integration without
an additional service. Tauri still requires platform webview/system libraries
and platform-specific packaging verification; no measured footprint is claimed.

## Consequences

- Reuses the platform webview instead of bundling Chromium; actual installer
  size and memory footprint depend on platform and build.
- The backend is Rust, so chat streaming, SQLite, and keyring access stay in one
  native process.
- The frontend should stay presentation-focused; current durable data access
  belongs in Rust. The future application boundary is covered by ADR 0003.
- Contributions require both a web and a Rust toolchain.

## Notes

This ADR records the shell choice already present in the codebase. It does not
describe the future app-core/runtime split, which is covered by
[ADR 0003](0003-pi-runtime-direction.md).
