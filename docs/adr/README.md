# Architecture Decision Records

Short, immutable records of decisions that shape Mira. Each ADR states the
status, the decision, and the consequences. Superseded decisions stay in place.

| ADR                                      | Status                                | Decision                                                                |
| ---------------------------------------- | ------------------------------------- | ----------------------------------------------------------------------- |
| [0001](0001-tauri-desktop-shell.md)      | Accepted                              | Use Tauri 2 with a React/TypeScript frontend                            |
| [0002](0002-sqlite-local-storage.md)     | Accepted                              | SQLite for durable data, OS keyring for secrets                         |
| [0003](0003-pi-runtime-direction.md)     | Runtime dependency superseded by 0006 | Keep UI / App Core / runtime / native ownership boundaries              |
| [0004](0004-bounded-automatic-memory.md) | Accepted                              | Keep automatic memory bounded and relevance-gated                       |
| [0005](0005-extension-boundary.md)       | Accepted                              | Hold the product boundary against IDE/agent/knowledge-base scope        |
| [0006](0006-rust-native-runtime.md)      | Accepted; implemented                 | Independent MIT Rust AI/Agent/Runtime packages consumed by Mira Desktop |
| [0007](0007-bounded-pi-parity.md)        | Accepted                              | JSONL sessions and bounded client capabilities; no platform scope       |

See [../architecture.md](../architecture.md) for how these fit together.
