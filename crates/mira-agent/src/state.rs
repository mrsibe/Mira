//! Snapshot of an agent's operational state.

use mira_ai::{AssistantMessage, Message};

/// A tool call that is executing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingToolCall {
    /// Provider identifier of the call.
    pub call_id: String,
    /// Tool name the call resolves to.
    pub name: String,
}

/// What an agent is doing right now.
///
/// This is operational state, not a durable source: the agent holds it in memory, a caller
/// persists its own transcript, and a fresh process rebuilds the agent from that transcript. The
/// snapshot holds no credential.
///
/// `messages` is the transcript the agent replays. The agent finalizes every run's own messages
/// before it becomes idle, so a snapshot taken while the agent is idle has exactly one ordered
/// result for every tool call of its own turns; history set or appended by a caller is the
/// caller's responsibility to validate.
///
/// # Sensitivity
///
/// `messages` and `partial` contain prompts, memories and model output. They are sensitive and
/// must not be logged. `Debug` renders them because tests and diagnostics need it; that is not
/// permission to record them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentState {
    /// Messages committed by finished turns of current and previous runs, oldest first.
    pub messages: Vec<Message>,
    /// Assistant turn being streamed right now, if any.
    ///
    /// Display state only, never authoritative. The run commits an assistant message when the
    /// provider reports its terminal result; until then `stop_reason` is a placeholder, a
    /// tool-call block is empty until its block ends, and nothing here may be executed.
    pub partial: Option<AssistantMessage>,
    /// True while a run is in progress.
    pub busy: bool,
    /// Tool calls currently executing, in execution order.
    pub pending_tool_calls: Vec<PendingToolCall>,
}
