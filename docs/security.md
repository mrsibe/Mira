# Security Notes

## API Keys

Current model-config saves store API keys through the operating system credential store from Rust and write the SQLite `model_configs.api_key` field as `NULL`. Provider listings return a masked indicator; explicit settings edits can retrieve the real key into frontend memory.

Startup does **not** migrate or clear legacy SQLite API-key values. Existing databases may retain keys from older versions. Treat database files and backups as sensitive regardless: they also contain private conversations and memories. Never add new credential storage to SQLite.

## Destructive Actions

The UI asks for confirmation before deleting conversations, projects, or memories. Archive is reversible; delete is not.

## Sensitive Memory Filtering

The automatic memory planner and fallback heuristics reject obvious secrets and identifiers such as API keys, passwords, tokens, ID numbers, bank cards and phone numbers. This is best-effort filtering, not a comprehensive privacy guarantee. User-saved memories also require user review.

## Network And Diagnostics

Model requests, selected history, memories and project context go to the user-configured provider; background memory extraction can use the configured background model. Updates contact the configured updater/release hosts. Local-first does not mean offline-only.

Never log API keys, authorization headers, raw provider bodies, prompts, messages or memory facts. A structured local logging/retention implementation is pending; see [engineering.md](engineering.md).
