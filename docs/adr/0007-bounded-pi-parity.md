# ADR 0007: Bounded Pi Parity — JSONL Sessions And A Client, Not A Platform

- **Status:** Accepted
- **Date:** 2026-10-03
- **Related:** [0005](0005-extension-boundary.md) stays in force,
  [0006](0006-rust-native-runtime.md) records the package design

## Context

Mira's Rust packages now implement the AI, agent and session stack that Pi
implements in TypeScript, and Mira Desktop consumes them. The question raised
next was how far to follow Pi: whether Mira should become "Pi reimplemented in
Rust" with `mira-desktop` as its client.

Investigation of Pi at `a276dabe5` established the real shape and cost:

- Pi is 13 packages and about 170,000 source lines. `coding-agent` alone is
  84,918 lines; `ai` 26,305; `tui` 19,277; `durable` 17,662; `agent` 2,513.
- `pi-tui` is a terminal rendering library, not a client. Pi's client is
  `coding-agent`; its shipped embedding surfaces are the in-process SDK and
  JSONL RPC modes. The CBOR `protocol`/`client`/`server` packages are
  experimental dependencies of `coding-agent`.
- Pi's most distinctive capability is also its most dangerous one. Extensions
  are loaded in-process with `jiti` at full OS privileges, Pi ships no
  sandbox, and project trust only decides whether protected resources load,
  not what they may do.

Mira's existing boundaries ([ADR 0005](0005-extension-boundary.md),
[PRODUCT.md](../../PRODUCT.md)) reject IDE and agent-platform scope. Most of
what makes Pi "Pi" is exactly that scope.

## Decision

Pursue **library parity and bounded client capabilities**, and do not become an
agent platform.

**In scope**

1. A JSONL session journal following Pi's format ideas: append-only entries
   with `id`/`parentId`, a versioned header, structured messages, and a model
   context projection over the active branch. The journal is canonical for
   conversation content; SQLite keeps projects, memories, settings, provider
   metadata and credentials-by-reference, and may keep a rebuildable index
   (conversation list, titles, archive flags, full-text search).
2. Lossless message contracts, so a stored turn can be replayed with its real
   model, content blocks, stop reason, usage and provider signatures.
3. Provider breadth with explicit compatibility: additional adapters and
   same-model reasoning/thinking signature replay, never inferred from a label
   or URL.
4. Context compaction, regeneration and branching as session features, plus
   attachments.
5. The transcript transform invariants Pi applies at the model boundary:
   preserve signatures for the same model, strip them across models,
   synthesize results for orphaned tool calls, drop failed turns.

**Out of scope, requiring a new ADR to change**

6. Arbitrary code execution: no file-editing or shell tools, no in-process
   extension or plugin loading, no `skills` or prompt-template discovery that
   can execute code, no `codemode`-style sandboxes, no MCP tool supply, and no
   subagent dispatch.
7. Remote or multi-process session transport. In-process clients sharing one
   runtime are sufficient; the architecture target in
   [architecture.md](../architecture.md) already covers additional in-process
   frontends.
8. A terminal client. `pi-tui` is not ported line by line, and no TUI is
   planned.
9. Vendor-specific provider families and OAuth flows that conflict with the
   explicit-credential rule in [ADR 0006](0006-rust-native-runtime.md).

## Consequences

- ADR 0005 remains in force. Sessions, compaction, branching and attachments
  are client capabilities, not agent-platform capabilities, so they do not
  contradict it; tools, extensions and subagents still do.
- The session journal becomes a new library boundary. It must not live inside
  `mira-runtime`, whose contract states that it holds no durable state;
  Pi splits this the same way (`agent` 2,513 lines, `durable` 17,662).
- Pi's storage implementation is a format reference, not a quality bar. Its
  JSONL storage disables `fsync` by default, has no cross-process lock, sets no
  restrictive file permissions, skips malformed lines silently, derives the
  resume point from the last physical line, and repairs a torn tail by
  appending to a file a writer may still be using. Mira's implementation must
  fix each of those rather than inherit them.
- Journal field names are chosen for Mira's own compatibility rules. Byte-level
  interoperability with Pi session files is not a goal and must not be claimed.
- [PRODUCT.md](../../PRODUCT.md) and [architecture.md](../architecture.md) are
  updated when the migration ships, not when this decision is recorded.
- Full Pi parity is explicitly not a target. The largest Pi package is the one
  this ADR excludes.

## Alternatives Considered

- **Full Pi-like platform.** Rejected: it requires a capability and trust model
  first, multiplies surface area, and contradicts the local-first,
  simple-enough product definition.
- **No further parity.** Rejected: the session journal and lossless message
  contract remove real current limitations (lossy assistant replay, fixed
  twenty-message window, no branching) without changing the product boundary.
