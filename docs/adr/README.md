# Architecture Decision Records

Short, immutable records of decisions that shape Mira. Each ADR states the
status, the decision, and the consequences. Superseded decisions stay in place.

| ADR                                      | Status                            | Decision                                                         |
| ---------------------------------------- | --------------------------------- | ---------------------------------------------------------------- |
| [0001](0001-tauri-desktop-shell.md)      | Accepted                          | Use Tauri 2 with a React/TypeScript frontend                     |
| [0002](0002-sqlite-local-storage.md)     | Accepted                          | SQLite for durable data, OS keyring for secrets                  |
| [0003](0003-pi-runtime-direction.md)     | Accepted (implementation pending) | Target UI → Mira App Core → Pi Runtime + native services         |
| [0004](0004-bounded-automatic-memory.md) | Accepted                          | Keep automatic memory bounded and relevance-gated                |
| [0005](0005-extension-boundary.md)       | Accepted                          | Hold the product boundary against IDE/agent/knowledge-base scope |

See [../architecture.md](../architecture.md) for how these fit together.
