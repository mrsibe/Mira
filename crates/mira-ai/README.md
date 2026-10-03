# mira-ai

Portable inference protocol and an OpenAI-compatible Chat Completions transport for Mira.

Requires Rust 1.88 or newer. This minimum applies to the independently consumed
package, not just the desktop workspace.

`mira-ai` is the bottom of the Mira library stack (`mira-runtime` → `mira-agent` →
`mira-ai`, see [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)). It is usable on its
own: it has no Tauri, SQLite, keyring, UI, desktop-path or application-prompt dependency, and
it never discovers credentials or configuration implicitly.

## Scope

Shipped here:

- Portable request/response types: model, API, capabilities, role-specific message and
  content blocks, tool calls, tool results, assistant source, stop reasons, reported usage.
- A bounded streaming protocol with exactly one terminal result per request.
- The injectable `Provider` seam, cancellation, deadlines and a redacted credential type.
- One transport: OpenAI-compatible `POST {base_url}/chat/completions` with server-sent events.

Not shipped here (and deliberately not stubbed):

- `mira-agent` and `mira-runtime` (agent state, tool loop, sessions, context composition).
- Native Anthropic Messages / Gemini GenerateContent adapters.
- Dynamic system-prompt or tool deltas across a transcript, OAuth, coding tools, skills,
  subagents, images generation or classifiers.
- A model catalog (pricing, context windows, provider autodetection). Callers supply model
  metadata; the transport never guesses limits or compatibility from a name or URL.

## Quick start

```rust
use mira_ai::api::openai_completions::{OpenAiCompletionsCompat, OpenAiCompletionsConfig, OpenAiCompletionsProvider};
use mira_ai::{Api, Context, Credential, Model, ModelCapabilities, Provider, ReasoningLevel, StreamRequest};

let mut config = OpenAiCompletionsConfig::new("https://api.deepseek.com/v1")?;
config.compat = OpenAiCompletionsCompat::deepseek();
let provider = OpenAiCompletionsProvider::new(config)?;

let mut context = Context::user_text("Hello");
context.system_prompt = Some("Be brief.".to_string());
let mut request = StreamRequest::new(
    Model::new(Api::OpenAiCompletions, "deepseek", "deepseek-reasoner")
        .with_capabilities(ModelCapabilities { reasoning: true, ..Default::default() }),
    context,
    Credential::new("sk-..."),
);
request.options.reasoning = Some(ReasoningLevel::High);

let mut stream = provider.stream(request)?;
while let Some(event) = stream.recv().await {
    // render `AssistantEvent::BlockDelta` fragments
}
let message = stream.result().await?;
```

`examples/offline_stream.rs` (`cargo run -p mira-ai --example offline_stream`) drives the
protocol with a fake provider and makes no network requests.

### Cargo features

- `openai-completions` (default): the HTTP transport. It pulls `reqwest` (rustls TLS),
  `futures-util` and `tokio` runtime support.
- Build with `default-features = false` to depend only on the protocol types, the stream
  protocol and the `Provider` seam — for example a consumer that only injects fake providers.

## Public API

### Protocol types (`mira_ai::types`, re-exported at the root)

| Type                                                              | Purpose                                                                                                                                                                         |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Api`                                                             | Transport family. `OpenAiCompletions` today; the enum is `#[non_exhaustive]`.                                                                                                   |
| `Model`, `ModelCapabilities`                                      | Model id, `api`, registry provider id, declared image/reasoning/tool capabilities. No base URL or pricing: the endpoint and credentials belong to the transport and the caller. |
| `TextBlock`, `ImageInput`, `InputContent`                         | Input parts for user messages and tool results.                                                                                                                                 |
| `ThinkingBlock`                                                   | Reasoning text with an opaque `signature` and a `redacted` flag.                                                                                                                |
| `ToolCall`                                                        | `id`, `name`, `arguments_raw` (exactly as streamed), `arguments` (strictly parsed object or `None`), opaque `signature`.                                                        |
| `AssistantContent`                                                | `Text` / `Thinking` / `ToolCall`, `#[non_exhaustive]`.                                                                                                                          |
| `UserMessage`, `AssistantMessage`, `ToolResultMessage`, `Message` | Role-specific turns. `AssistantMessage` carries `source`, `stop_reason`, `raw_stop_reason`, `usage`, `error_message` and helpers `text()` / `tool_calls()`.                     |
| `StopReason`                                                      | `EndTurn`, `MaxTokens`, `ToolUse`, `Failed`.                                                                                                                                    |
| `Usage`                                                           | `input_tokens`, `output_tokens`, optional `cached_input_tokens` and `reasoning_tokens`. Present only when the provider reported it.                                             |
| `Context`, `ToolDefinition`                                       | System prompt, message history, JSON-Schema tool declarations.                                                                                                                  |

`AssistantMessage::usage` is `None` when the provider reported no usage; counts are never
fabricated as zero. `reasoning_tokens` is a subset of `output_tokens` when reported.

### Provider seam (`mira_ai::provider`)

```rust
pub trait Provider: Send + Sync {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError>;
}
```

- Object safe and `Send + Sync`: `Arc<dyn Provider>` works.
- `StreamRequest { model, context, credential, options }`; `RequestOptions` carries
  `temperature`, `max_output_tokens`, `reasoning`, the three streaming deadlines,
  `max_retries`, `max_retry_delay` and `event_buffer`.
- `Credential` is explicit per request; it is never read from the environment or a global
  registry, its `Debug` prints `Credential([redacted])` and it does not implement `Serialize`.
- `Provider::stream` must be called inside a Tokio runtime (the transport spawns the request);
  outside one it returns `AiError::NoRuntime`. Request-building failures are returned
  synchronously, runtime failures arrive through the terminal result.

### Stream protocol (`mira_ai::stream`)

`AssistantEvent` is `Start`, then `BlockStart`/`BlockDelta`/`BlockEnd` per content block.
Events are immutable: deltas carry only the new fragment (`Arc<str>`), never a growing message
snapshot. The event channel is bounded (`RequestOptions::event_buffer`, default 64) so a slow
consumer applies backpressure instead of losing events. Provider requests reject capacities
above `MAX_EVENT_BUFFER` with `AiError::InvalidRequest` before any HTTP work.

The terminal outcome is delivered exactly once, separately from the event channel, by
`StreamHandle::result() -> Result<AssistantMessage, AiError>`:

- `Ok(message)` — the provider sent a finish reason; `message.stop_reason` carries the
  outcome, including `Failed` for provider-side failures such as `content_filter`.
- `Err(AiError)` — no usable response: transport, protocol, timeout, cancellation or a stream
  that ended without a finish reason.

`StreamHandle::cancel()` cancels; dropping the handle cancels too and releases the HTTP work.
`AssistantEmitter` is the producer half and exists so consumers can write fake providers without
the transport feature:

- `try_new` checks caller-supplied capacity and returns a typed error. `new` is the
  convenience constructor for known capacities and panics above `MAX_EVENT_BUFFER`.
- `emit` waits for buffer space, and the wait ends with `EmitError::Cancelled`,
  `EmitError::ConsumerDropped` or `EmitError::Finished` (`emit` observes cancellation itself, so
  a fake provider needs no wrapper).
- `finish` publishes the single terminal result **and closes event delivery**, so a consumer's
  `recv` loop ends even when a producer keeps its emitter alive; events published after `finish`
  are rejected.
- A producer that is dropped without finishing fails closed: the consumer sees the end of events
  and `result()` returns `AiError::IncompleteStream`, never a success.

Tool execution stays consumer-owned: events are display data, and a caller must execute a tool
only from the terminal `Ok(message)` when `stop_reason` is `StopReason::ToolUse` and the call's
`arguments` parsed (plus whatever schema validation the caller requires).

### Transport (`mira_ai::api::openai_completions`)

| Type                         | Purpose                                                                                                                                                                   |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `OpenAiCompletionsConfig`    | Validated `base_url`, `compat`, extra headers, `connect_timeout`, `max_sse_event_bytes`, `max_content_blocks`.                                                            |
| `OpenAiCompletionsCompat`    | Explicit switches: `reasoning` encoding, `supports_reasoning_effort`, `supports_usage_in_streaming`, `replay_reasoning_content`, `max_tokens_field`. Preset `deepseek()`. |
| `ReasoningEncoding`          | `ReasoningEffort` (top-level `reasoning_effort`) or `DeepSeekThinking` (`thinking: {"type":"enabled"}`, optionally with `reasoning_effort`).                              |
| `OpenAiCompletionsProvider`  | `Provider` implementation holding one shared HTTP connection pool.                                                                                                        |
| `MaxTokensField`             | `max_completion_tokens` or `max_tokens`, used only when a caller sets a cap.                                                                                              |
| `AiError`, `ProviderFailure` | Failure surface: statuses and allowlisted categories, timeouts, cancellation, protocol failures and the explicit `RetryDelayExceeded` cooldown refusal.                   |

Behavior:

- **Endpoint.** `base_url` must be an absolute `http`/`https` URL without userinfo, query or
  fragment; `/chat/completions` is appended, so a configured `/v1` prefix is preserved. A URL
  that already ends in `/chat/completions` is rejected. Error messages never echo the URL.
- **Request shape.** `model`, `messages`, `stream: true`, `stream_options.include_usage` when
  supported, `temperature` and an output-token cap only when the caller sets them, `tools`
  when the context declares them, and reasoning parameters only when requested and encodable
  (an unencodable request fails with `AiError::Unsupported` before it is sent).
- **Messages.** The system prompt is the leading system message. A user message with no image
  is sent as a compact string; with images it becomes `text` + `image_url` data parts. A model
  without image capability receives an explicit placeholder instead. Tool results are sent as
  `role: "tool"`; images from a tool result follow in a user turn. Assistant turns replay text
  and tool calls verbatim (`arguments_raw`), skip failed turns, and replay reasoning only when
  `replay_reasoning_content` is set and the message came from the same provider, API and model.
- **Response decoding.** Text, the first non-empty of `reasoning_content`/`reasoning`/
  `reasoning_text`, and interleaved fragmented `tool_calls` are decoded into `BlockKind`
  blocks. Tool arguments accumulate as text and are parsed strictly once: `arguments` is a
  JSON object or `None`. Nothing is repaired or salvaged, so a truncated `length` turn cannot
  authorize tool execution. Finish reasons map to `EndTurn` (stop/end), `MaxTokens` (length),
  `ToolUse` (tool_calls/function_call) and `Failed` (everything else, with the raw reason
  preserved). Usage-only trailing chunks are applied; a usage object missing
  `prompt_tokens` or `completion_tokens` is ignored rather than zero-filled.
- **SSE framing.** Bounded parser with arbitrary byte/Unicode fragmentation, `\n`, `\r\n` and
  lone `\r` line endings, multi-line `data` fields joined with `\n`, comments and unknown
  fields ignored, empty events skipped. An oversized event, a non-UTF-8 line, an unparsable
  payload or an in-stream provider error fails the request closed.
- **Terminal requirement.** `[DONE]` or EOF is accepted only after a finish reason. A stream
  that ends without one fails with `AiError::IncompleteStream`.
- **Retries.** Up to `max_retries` (default 2) retries, only for setup failures before any
  content is published: connection failures/timers before response headers, HTTP 429 and 5xx.
  Retries never replay published output, and there is no retry after the response body starts.
  `Retry-After`/`Retry-After-Ms` (seconds or HTTP-date) is honored up to `max_retry_delay`
  (default 5s) and up to the remaining total budget; a cooldown that does not fit is **refused**
  with `AiError::RetryDelayExceeded` instead of being shortened. The transport's own backoff is
  deterministic 500ms × 2ⁿ and is skipped when it cannot fit the remaining budget
  (`AiError::Timeout(TimeoutStage::Total)`). Cancellation interrupts the wait.
  HTTP dates accept IMF-fixdate, RFC 850 and asctime forms using `httpdate`;
  overflowing numeric cooldowns are refused, not ignored.
- **Deadlines.** `connect_timeout` (default 15s) on the shared client,
  `response_header_timeout` (45s), `stream_inactivity_timeout` (180s) and `total_timeout`
  (300s) per request; all are configurable and defaults can be lowered. The total deadline
  bounds the _whole_ lifecycle — connecting, streaming, publishing events, reading error
  bodies, retrying — so a consumer that stops draining events cannot keep a request alive past
  it; the timeout is still delivered through the terminal channel. Durations that cannot be
  scheduled (`Duration::MAX` and friends) and zero deadlines are rejected with
  `AiError::InvalidRequest` before a request is sent.
- **Redirects.** Refused (`Policy::none()`), and reported as
  `ProviderFailure::Redirect`. A provider redirect cannot forward the conversation or the
  caller's custom credential headers to another origin.
- **Errors.** Provider responses are reduced to a `ProviderFailure` category from the HTTP
  status and a small allowlist of machine-readable `code`/`type` values; the provider's own
  text is never retained or shown, and unknown finish reasons use a constant description. Error
  bodies are read only (bounded, `MAX_ERROR_BODY_BYTES`) to extract that code, and a stalled or
  unreadable error body never replaces cancellation or a timeout outcome. Raw `reqwest`
  messages (which contain the URL), response bodies, request bodies, prompts and credentials
  never appear in errors, and nothing is logged. Assistant content and
  `raw_stop_reason` remain sensitive, untrusted response data: bounding their length
  is not redaction, and consumers must not log them.

## Intentional differences from Pi

`mira-ai` translates Pi's invariants, not its mechanics. Recorded differences:

- The system prompt and tools live in `Context`, not in a transcript of system messages;
  dynamic prompt sections and mid-conversation tool additions/removals are not implemented.
- Stream events are immutable block events with a separate single terminal result; Pi reuses a
  shared mutable `partial` message and emits terminal `done`/`error` events into the stream.
- Tool arguments are parsed strictly once. Pi also parses _partial_ JSON while streaming to
  populate the live object, which can salvage incomplete arguments.
- A response stream must carry a provider finish reason. Pi can infer `stop`/`toolUse` when the
  provider is configured without finish reasons (`supportsFinishReason: false`); that flag is
  not supported here.
- Cancellation is an `Err(AiError::Cancelled)` rather than an assistant message with an
  `aborted` stop reason, because partial assistant output is not persisted.
- `Usage::input_tokens` is the provider's `prompt_tokens` verbatim and
  `cached_input_tokens` is the reported copy, instead of Pi subtracting cache reads from
  input.
- Provider `Retry-After` above the allowed wait or the remaining total budget is refused, as in
  Pi, which fails rather than shortening a server-requested cooldown. The transport adds the
  remaining-budget rule and parses HTTP-date values.
- Provider prose is never surfaced: Pi composes an error display string from the provider's
  message body, while `mira-ai` keeps only a `ProviderFailure` category (plus the HTTP status).
  This deliberately trades diagnostic text for not leaking prompts, credentials or unrelated
  provider content into user-visible errors.
- Redirects are refused. The OpenAI SDKs (and therefore Pi's fetch-based transports) follow
  redirects by default, which can move a request and its custom credential headers to another
  origin.
- Thinking is replayed only as `reasoning_content` for the same model (or dropped). Pi also
  converts reasoning to visible text on a cross-model handoff; that path is not implemented.
- Orphaned tool calls (a tool call without its result) are the caller's responsibility here;
  Pi inserts synthetic error tool results. Incomplete, failed assistant turns are skipped in
  both.
- No model catalog, pricing, cost calculation, thinking-level maps, sampling-parameter
  passthrough, prompt caching, cache-control markers, grammar-constrained tools, session
  affinity headers, `store`, Azure/Responses variants, image generation or classifiers.
- HTTP client: `reqwest` with rustls (no HTTP/2, system proxy, charset transcoding or
  compression features enabled).
- The transport does not read environment variables or maintain module-level registries.

## Testing

All transport tests are offline: they use a minimal loopback HTTP server (`tests/support`) that
scripts status codes, chunk delays, stalls, truncated bodies, redirects and credential echo. No
test uses a live provider, user database or keyring credential. Regression coverage includes
provider prose/credential echo in HTTP and SSE errors, two-endpoint redirect refusal, total
timeouts under event backpressure, retry cooldowns (in-budget, refused, HTTP-date, overflow) and
`Duration::MAX` options, stalled error bodies, and the emitter contract above
(`tests/protocol_stream.rs` runs with and without the transport feature).

```bash
cargo test -p mira-ai                              # unit + integration
cargo test -p mira-ai --all-features
cargo check -p mira-ai --no-default-features       # protocol-only build
cargo clippy -p mira-ai --all-targets --all-features
cargo build -p mira-ai --examples
cargo package --list -p mira-ai                    # verify published contents
```

## License

MIT (see `LICENSE`). This crate translates code and tests from the MIT-licensed Pi project;
`NOTICE` reproduces Pi's copyright and license, and lists the reference files. Dependencies
keep their own licenses.
