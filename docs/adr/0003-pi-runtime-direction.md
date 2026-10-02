# ADR 0003: Target the UI → Mira App Core → Pi Runtime Split

- **Status:** Accepted (App Core split pending; gateway phase 1 in ADR 0006)
- **Date:** 2026-10-02

## Context

Mira's current backend mixes app logic with inference and native concerns inside
the Tauri Rust process. As the product grows, the team wants a clearer boundary
between presentation, application logic, inference/session handling, and native
desktop capabilities. This ADR records the agreed target direction.
[ADR 0006](0006-pi-ai-sidecar.md) implements the first gateway replacement, not
the full App Core split.

## Decision

Adopt the following target layering:

```txt
UI (React / Zustand presentation)
  ↓ Application API
Mira App Core (conversation, project, memory, settings)
  ├── Pi Runtime (inference, session and model/provider adapter)
  └── Native services (SQLite, keyring, filesystem, OS integration)
```

Hard boundaries:

- **UI is presentation only.** It renders state and forwards intent; it owns no
  durable state and no inference or session logic.
- **Mira App Core** owns application logic and coordinates the UI, the runtime,
  and durable storage.
- **Pi Runtime** owns inference and session handling but is **not** the durable
  source of truth. Its session and inference state is operational, not the
  canonical record.
- **Native services** provide desktop capabilities. The Tauri native layer is
  the desktop shell, **not** the future orchestration layer.
- **Durable state** stays in local storage: SQLite for app data and the OS
  keyring for credentials. SQLite continues to hold no API keys
  ([ADR 0002](0002-sqlite-local-storage.md)).

## Alternatives

- Keep the custom Rust inference loop: smallest immediate change, but Mira must
  maintain provider/runtime compatibility itself.
- Use Vercel AI SDK or LangChain: other ecosystems, but not the selected Pi
  session/extension/skill reuse direction. No comparative benchmark is claimed.

The chosen direction reuses Pi behind an adapter while Mira retains product
policy and canonical data. Exact packages/versions, process model, packaging,
permission boundaries, migration and event/cancellation contracts need a later
integration ADR and tests; this decision does not enable autonomous tools.

## Consequences

- Future code should be placed according to its layer, not by convenience.
- Durable state must be reconstructable from SQLite and the keyring even if Pi
  Runtime session state is lost.
- The Tauri native layer must not accumulate orchestration responsibilities.

## Status Note

This is a direction, not the integration design. The phase-1 Pi AI sidecar's
protocol and packaging are now defined in [ADR 0006](0006-pi-ai-sidecar.md).
The full Application API/App Core migration remains pending.
