# ADR 0006: Composable Rust AI Packages and Mira Desktop

- **Status:** Accepted; libraries, desktop integration and directory migration implemented and independently reviewed
- **Date:** 2026-10-03
- **Supersedes:** ADR 0003's dependency on the TypeScript Pi runtime, not its data-ownership boundaries

## Decision

Build a Pi-inspired, Rust-native library stack. Mira Desktop is a consumer, not
an implicit dependency of the libraries. Use one monorepo for coordinated API
and test changes; extracting or publishing repositories/packages is not part of
this decision and requires separate authorization.

```txt
mira-desktop (React + Tauri, application policies, SQLite and keyring)
  -> mira-runtime (operational sessions, context assembly, model registry)
       -> mira-agent (agent state, bounded tool loop, events)
            -> mira-ai (messages, provider adapters, streaming, usage)
```

The layout is `crates/{mira-ai,mira-agent,mira-runtime}` and
`apps/mira-desktop`. Directory migration was a separately validated integration
step, not a prerequisite for implementing or testing the libraries. Storage, memory and
project modules remain application-owned until an independently useful package
boundary is justified.

## Package boundaries

- Packages have no Tauri, SQLite, keyring, UI or desktop configuration dependency.
  No application paths, Mira prompts, database entities or implicit credential
  discovery occur inside these packages.
- `mira-ai` is independently usable for inference. `mira-agent` uses its portable
  protocol and an injectable provider; `mira-runtime` composes both. Dependencies
  flow down only, with no cycles or global mutable provider registration.
- Credentials and context sources are explicit caller inputs or injected
  interfaces. HTTP connection pools may be shared; authorization is per request.
- Each package includes public API documentation, an example, its own tests and
  publishable metadata. Third-party consumers do not build the desktop app.
- First-party code is MIT. Preserve Pi's MIT notice for translated code/tests.
  Third-party components retain their licenses; this decision does not relicense
  dependencies or grant rights to contributions the operator does not own.

## Reference and quality

Reference the local Pi checkout at commit `a276dabe5`, especially
`packages/ai/src/{types.ts,utils/event-stream.ts,utils/transcript.ts,
api/transform-messages.ts,api/openai-completions.ts}` and
`packages/agent/src/{types.ts,agent.ts,agent-loop.ts}`. Record intentional
simplifications. Translate relevant offline tests, not just API names.

Use Rust ownership and immutable event payloads rather than JavaScript shared
mutable partial objects. Principal event delivery is bounded and lossless;
optional observers must not be the sole owner of terminal notifications.
Each run has an independent cancellation handle and exactly one terminal outcome.
Unexpected EOF is not successful completion. Dropping the consumer stops work.
Cancellation must interrupt header waits, streaming and retry backoff.

Tool arguments are accumulated until complete and schema-validated before
execution. Truncated model output never authorizes tool execution. Start with
sequential tools, finite turn/tool/time limits, no steering/follow-up queues,
OAuth, coding tools, skills, extensions or subagents.

## Migration and scope

Steps 1–3 are implemented and independently reviewed. Local SDK/native/frontend
checks, an outside-workspace consumer and a Tauri debug build passed. Publication,
GUI/restart/keyring acceptance and multi-platform release verification were not
performed. Step 4 remains planned.

1. Implement `mira-ai` with OpenAI-compatible Chat Completions and explicit
   capability/compatibility settings. Support text/thinking, image inputs,
   tool-call parsing, reported usage, bounded SSE framing and safe errors.
2. Implement `mira-agent` and `mira-runtime` with fake-provider/tool tests,
   sessions, context composition and independent cancellation.
3. Route both Mira chat and background memory inference through the libraries.
   Keep frontend IPC, prompt formatting, the last-20-message window, credential
   storage and memory fallback. Move the desktop into `apps/mira-desktop` in a
   separately validated integration step.
4. Add native Anthropic Messages and Gemini GenerateContent adapters with
   signature replay and tool-loop fixtures. These are planned, not shipped.

Existing provider labels must not silently select a new API. Existing Base URL
settings continue to mean `{base_url}/chat/completions`; DeepSeek reasoner/v4
thinking behavior remains. Do not fabricate context-window limits, usage or
pricing. Retry only retryable failures before generated updates have been
published; do not replay partial output or tool side effects.

SQLite remains canonical. Sessions are operational and reconstructable.
Cancelled runs preserve the saved user message but do not persist partial
assistant output or trigger its memory pass. Chat and memory runs are isolated.
Mira declares no executable tools. Tool support in reusable packages does not
expand the desktop product into an autonomous agent platform.

Regeneration, message editing, branching, attachment UI and structured transcript
persistence are separate work: the current text/reasoning schema cannot represent
complete native-provider tool/signature history.

## Validation

Use synthetic credentials, fake providers/tools and loopback HTTP fixtures only;
never use a live provider, user database or actual keyring credentials. Validate
SSE fragmentation/Unicode, terminal and error ordering, invalid/oversized data,
retry limits, stalled cancellation, tool truncation/validation, resource cleanup,
request isolation, prompt parity and memory fallback. Run independent review of
major phases and a third-party consumer example without Tauri dependencies.

The operator authorized removal of Playwright tests, dependencies and configs.
Vitest IPC/store checks and Rust tests remain; rendered UI/native acceptance are
manual. Removing browser automation does not prove desktop correctness.

## Consequences

Provider compatibility maintenance becomes this project's responsibility. The
Rust packages are not a full Pi port. Installation size and performance changes
must be measured, not assumed. Package publication, remote repository creation,
commits, pushes and releases require separate authorization.
