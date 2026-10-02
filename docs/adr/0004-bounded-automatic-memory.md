# ADR 0004: Keep Automatic Memory Bounded and Relevance-Gated

- **Status:** Accepted
- **Date:** 2026-10-02 (retroactive record of the shipped memory system)

## Context

Mira extracts long-term facts automatically. Unbounded or unconditional memory
injection would grow the system prompt without limit, degrade replies, and make
cost unpredictable. Memory also contains user-sensitive content, so
indiscriminate writing is a risk.

## Decision

Bound both memory writing and memory retrieval:

- **Retrieval is relevance-gated.** Memory injection only runs when the current
  message is likely to need memory, and only a small top-N set is injected
  (currently capped at 6 facts), selected via SQLite FTS5 and a keyword
  fallback.
- **Writes are gated and filtered.** The planner only records stable, factual,
  future-useful information, and sensitive content (API keys, passwords,
  tokens, ID numbers, bank cards, phone numbers, and similar) is checked with
  heuristic filters before automatic writing. The filters are best-effort, not
  a comprehensive privacy barrier; user-saved facts also need user review.
- **Stale memory decays.** Cleanup lowers the importance of unused, unarchived
  memories older than 45 days, so retrieval naturally favors fresh, used facts.
- **User-managed memory is protected.** Automatic writes never overwrite
  `saved` memories unless the new write is also `saved`.

## Alternatives

Injecting all memories/history grows prompts and cost. A vector service would
expand the deployment boundary. Keep local relevance search and bounded context
for this product; evaluate changes against actual recall needs rather than
adding infrastructure speculatively.

## Consequences

- Injected fact count is bounded per turn; prompt tokens and inference cost are
  not strictly bounded. A single user-saved fact can still be large.
- The user can still inspect and delete automatic memories, but does not have to
  curate them.
- Recall is best-effort by relevance, not exhaustive; a fact that never matches
  the current message will not be injected.

## Related

- [../memory-system.md](../memory-system.md)
- [0005](0005-extension-boundary.md)
