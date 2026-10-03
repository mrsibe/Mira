# mira-agent

Agent state, a bounded sequential tool loop and immutable run events for Mira.

Requires Rust 1.88 or newer. `mira-agent` depends on `mira-ai` with
`default-features = false`, so consuming it never pulls the HTTP transport, `reqwest` or TLS.

`mira-agent` is the middle of the Mira library stack (`mira-runtime` → `mira-agent` →
`mira-ai`, see [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)). It owns the operational
state of one conversation, drives a provider turn by turn, executes declared tools sequentially
and publishes immutable events. It is usable on its own: it has no Tauri, SQLite, keyring, UI,
desktop-path, storage or application-prompt dependency, it never discovers a credential or a
model implicitly, and it provides no durable state.

```txt
Agent::prompt(text, credential) -> AgentRun
  AgentRun::recv()      bounded, lossless event stream
  AgentRun::outcome()   exactly one terminal RunOutcome or AgentError
  AgentRun::cancel()    independent cancellation of this run
```

## Scope

Shipped here:

- `Agent`, a shared (cloneable) handle owning one transcript, one model, one tool registry, one
  request-option set, one optional context transform and one finite limit set.
- `Agent::start`/`Agent::prompt` returning an independently cancellable `AgentRun` immediately.
- A turn loop that streams provider events, commits the authoritative assistant message, and
  executes tool calls sequentially after an authorized `StopReason::ToolUse` terminal.
- Agent-owned transcript finalization: a run's own messages are repaired into complete,
  replayable call/result pairs before the agent becomes idle, for every way a run can end.
- `Tool`, `ToolResult`, `ToolRegistry`: portable JSON-Schema tool declarations, object-safe
  async execution, duplicate-name rejection and offline schema validation.
- `AgentEvent`: immutable lifecycle and message events.
- `AgentState`: a snapshot of the committed transcript, the partial assistant turn, the busy flag
  and the pending tool calls.
- A pluggable `ContextTransform` that derives each request's context from the canonical
  transcript without being able to change it.
- Finite run budgets (turns, processed tool calls, wall-clock time) enforced across provider,
  transform, tool and event-publication waits.
- Agent-owned history finalization: each run's own messages are repaired into complete call/result
  pairs before the agent goes idle, whatever ended the run.

Not shipped here (and deliberately not stubbed):

- Durable storage, sessions, resumption, SQLite, keyring or credential discovery. `AgentState`
  is operational: a caller persists and restores its own transcript. SQLite, persistence and
  credential lookup stay application-owned, and `mira-runtime` is expected to compose injected
  interfaces rather than own them; this crate has no storage dependency at all.
- `mira-runtime` (context assembly, model registry, session lifecycle) and the desktop
  integration.
- Filesystem, shell, editing or other built-in tools. No tool implementation ships with this
  crate.
- Parallel tool execution, steering/follow-up queues, subagents, hooks, skills, extensions,
  OAuth, retries of tools, or any autonomous-agent platform behavior.
- HTTP providers: a `Provider` implementation is injected by the caller.

## Quick start

```rust
use mira_agent::mira_ai::{Api, Credential, Model, Provider};
use mira_agent::{Agent, AgentConfig, ToolRegistry};
use std::sync::Arc;

fn build(provider: Arc<dyn Provider>) -> Result<(), mira_agent::AgentError> {
    let config =
        AgentConfig::new(provider, Model::new(Api::OpenAiCompletions, "provider", "model"))
            .with_system_prompt("Be brief.")
            .with_tools(ToolRegistry::new());
    let agent = Agent::new(config);

    Ok(())
}

async fn run_once(
    agent: &Agent,
    credential: Credential,
) -> Result<(), mira_agent::AgentError> {
    // `prompt` returns as soon as the run is scheduled; the run owns its own cancellation.
    let mut run = agent.prompt("Hello", credential)?;
    while let Some(event) = run.recv().await {
        // Render `AgentEvent::MessageUpdate` deltas, tool starts, turn ends and so on.
        let _ = event;
    }
    // The terminal outcome is delivered exactly once, even if you never read an event.
    let _ = run.outcome().await?;
    Ok(())
}
```

`examples/offline_agent.rs` (`cargo run -p mira-agent --example offline_agent`) drives a full
tool turn with a fake provider and a calculator tool and makes no network requests.

## Public API

### `Agent` (`mira_agent::Agent`)

A shared, cloneable handle. Clones share one state; separate agents are fully independent.

| Method                                                                                                                        | Purpose                                                                             |
| ----------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `Agent::new(config)`                                                                                                          | Build an agent from `AgentConfig`.                                                  |
| `prompt(text, credential) -> Result<AgentRun, AgentError>`                                                                    | Start a run from text. Returns as soon as the run is scheduled.                     |
| `start(UserMessage, credential) -> Result<AgentRun, AgentError>`                                                              | Start a run from a user message that may carry images.                              |
| `snapshot() -> AgentState`                                                                                                    | Transcript, partial turn, busy flag and pending tool calls.                         |
| `is_busy() -> bool`                                                                                                           | Whether a run is in progress.                                                       |
| `set_model` / `set_system_prompt` / `set_tools` / `set_context_transform` / `set_options` / `set_limits` / `set_event_buffer` | Configure future runs; every setter rejects `AgentError::Busy` while one is active. |
| `append_message` / `set_history` / `clear_history`                                                                            | Edit the transcript while idle; rejected with `AgentError::Busy` while running.     |

`Agent::start` requires a Tokio runtime (`AgentError::NoRuntime` otherwise) and rejects a
concurrent start with `AgentError::Busy` atomically: starts and configuration changes are
serialized by the same lock, so no run can observe a half-applied configuration and no run can
swap its model mid-flight. Cloning the `Agent` does not create a second run slot.

### `AgentRun` (`mira_agent::AgentRun`)

| Method                                           | Purpose                                                                                                                                                                                |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `recv() -> Option<AgentEvent>`                   | Next event. Bounded channel: a slow consumer applies backpressure instead of losing events.                                                                                            |
| `outcome() -> Result<RunOutcome, AgentError>`    | The exactly-one terminal result. Discards pending events but drains them first, so a run waiting for buffer space can finish. Dropping this future before it resolves cancels the run. |
| `cancel()` / `is_cancelled()` / `cancellation()` | Cancel this run, and observe the run's cancellation/lifetime signal.                                                                                                                   |

Dropping an `AgentRun` cancels the run: the provider stream, the tool future and the context
transform future are dropped, and the agent becomes idle. The terminal outcome is delivered on
its own channel, never as an event, so it is never lost when a consumer stops reading events —
and run cleanup never depends on a consumer reading anything. The cancellation token is also the
run's lifetime signal: it is cancelled when the run ends for any reason, so a watcher handed to a
provider, tool or transform never outlives the run.

`RunOutcome` is `Completed { message }` (the model stopped without requesting tools) or
`Truncated { message }` (the model stopped at its output token limit without requesting tools;
the message may be cut off).

### `AgentEvent` (`mira_agent::AgentEvent`)

`RunStart`, `TurnStart`, `MessageStart`, `MessageUpdate`, `MessageEnd`, `ToolStart`, `ToolEnd`,
`TurnEnd`. Turn-scoped events carry a 1-based `turn`; `MessageUpdate` carries the immutable
`mira_ai::AssistantEvent` the provider published, so a delta never re-sends a growing message
snapshot. A run ends with `TurnEnd`; its terminal result arrives through `AgentRun::outcome`.
Message payloads are sensitive (prompts, memories, model output) and must not be logged.

### `Tool`, `ToolResult`, `ToolRegistry`

```rust
pub trait Tool: Send + Sync {
    fn definition(&self) -> &ToolDefinition;
    fn execute<'a>(&'a self, arguments: serde_json::Value, cancellation: CancellationToken)
        -> ToolFuture<'a>; // boxed `Send` future resolving to `ToolResult`
}
```

- `ToolDefinition` is `mira_ai::ToolDefinition`, so the declaration sent to the model is exactly
  the declaration validated locally.
- `ToolResult` carries `content: Vec<InputContent>` and `is_error`; a tool reports failure with
  `ToolResult::error(..)` instead of throwing. A tool that panics is contained — both a panic in
  `execute` itself and a panic in the future it returns — and the call becomes the single generic
  failed result `"Tool call \"NAME\" failed."`. The payload is never returned, logged by this
  crate or put in an event, but containment is not silencing: `catch_unwind` does not replace
  Rust's panic hook, so the process hook still runs and a caller that must not surface panic
  payloads has to install its own hook. This crate never installs a global hook.
- `ToolRegistry::insert` rejects duplicate names (`AgentError::DuplicateTool`) and validates the
  schema once (`AgentError::InvalidToolSchema`): the schema must be a JSON Schema object and must
  compile offline. References that would require retrieval over the network or the filesystem are
  refused; a reference that resolves inside the same schema document (a local fragment such as
  `#/$defs/item`, or an absolute URI matching an `$id` declared there) needs no retrieval and is
  accepted. The `jsonschema` dependency is compiled with all retrieval features off, so building
  a registry and validating arguments never perform I/O.
- Declarations keep registration order and are cloned per request.

### `AgentConfig`, `AgentLimits`

`AgentConfig` holds the injected `Arc<dyn Provider>`, the `Model`, an optional system prompt,
the `ToolRegistry`, an optional `Arc<dyn ContextTransform>`, `mira_ai::RequestOptions`, the
`AgentLimits`, the run event buffer and an initial transcript. `Debug` prints the model id and
counts only — never the prompt, the transcript or the provider.

`AgentLimits` defaults: `max_turns: 16`, `max_tool_calls: 64`, `total_time: 300s`. The turn
budget is checked before every provider request, the tool-call budget for a whole batch before
any call of it is processed, and the time budget on every await of the run — provider stream,
context transform, tool execution and event publication. A call the run refuses without
executing counts against the call budget exactly like one it executes, so a turn that answers
its calls with refusal results cannot bypass the budget. A zero budget fails immediately; it is
not a way to disable a limit.

The event buffer is capped at `MAX_EVENT_BUFFER`; a larger value is rejected by `start` and
`set_event_buffer` with `AgentError::InvalidEventBuffer` before the run slot is taken, instead of
panicking in the channel constructor.

### `AgentState` (`mira_agent::AgentState`)

`messages` (committed transcript), `partial` (the assistant turn being streamed), `busy`, and
`pending_tool_calls`. The snapshot is display state: `partial` is illustrative until the terminal
message commits (`stop_reason` is a placeholder and a tool-call block is empty until its block
ends), and nothing in it may be executed. `messages` is the transcript the agent replays and is
finalized before the agent goes idle (see below). Operational, not durable: this crate never
touches SQLite, a keyring or any other store, and a caller owns persistence.

### `ContextTransform`

```rust
pub trait ContextTransform: Send + Sync {
    fn transform<'a>(&'a self, context: &'a Context, cancellation: CancellationToken)
        -> ContextTransformFuture<'a>; // Result<Context, ContextTransformError>
}
```

The transform receives the canonical transcript by immutable reference and returns the context
for one request, so it cannot mutate agent state. It runs before every request, including the
first, under the run's cancellation token and deadline. A failure ends the run with
`AgentError::ContextTransform` and no request is sent. `ContextTransformError` carries no prose;
implementations record their own diagnostics.

### Errors (`mira_agent::AgentError`)

`NoRuntime`, `Busy`, `Cancelled`, `Limit(RunLimit)`, `Provider(ProviderFailure)`,
`AmbiguousToolCalls`, `ContextTransform`, `DuplicateTool`, `InvalidToolSchema`,
`InvalidEventBuffer`, `Internal`. Every category is constant: the provider's prose, the request
body, prompts, tool output, the URL and credentials never appear in an error. `ProviderFailure`
reduces a `mira_ai::AiError` to `Setup`, `Stream`, `Timeout` or `FailedTurn`; `Internal` reports a
contained panic (payload dropped) or a lost producer, and not the panic text.

## Run semantics

**Loop.** `RunStart` → `TurnStart` → commit the prompt → per turn: derive the request context
(optional transform), stream the provider response and relay `MessageUpdate`s, commit the
authoritative assistant message, then act on its stop reason.

| `stop_reason`             | What the run does                                                                                                                                                                                                              |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ToolUse` with calls      | Validates the call identifiers, then executes every call sequentially and in order.                                                                                                                                            |
| `ToolUse` without calls   | Ends the run as `Completed` (nothing to execute).                                                                                                                                                                              |
| `EndTurn` without calls   | Ends the run as `Completed`.                                                                                                                                                                                                   |
| `EndTurn` with calls      | Fails closed: no call executes; every call is answered by an ordered refusal result, and a bounded follow-up turn may re-issue complete calls.                                                                                 |
| `MaxTokens` without calls | Ends the run as `Truncated`.                                                                                                                                                                                                   |
| `MaxTokens` with calls    | Reproduces Pi's safety rule: **no** call executes (arguments may be truncated), every call is answered by an ordered refusal result, and a bounded follow-up turn may re-issue them. Arguments are never salvaged or repaired. |
| `Failed`                  | Ends the run with `AgentError::Provider(ProviderFailure::FailedTurn)`; no call executes, the turn is not replayed, and cleanup removes it when it requested calls.                                                             |

A provider setup failure, stream failure, timeout, incomplete stream or cancellation exits
before any tool runs. A provider stream that ends without a finish reason is never a success.
The whole batch of a committed turn is reserved against the call budget before any call of it is
processed, whether those calls execute or are refused; a batch that does not fit fails with
`AgentError::Limit(RunLimit::ToolCalls)` and nothing from it is processed.

**Tool execution.** Calls execute only after the terminal assistant message reports
`StopReason::ToolUse`, never from a streamed `BlockEnd`. For each call, arguments must have
parsed into a JSON object and must validate against the tool's schema; otherwise the call becomes
a synthetic error result and never reaches the tool. Unknown tool names, unparsed arguments,
schema mismatches, truncated turns, unauthorized turns, tool failures and tool panics are all
answered with an ordered `ToolResultMessage`, so the transcript keeps call/result pairs. Tool
calls with duplicate or missing identifiers cannot be paired and fail the run with
`AgentError::AmbiguousToolCalls` before anything executes. Execution is sequential in the initial
version, and a tool is never retried; tool side effects are never rolled back.

**Limits.** Reaching a budget ends the run with `AgentError::Limit(..)`, so repeated invalid
calls or a stalling provider terminate. The time budget also covers waiting for buffer space.

**Cancellation and run lifetime.** `AgentRun::cancel`, dropping the run (including dropping a
half-polled `AgentRun::outcome` future), and the run deadline all interrupt provider waits, tool
execution, context transforms and event publication. No event is published after cancellation has
been observed, and the agent is idle before the terminal outcome becomes observable. The run's
cancellation token is also its lifetime signal: cleanup cancels it on every exit path — success,
failure, cancellation, deadline, budget, contained panic and task destruction — so work a
provider, tool or transform started around that token never outlives the run.

**History finalization.** Before an agent becomes idle, it finalizes the messages its run
appended, so the transcript it replays always pairs calls with results:

- Completed assistant turns and the actual results of tools that ran are preserved exactly.
- A call without a result gets one ordered, constant error result stating that the run ended
  before the call reported a result and that whether it ran is unknown, so a caller must re-issue
  it only when repeating it is safe. Cleanup never claims a side effect did not happen and never
  executes a call.
- A turn that cannot be replayed at all is removed with only the results answering its own calls:
  duplicate or missing call identifiers, or a failed turn whose calls a request encoder drops.
  Saved user prompts and the previously completed transcript stay.
- Only the run's own messages are repaired. History installed with `set_history` or
  `append_message` is the caller's to validate.

This is why the next `prompt` of the same agent can always replay a cancelled, failed, budgeted or
dropped run safely, and why a tool that already ran is never reported as if it had not.

**Credentials.** A credential is an explicit argument of `Agent::start`/`Agent::prompt` and is
used for the requests of that run only. It is never stored in agent state, never part of an
event, an error or a `Debug` rendering (including `Agent`, `AgentRun`, `AgentConfig` and
`AgentState`), and it is never read from the environment, a file, a database or a global
registry.

## Intentional differences from Pi

`mira-agent` translates Pi's invariants, not its mechanics. Recorded differences:

- State is Rust-owned: a turn is committed as immutable values, and streamed deltas are
  `Arc<str>` fragments rather than a shared mutable `partial` object.
- The terminal result is a separate channel (`AgentRun::outcome`), not an `agent_end` event, and
  run cleanup does not depend on the consumer reading events.
- There is no `AgentMessage` union, no `convertToLlm`, no dynamic system messages and no tool
  load-out deltas announced inside the transcript: the system prompt and tools live in the
  request `Context`, as in `mira-ai`.
- Pi executes tool calls regardless of `stop_reason` (except `length`). Here only
  `StopReason::ToolUse` authorizes execution; `EndTurn` with calls fails closed.
- Tool calls with duplicate or missing identifiers fail the run instead of being executed, and
  the un-replayable turn is removed from canonical history by cleanup.
- Tool arguments are validated against the declared JSON Schema offline before execution; a
  reference that would have to be fetched is refused when the tool is registered.
- A panicking tool becomes a constant failed result rather than an error message derived from the
  thrown value, because that value is arbitrary content. This covers a panic in the tool method
  itself as well as in the future it returns.
- An interrupted run repairs its own transcript instead of leaving unpaired calls for a consumer
  or a runtime to fix, and a turn that cannot be replayed is dropped rather than sent.
- Cancellation, deadlines, turn budgets and tool-call budgets are first-class, and a run cannot
  outlive them. The call budget counts refused calls too, and the run's cancellation token is a
  lifetime signal that cleanup always sets.
- No steering queue, follow-up queue, `prepareRequest`, `prepareNextTurn`, `finishTurn`,
  `beforeToolCall`/`afterToolCall` hooks, `runToolCall`, `sessionId`, thinking budgets,
  transport selection, retry-delay cap, parallel tool execution, tool update streaming or
  `terminate` hints.
- Only `Agent::prompt`/`start` are exposed; there is no `continue()`, queue API, `reset()` or
  subscriber list. The caller reads the event stream instead of registering listeners.
- Persistence, session identity and context assembly are out of scope here: SQLite, the keyring
  and any store belong to the application.

## Testing

Every test is offline: a scripted `Provider` (`tests/support`) publishes `mira_ai` stream events
and models stalls, failures, abandoned streams and truncation; scripted tools model success,
failure, panic and stall. No test uses a live provider, a database or a keyring credential.

```bash
cargo test -p mira-agent --locked --offline
cargo clippy -p mira-agent --all-targets --locked -- -D warnings
cargo build -p mira-agent --examples --locked
cargo package --list -p mira-agent
```

Coverage includes: streaming and zero-delta turns, multi-turn tool loops with request-context
replay, execution order, truncation and unauthorized-turn refusals, unknown tools, unparsed and
schema-invalid arguments, duplicate and missing call identifiers, tool panics (both a panicking
future and a synchronously panicking method), provider setup and stream failures, failed turns, a
mid-run panic, cancellation before a request and during provider, transform and tool waits,
backpressure with a saturated buffer, a tool result that survives an interrupted publication,
dropping a half-polled outcome future, busy atomicity (including a refused oversized event
buffer), configuration rejection while running, credential containment, turn/call/time budgets
covering refused calls, and same-agent follow-up prompts after cancellation, drop, panic,
budget and ambiguous-call exits asserting one ordered result per call and no repeated effect.

## License

MIT (see `LICENSE`). This crate translates code and tests from the MIT-licensed Pi project;
`NOTICE` reproduces Pi's copyright and license, and lists the reference files. Dependencies keep
their own licenses.
