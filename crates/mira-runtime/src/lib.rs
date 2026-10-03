//! Operational sessions, an explicit model registry and caller-owned context for Mira.
//!
//! `mira-runtime` is the top of the Mira library stack (`mira-runtime` → `mira-agent` →
//! `mira-ai`, see [ADR 0006](../../docs/adr/0006-rust-native-runtime.md)). It is thin composition,
//! not another orchestration framework: a [`Session`] owns exactly one `mira-agent`
//! [`Agent`](mira_agent::Agent) as its canonical operational transcript and adds the four pieces
//! the agent deliberately leaves to its caller.
//!
//! - Which model and provider to run, through an explicit [`ModelRegistry`]. Bindings are
//!   caller-defined and immutable; there is no generated catalog, no implicit discovery and no
//!   base-URL or credential assumption. Each binding also declares the identifier a
//!   [`CredentialResolver`] should use to find its credential — never the credential itself.
//! - Where the credential comes from, through the object-safe async [`CredentialResolver`], or
//!   through the explicit [`Session::prompt_with_credential`] path. Credentials are per run and
//!   are never stored in the registry, a snapshot, the configuration, an event, an error or a
//!   `Debug` rendering.
//! - What context to inject, through a [`ContextManager`] composing caller-owned
//!   [`ContextProvider`]s. Context is applied to request copies inside the agent's
//!   `ContextTransform` seam, so injected snippets never enter the canonical transcript.
//! - How long a run may take, through [`RuntimeLimits`]. One total deadline covers credential
//!   preparation, context assembly, the model run, event forwarding and backpressure.
//!
//! ```txt
//! Session::prompt(text)                  -> SessionRun
//!   SessionRun::recv()      forwards the child agent's immutable events, bounded
//!   SessionRun::outcome()   exactly one terminal RunOutcome or RuntimeError
//!   SessionRun::cancel()    independent cancellation, from before credential preparation
//! ```
//!
//! Invariants:
//!
//! - One run at a time. A concurrent prompt is refused with [`RuntimeError::Busy`] from before
//!   credential preparation instead of being queued, and configuration changes are refused the
//!   same way. Independent sessions are isolated.
//! - [`Session::prompt`] returns immediately. The returned [`SessionRun`] can be cancelled before
//!   the credential resolver has answered, and dropping its outcome future cancels the run.
//! - The session never reports idle before the child agent has repaired the transcript of its own
//!   run, for every exit path including a dropped task.
//! - No durable state, no queue, no mid-run model change and no autonomous behaviour. Persistence,
//!   keyring access and application prompts stay in the consumer.
//!
//! A complete offline run — fake provider, fake credential resolver and fake context provider — is
//! in `examples/offline_session.rs` (`cargo run -p mira-runtime --example offline_session`).
//!
//! The crate has no HTTP, Tauri, SQLite, keyring, environment or filesystem dependency: it depends
//! on `mira-agent` and `mira-ai` with `default-features = false`.

#![deny(missing_docs)]

pub mod context;
pub mod credential;
pub mod error;
pub mod registry;
pub mod session;

pub use context::{
    latest_user_query, ContextComposition, ContextError, ContextItem, ContextManager,
    ContextProvider, ContextProviderFuture, ContextRequest, ContextUsage,
    DEFAULT_MAX_INJECTED_CHARS,
};
pub use credential::{CredentialError, CredentialFuture, CredentialRequest, CredentialResolver};
pub use error::RuntimeError;
pub use registry::{CredentialId, ModelBinding, ModelRegistry};
pub use session::{
    RuntimeLimits, Session, SessionConfig, SessionRun, SessionSnapshot, DEFAULT_EVENT_BUFFER,
    DEFAULT_RUN_TIME, MAXIMUM_RUN_TIME, MAX_EVENT_BUFFER,
};

pub use mira_agent;
pub use mira_agent::{
    AgentError, AgentEvent, AgentLimits, AgentRun, ContextTransform, ProviderFailure, RunLimit,
    RunOutcome, Tool, ToolFuture, ToolRegistry, ToolResult,
};
pub use mira_ai;
pub use mira_ai::{
    Api, AssistantEvent, AssistantMessage, Context, Credential, Message, Model, ModelCapabilities,
    Provider, ReasoningLevel, RequestOptions, StopReason, Usage, UserMessage,
};
// Re-exported so a resolver or context provider implementation, and a caller that only watches
// cancellation, can name the token without depending on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;
