# ADR 0005: Hold the Product Boundary Against IDE/Agent/Knowledge-Base Scope

- **Status:** Accepted
- **Date:** 2026-10-02

## Context

Mira is easy to fork, and "memory-aware chat client" invites scope creep toward
being an IDE, an agent orchestration platform, or a knowledge base. Each of
those directions would multiply surface area, dependencies, and maintenance
cost, and would contradict the local-first, single-user, simple-enough
principles in [../../PRODUCT.md](../../PRODUCT.md).

## Decision

Keep Mira's product and extension surface bounded:

- Mira stays a lightweight, local-first, memory-aware **chat client**.
- It is **not** an IDE: no file editing, terminals, or workspace management.
- It is **not** an agent orchestration platform: no autonomous planning,
  sub-agent dispatch, or multi-step tool workflows.
- It is **not** a knowledge base or RAG product: no vector database, document
  ingestion pipeline, or external retrieval service.
- New capability is only in scope when it fits the local-first, single-user
  product and does not turn Mira into one of the above.

## Extension Status And Alternatives

No product extension loader or permission model is implemented today. Pi's
extension/skill ecosystem is a future adapter option, not an approved arbitrary
code-execution surface. A future extension design must specify capabilities,
trust, data access, failure isolation and installation before enabling it.
Coding-agent plugins used by developers are local tooling, not Mira dependencies.

Turning Mira into an IDE, knowledge base or general orchestration platform would
be an alternative product, not a routine extension. That broader scope is
rejected in favor of a lightweight chat client.

## Consequences

- Feature and PR review can reject proposals by reference to this boundary.
- Forking remains cheap because the core stays small.
- Users who need IDE/agent/knowledge-base behavior are directed to dedicated
  tools rather than to Mira.

## Related

- [../../PRODUCT.md](../../PRODUCT.md)
- [0004](0004-bounded-automatic-memory.md)
