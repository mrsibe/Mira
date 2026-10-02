# ADR 0006: Replace the Model Gateway with a Pi AI Sidecar

- **Status:** Accepted (phase 1 implemented)
- **Date:** 2026-10-07

## Context

[ADR 0003](0003-pi-runtime-direction.md) approves a Pi runtime adapter, but
leaves its integration design open. Rust currently maintains HTTP request
construction, SSE parsing, reasoning and retries, with a separate HTTP path
for the memory planner. Replace that compatibility layer without changing
conversation persistence or expanding the product into a coding agent.

## Decision

Use `@earendil-works/pi-ai@1.0.0` in `runtime/`, a separate TypeScript workspace.
Compile it with pinned Bun into a standalone Tauri `externalBin`. End users
need neither Node nor Bun. Do not import Pi into React or use `pi-coding-agent`,
its RPC CLI, sessions, extensions, skills, shell tools or filesystem tools.
`pi-agent-core` is deferred until an agent loop is actually needed; no tools
are declared or executed in this phase.

Keep existing model configurations on Pi's OpenAI Chat Completions adapter.
Do not silently reinterpret an `openai` label as Responses API, ignore a user's
custom endpoint, or claim native Anthropic/Google/OAuth support. Model catalogs,
provider-native API selection, OAuth credential refresh and their UI require
separate work. Mira still constructs memory/project/system context and owns
all SQLite writes. Both chat and memory planner inference use the sidecar.
Application coordination remains in Rust/Zustand temporarily; the full App
Core split in ADR 0003 is not implemented by this gateway replacement.

### Process and protocol

Each inference owns a child process. This deliberately avoids shared mutable
sessions, request multiplexing, cross-request credentials and a complex
long-lived supervisor in the first migration. Chat and background memory can
run independently. The Rust adapter resolves only the bundled sibling binary
(or the target-suffixed `src-tauri/binaries` binary in debug builds), not PATH.

Private stdin/stdout uses JSONL version `1`:

- Request: `complete`, UUID `id`, model config, prepared system/user/assistant
  messages, optional temperature. Credentials travel only on stdin.
- Updates: `text_delta` and `thinking_delta`, carrying the request ID.
- Exactly one terminal event: `done` with canonical text, or `error` with a
  `protocol`, `provider` or `cancelled` code. No raw SDK errors or bodies.
- Maximum request: 8 MiB. Maximum event frame: 1 MiB. Unknown/mismatched/malformed
  frames, EOF before a terminal event and timeouts fail closed.

Pi owns provider HTTP, parsing, thinking and SDK retries (two retries). HTTP
requests use a 45-second SDK timeout and reject Retry-After delays above five
seconds. The Rust host bounds inactivity to 180 seconds and total inference to
300 seconds, and
polls cancellation while waiting, including before the first token. Cancelling
kills and reaps that request's process; callback/protocol failures do the same.
The sidecar also maps an AbortSignal to Pi when signalled. Child stderr is not
forwarded, because SDK failures may contain credentials or user content.
`OPENAI_LOG` is forced off in the host and sidecar so inherited SDK logging
cannot bypass the JSONL/error-redaction boundary.
Neither sidecar nor host writes prompts or secrets to disk or logs.

### Compatibility and packaging

Keep the existing custom Base URL/model/key settings, context history window,
DeepSeek reasoner/v4 high thinking, content/reasoning separation and heuristic
memory fallback. No schema migration or frontend IPC change is required.
Existing key-required behavior remains, including for local endpoints.

`pnpm runtime:build` produces the host-target binary; Tauri dev/build hooks
build it automatically. Linux x64 and Windows x64 use Bun's baseline CPU
build. macOS arm64/x64 and Linux arm64 are also mapped; unsupported targets
fail explicitly. CI builds the Linux sidecar and runs offline protocol tests.
Release Tauri hooks compile it on each platform before bundling/signing.

## Consequences

- Removes Mira's HTTP/SSE provider compatibility code, including the independent
  memory-planner gateway, while preserving Mira's product boundaries.
- A per-request process has startup overhead and no shared connection pool.
- Bundling Bun adds substantial desktop artifact size (approximately 78 MiB
  uncompressed on Linux x64 in local validation). This is a real cost, not a
  claim that toolkit reuse is free. Re-evaluate size and process reuse before
  further provider expansion.
- Errors are deliberately redacted; provider-specific safe diagnostic categories
  can be added later without exposing raw responses.
- Linux offline fixtures verify the compiled runtime, not real providers,
  OAuth, the actual keyring, a live desktop session or other platform packages.
