//! Agent state, a bounded sequential tool loop and immutable run events for Mira.
//!
//! `mira-agent` sits between `mira-runtime` and `mira-ai` in the Mira library stack
//! (`mira-runtime` → `mira-agent` → `mira-ai`, see
//! [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)). It owns the operational state of one
//! conversation, drives a provider turn by turn, executes declared tools sequentially and
//! publishes immutable events. It knows nothing about Tauri, SQLite, the keyring, HTTP
//! transports, application prompts or durable storage: the provider is injected, the credential
//! is supplied per run, and persisting a session is the caller's job. SQLite, persistence and
//! credential lookup stay application-owned; this crate composes injected interfaces only.
//!
//! ```txt
//! Agent::prompt(text, credential) -> AgentRun
//!   AgentRun::recv()      bounded, lossless event stream
//!   AgentRun::outcome()   exactly one terminal RunOutcome or AgentError
//!   AgentRun::cancel()    independent cancellation of this run
//! ```
//!
//! Invariants the loop keeps:
//!
//! - One run per agent. A concurrent `start`/`prompt`, or a configuration change while a run is
//!   in progress, fails with [`AgentError::Busy`] instead of being queued.
//! - Every run is bounded: turns, processed tool calls and wall-clock time. The budgets cover the
//!   whole run, including context transforms, provider waits, tool execution and event
//!   publication; a call the run refuses without executing counts against the call budget just
//!   like one it executes.
//! - Tool calls execute only after the provider's terminal assistant message says
//!   [`StopReason::ToolUse`](mira_ai::StopReason::ToolUse) and every call's arguments parsed into
//!   a JSON object that validates against the tool's schema. A truncated turn
//!   (`MaxTokens`) never executes anything.
//! - Tool calls run sequentially, in the order the model requested them, and every call is
//!   answered by exactly one result message in that order.
//! - The agent keeps its own history replayable. When a run ends — success, failure,
//!   cancellation, deadline, budget, panic or a dropped task — cleanup signals the run's
//!   cancellation token, finalizes the messages that run appended, and only then clears the busy
//!   flag. Completed tool results are preserved; a call without a result gets a constant error
//!   result saying its outcome is unknown; a turn that cannot be replayed at all (duplicate or
//!   missing call identifiers, or a failed turn whose calls a request encoder drops) is removed.
//!   A tool is never executed during cleanup, and imported history is left to the caller.
//! - The terminal outcome is delivered once, on its own channel, and run state is cleared before
//!   it is observable — even when the consumer never reads an event, cancels, or drops the run.
//!
//! Panic containment does not install a panic hook: a contained panic is reported as a category
//! instead of a payload, but the process hook still runs, so a caller that must not log panic
//! payloads has to install its own hook.
//!
//! The crate is not a Pi port: the reference commit `a276dabe5` and the intentional differences
//! are recorded in `NOTICE` and `README.md`. No durable state is provided.
//!
//! The whole `mira-ai` protocol is reachable as [`mira_ai`], and the types used by this crate's
//! public API are re-exported at the root.
//!
//! A complete offline run — fake provider, fake calculator tool — is in
//! `examples/offline_agent.rs` (`cargo run -p mira-agent --example offline_agent`).

#![deny(missing_docs)]

mod agent;
mod agent_loop;

pub mod config;
pub mod error;
pub mod event;
pub mod run;
pub mod state;
pub mod tool;
pub mod transform;

pub use agent::Agent;
pub use config::{
    AgentConfig, AgentLimits, DEFAULT_EVENT_BUFFER, DEFAULT_MAX_TOOL_CALLS, DEFAULT_MAX_TURNS,
    DEFAULT_RUN_TIME, MAXIMUM_RUN_TIME, MAX_EVENT_BUFFER,
};
pub use error::{AgentError, ContextTransformError, ProviderFailure, RunLimit};
pub use event::AgentEvent;
pub use run::{AgentRun, RunOutcome};
pub use state::{AgentState, PendingToolCall};
pub use tool::{Tool, ToolFuture, ToolRegistry, ToolResult};
pub use transform::{ContextTransform, ContextTransformFuture};

pub use mira_ai;
pub use mira_ai::{
    AssistantEvent, AssistantMessage, Context, Credential, InputContent, Message, Model, Provider,
    RequestOptions, ToolCall, ToolDefinition, ToolResultMessage, UserMessage,
};
// Re-exported so that a [`Tool`] or [`ContextTransform`] implementation and a caller that only
// watches cancellation can name the token without depending on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;
