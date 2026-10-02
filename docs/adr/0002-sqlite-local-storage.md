# ADR 0002: SQLite for Durable Data, OS Keyring for Secrets

- **Status:** Accepted
- **Date:** 2026-10-02 (retroactive record of the shipped v0.x architecture)

## Context

Mira is local-first and single-user. It needs durable storage for
conversations, messages, projects, memory facts, and model config metadata, plus
relevance search for memory and project context. API keys are sensitive and must
not be stored as plaintext in the app database.

## Decision

- Store durable app data in a local SQLite file (`mira.sqlite3`) in the app data
  directory.
- Use SQLite FTS5 virtual tables with the `trigram` tokenizer for memory and
  project-message relevance search.
- Track schema evolution with `schema_meta.schema_version` and a separate FTS
  schema version.
- Store API keys in the operating system credential store through the `keyring`
  crate (service `mira`). Current saves write `model_configs.api_key` as `NULL`
  and provider listings show a masked indicator. Existing legacy values are not
  migrated/cleared on initialization; future credential migration needs its own
  safe, tested implementation.

## Alternatives

Plain JSON files would simplify small records but make relational lifecycle and
FTS queries harder to maintain. A server database or vector store adds services
outside the single-user lightweight product boundary. These are rationale for
the documented choice, not a claim of a historical evaluation.

## Consequences

- One embedded database, no server process, no external dependency to run.
- Full-text search is available offline without a vector database.
- Database copies do not copy keyring credentials. Backups still contain private
  conversations/memories and are **not safe to share** merely because new
  credential writes avoid SQLite.
- Recovery depends on the OS credential store; a missing key surfaces as a
  credential status on the model config.
- SQLite remains the durable source of truth in the approved target
  architecture ([ADR 0003](0003-pi-runtime-direction.md)).
