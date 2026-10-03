# mira-runtime

Operational sessions, an explicit model registry and caller-owned context for Mira.

Requires Rust 1.88 or newer. `mira-runtime` depends on `mira-agent` and `mira-ai` with
`default-features = false`, so consuming it never pulls the HTTP transport, `reqwest` or TLS.

`mira-runtime` is the top of the Mira library stack (`mira-runtime` → `mira-agent` → `mira-ai`,
see [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)). It is a thin composition layer, not
an orchestration framework: a `Session` owns exactly one `mira-agent` `Agent` as its canonical
operational transcript and adds the four pieces the agent leaves to its caller.

```txt
Session::prompt(text)                  -> SessionRun
  SessionRun::recv()      forwards the child agent's immutable events through a bounded channel
  SessionRun::outcome()   exactly one terminal RunOutcome or RuntimeError
  SessionRun::cancel()    independent cancellation, from before credential preparation
```

There is no Tauri, SQLite, keyring, UI, filesystem, environment or desktop-path dependency, no
implicit credential discovery, no generated model catalog, no durable storage and no logging.

## Scope

Shipped here:

- `Session`, owning one operational transcript, one selected binding and one run slot. Concurrent
  prompts and configuration changes are refused with `Busy` from before credential preparation.
- `ModelRegistry` / `ModelBinding`: an immutable, caller-constructed set of bindings with unique
  keys. A binding carries a `mira-ai::Model` identity, the exact injected `Arc<dyn Provider>`,
  default `RequestOptions` and a credential _lookup identifier_ — never a credential. The registry
  never constructs a provider, reads a base URL, generates a catalog or guesses token limits or
  pricing.
- `CredentialResolver`: an object-safe async seam that receives the selected binding and model
  identity plus a cancellation token and returns a `Credential` or a constant `CredentialError`.
  `Session::prompt_with_credential` is the explicit, resolver-free path for callers that already
  hold a credential.
- `ContextManager` / `ContextProvider` / `ContextItem`: generic, deterministic snippet composition
  applied to request copies inside the child agent's `ContextTransform` seam, under a finite
  injected-_character_ budget.
- `SessionRun`: an independently cancellable run handle with a bounded event stream and exactly one
  terminal outcome on a separate channel.
- `RuntimeLimits`: one total deadline covering credential preparation, context assembly, the model
  run, forwarding and backpressure, plus a bounded event buffer.
- `Session::snapshot()` and `Session::context_usage()`: operational state and last-reported counts.

Not shipped here (and deliberately not stubbed):

- Durable storage, session identity, persistence, SQLite, keyring access or credential discovery.
  A session is operational: the caller persists its own transcript and rebuilds a session from it.
- Concrete memory, project or attachment context sources. Context sources are application-owned and
  injected explicitly; only the generic injection seam ships.
- Native Anthropic Messages / Gemini GenerateContent adapters, provider catalogs, pricing or cost
  calculation, token counting or context-window guessing.
- Queues, steering, mid-run model changes, autonomous scheduling, subagents, hooks, skills,
  extensions, OAuth or coding tools.
- HTTP: a `Provider` implementation is injected by the caller.

## Quick start

```rust
use std::sync::Arc;

use mira_runtime::mira_ai::{Api, Model};
use mira_runtime::{
    Credential, CredentialFuture, CredentialId, CredentialRequest, CredentialResolver,
    ModelBinding, ModelRegistry, RuntimeError, Session, SessionConfig, CancellationToken,
};

struct OwnedCredential;

impl CredentialResolver for OwnedCredential {
    fn resolve<'a>(
        &'a self,
        _request: &'a CredentialRequest,
        _cancellation: CancellationToken,
    ) -> CredentialFuture<'a> {
        Box::pin(async { Ok(Credential::new("caller-owned-secret")) })
    }
}

async fn run(provider: Arc<dyn mira_runtime::Provider>) -> Result<(), RuntimeError> {
    let registry = Arc::new(ModelRegistry::single(ModelBinding::new(
        "main",
        Model::new(Api::OpenAiCompletions, "provider", "model"),
        provider,
        CredentialId::new("main-credential"),
    ))?);
    let session = Session::new(SessionConfig::new(registry, "main", Arc::new(OwnedCredential)))?;

    let mut run = session.prompt("Hello")?;
    while let Some(event) = run.recv().await {
        // Render `AgentEvent` deltas, tool starts and turn ends.
        let _ = event;
    }
    // The terminal outcome is delivered exactly once, even if you never read an event.
    let _ = run.outcome().await?;
    Ok(())
}
```

`examples/offline_session.rs` (`cargo run -p mira-runtime --example offline_session`) runs a full
turn with a fake provider, a fake credential resolver and a fake context provider, and makes no
network request.

## Public API

### `ModelRegistry`, `ModelBinding`, `CredentialId`

```rust
pub struct ModelBinding { /* key, model, provider, options, credential_id */ }

impl ModelBinding {
    pub fn new(key: impl Into<String>, model: Model, provider: Arc<dyn Provider>,
               credential_id: CredentialId) -> Self;
    pub fn with_options(self, options: RequestOptions) -> Self;
    pub fn key(&self) -> &str;
    pub fn model(&self) -> &Model;
    pub fn provider(&self) -> &Arc<dyn Provider>;
    pub fn options(&self) -> &RequestOptions;
    pub fn credential_id(&self) -> &CredentialId;
}
```

`ModelRegistry::new(Vec<ModelBinding>)` validates once and rejects duplicate keys
(`RuntimeError::DuplicateBinding`) and empty keys or model ids (`RuntimeError::InvalidBinding`).
`ModelRegistry::get`, `keys`, `len`, `contains_key`. The registry is shared as
`Arc<ModelRegistry>`; there is no global mutable provider registration.

`CredentialId` is an identifier, not a credential. It is printed by `Debug` and may be shown to a
user, so it must not contain a secret.

### `CredentialResolver`

```rust
pub trait CredentialResolver: Send + Sync {
    fn resolve<'a>(&'a self, request: &'a CredentialRequest, cancellation: CancellationToken)
        -> CredentialFuture<'a>; // Result<Credential, CredentialError>
}
```

`CredentialRequest` carries the selected binding key, the `Model` identity and the binding's
`CredentialId`. `CredentialError` is a constant category with no prose. The runtime never reads the
environment, a file, a database or a keyring; the resolver implementation belongs to the consumer
and must observe the cancellation token. `Session::prompt_with_credential` bypasses the resolver
for callers that already hold a credential.

### `ContextManager`, `ContextProvider`, `ContextItem`, `ContextUsage`

```rust
pub trait ContextProvider: Send + Sync {
    fn provide<'a>(&'a self, request: &'a ContextRequest<'a>, cancellation: CancellationToken)
        -> ContextProviderFuture<'a>; // Result<Vec<ContextItem>, ContextError>
}
```

- `ContextRequest` carries the selected `Model`, the latest original user query
  (`latest_user_query`) and the canonical `Context` of the request.
- `ContextItem` is `source`, `id`, `text`, `priority` and an informational `token_estimate`.
- `ContextManager::new(providers)` composes them; `with_max_injected_chars` sets the ceiling
  (default `DEFAULT_MAX_INJECTED_CHARS`, 16 384 characters). `compose` returns a
  `ContextComposition` with the derived context and the exact counts.

The manager is immutable and stateless, so several sessions may share one `Arc<ContextManager>`;
counts are reported per composition and each session keeps its own.

Composition is deterministic: items are ordered by descending `priority`, then by provider
registration order, then by the order the provider returned them. Items are included whole while the
accumulated **rendered** character count stays within the ceiling; an item that does not fit is
skipped and a later smaller item may still be included. The ceiling counts every character the
composition adds to the derived system prompt — the provenance label, the item text and the
separators between snippets, including the separator after a preserved base prompt — so a long label
cannot smuggle text past it. The count is in Unicode scalar values, not bytes and not tokens, and it
never truncates the canonical current user message or history.

The selected snippets are appended once, labeled, to a **derived** copy of the system prompt:

```txt
{base system prompt}

[context source={source} id={id}]
{text}
```

The canonical transcript, the canonical system prompt and the tool declarations are untouched, so a
snippet is never replayed, duplicated across a tool loop or persisted. `ContextComposition` reports
the injected character and item counts of one composition; `Session::context_usage()` adds the last
usage a provider actually reported and is owned by the session, so sharing a manager cannot mix two
sessions' metrics. No count is fabricated and no character is converted into a token.

Provider failure is reported as the constant `ContextError`, which ends the run with
`RuntimeError::Context`; raw provider errors are never surfaced. A provider that returns nothing is
an exact no-op, so a caller that already assembles its own context can keep zero providers.

**Trust.** A `ContextItem`'s text is data, not policy. It is placed in the system prompt and can
influence the model. The context manager never adds an executable tool and never changes the tool
policy: the tool set of a run is exactly the selected agent's registry.

### `Session`, `SessionConfig`, `RuntimeLimits`

| Method                                                                                                                                       | Purpose                                                                                   |
| -------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `Session::new(config)`                                                                                                                       | Validate the configuration and build the child agent.                                     |
| `prompt(text)` / `start(UserMessage)`                                                                                                        | Start a run, resolving the credential through the configured resolver.                    |
| `prompt_with_credential(text, credential)` / `start_with_credential(UserMessage, credential)`                                                | Start a run with an explicit credential, bypassing the resolver.                          |
| `snapshot() -> SessionSnapshot`                                                                                                              | Selected binding, busy state and the canonical transcript.                                |
| `is_busy() -> bool`                                                                                                                          | True while a run is in progress **or** the child agent is still repairing its transcript. |
| `binding() -> String`                                                                                                                        | The currently selected binding key.                                                       |
| `context_usage() -> ContextUsage`                                                                                                            | This session's last composition counts plus the last reported model usage.                |
| `set_model(key)` / `set_thinking(Option<ReasoningLevel>)` / `set_history(Vec<Message>)` / `set_context_manager(Option<Arc<ContextManager>>)` | Idle-only configuration changes; every setter rejects `RuntimeError::Busy`.               |

`SessionConfig` holds the registry, the selected binding key, the tool implementations, the optional
context manager, the optional per-request option override, the requested reasoning effort, the
system prompt, the child agent limits, the runtime limits, the initial transcript and the resolver.
`Debug` prints the selection and counts only — never a prompt, a credential or the transcript.

`set_model` rebuilds the child agent, preserving the transcript taken from its snapshot, because the
agent owns its transcript and has no provider setter. `set_thinking`, `set_history` and
`set_context_manager` update the idle agent in place.

`RuntimeLimits` is `total_time` (default `DEFAULT_RUN_TIME`, 300 s, at most `MAXIMUM_RUN_TIME`) and
`event_buffer` (default `DEFAULT_EVENT_BUFFER`, at most `MAX_EVENT_BUFFER`). Both are validated
before the run slot is taken, so an out-of-range budget is a typed error
(`RuntimeError::InvalidRunTime`, `RuntimeError::InvalidEventBuffer`) instead of a panic or an
overflowing deadline.

### `SessionRun`

| Method                                           | Purpose                                                                                                                                  |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `recv() -> Option<AgentEvent>`                   | Next child agent event. Bounded: a slow consumer applies backpressure instead of losing events.                                          |
| `outcome() -> Result<RunOutcome, RuntimeError>`  | The exactly-one terminal result. Discards pending events but drains them first. Dropping this future before it resolves cancels the run. |
| `cancel()` / `is_cancelled()` / `cancellation()` | Cancel the run, and observe the run's cancellation/lifetime signal.                                                                      |

Dropping a `SessionRun` cancels the run — even before the credential resolver has answered, so
nothing is started. The terminal outcome is delivered on its own channel, never as an event.

### Errors (`mira_runtime::RuntimeError`)

`NoRuntime`, `Busy`, `Cancelled`, `Timeout`, `Credential`, `Context`, `UnknownBinding(String)`,
`DuplicateBinding(String)`, `InvalidBinding(String)`, `Agent(AgentError)`, `InvalidRunTime`,
`InvalidEventBuffer`, `Internal`. Every category is constant: provider prose, context snippets,
prompts, the request body and credentials never appear in an error. `Agent(AgentError)` exposes the
same constant categories as `mira-agent`; a child time limit and the runtime deadline both map to
`Timeout`.

## Run semantics

**One run at a time.** The run slot is taken before credential preparation, so a concurrent prompt
is refused with `Busy` instead of being queued and instead of racing the resolver. Configuration
changes are refused the same way. Independent sessions are isolated.

**Bounded and cancellable.** `Session::prompt` returns immediately. The run driver resolves the
credential under the run's cancellation token and total deadline, starts the child agent, forwards
its immutable events through a bounded channel, and ends at the deadline even when a consumer stops
reading. There is no queue and no mid-run model change.

**Cleanup.** Every exit path drives the child agent to a terminal outcome: normally by reading it,
and otherwise by cancelling it and draining its outcome. The session never reports idle before the
child agent has repaired the transcript of its own run, and the child agent's own busy flag is part
of `is_busy`, `snapshot` and every setter — so a run whose task was dropped still refuses a
concurrent prompt until its cleanup settles.

**Credentials.** A credential is produced per run and used for that run only. It is never stored in
the registry, a session snapshot, the configuration, an event, an error or a `Debug` rendering, and
it is never read from the environment, a file, a database or a global registry.

## Intentional differences from Pi

`mira-runtime` follows Pi's session and agent boundaries, not its mechanics. Recorded differences:

- A session owns one `mira-agent` agent as its canonical transcript instead of a separate message
  store; there is no second copy to keep in sync and no `AgentMessage` union.
- The model registry is caller-constructed and immutable. Pi resolves providers and credentials from
  a registry and settings store; here every binding, provider, option and credential identifier is
  explicit and there is no discovery, no catalog and no base-URL assumption.
- Credentials are an explicit resolver seam or an explicit argument, never auto-discovered from the
  environment, a keyring, a file or OAuth.
- Context is composed inside the agent's `ContextTransform` seam under a finite character budget.
  Pi composes prompt sections and memory in the application; those concrete sources stay
  application-owned here.
- One run at a time with a single terminal outcome and independent cancellation, instead of an
  event-driven session with listeners, steering and follow-up queues.
- `RuntimeError` and the context/credential failures are constant categories; Pi surfaces provider
  and tool error text.
- No durable session storage, no session identity, no resume, no branching, no compaction, no hooks,
  no skills, no extensions, no subagents and no autonomous scheduling.
- No HTTP dependency: a `Provider` is injected.

## Testing

Every test is offline: scripted providers publish `mira-ai` stream events, a scripted resolver
models success, constant failure, a stalled lookup and a panic, and scripted context providers model
items, constant failure and a stall. No test uses a live provider, a database or a keyring
credential.

```bash
cargo test -p mira-runtime --locked --offline
cargo clippy -p mira-runtime --all-targets --locked -- -D warnings
cargo build -p mira-runtime --examples --locked
cargo run -p mira-runtime --example offline_session
cargo package --list -p mira-runtime
```

Coverage includes: a single streamed turn with a terminal outcome; provider routing by binding;
history preservation and freshly resolved credentials and options across an idle model switch;
thinking parameters; explicit-credential prompting; isolation of credentials, selection and context
across sessions; busy refusal of a concurrent prompt and of every setter; dropping a half-polled
outcome future; a total-deadline timeout under event backpressure; out-of-range run budgets;
unregistered binding keys; a panicking resolver that leaks no busy flag; resolver failure and
cancellation before the agent starts; context injection once per request with no canonical
pollution; deterministic priority and rendered-character-budget selection including label-only and
Unicode snippets and the base-prompt separator; session-owned metrics under a shared manager; an
empty provider being an exact no-op; context failure and cancellation; tools never added by
context; and credential containment in `Debug`, events and errors.

## License

MIT (see `LICENSE`). This crate follows Pi designs; `NOTICE` reproduces Pi's copyright and license
and records the reference revision. Dependencies keep their own licenses.
