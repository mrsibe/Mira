//! Immutable lifecycle events of one run.
//!
//! Events are progress reports. The terminal result of a run is delivered exactly once through
//! [`AgentRun::outcome`](crate::AgentRun::outcome), never as an event, so event delivery can be
//! bounded and backpressured without risking the outcome.

use mira_ai::{AssistantEvent, AssistantMessage, Message, ToolResultMessage};

/// One event of a run, in the order the run produced it.
///
/// Events carry committed messages and immutable provider updates; the agent never re-sends a
/// growing message snapshot per delta. Message content is sensitive: it must not be logged.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// The run accepted the prompt. Always the first event of a run.
    RunStart,
    /// A turn started: one provider request, plus the tool calls it requests.
    TurnStart {
        /// 1-based turn number.
        turn: u32,
    },
    /// A message was committed to the transcript: the prompt, a tool result, or an assistant
    /// turn.
    MessageStart {
        /// Turn that commits the message.
        turn: u32,
        /// Committed message.
        message: Message,
    },
    /// Incremental output of the assistant turn being streamed.
    MessageUpdate {
        /// Turn that is streaming.
        turn: u32,
        /// Immutable event the provider published.
        update: AssistantEvent,
    },
    /// A committed message is complete in the transcript.
    MessageEnd {
        /// Turn that commits the message.
        turn: u32,
        /// Committed message.
        message: Message,
    },
    /// A tool call started, whether it will execute or be answered with a synthetic error.
    ToolStart {
        /// Turn that requested the call.
        turn: u32,
        /// Provider identifier of the call.
        call_id: String,
        /// Tool name the call resolves to.
        tool: String,
    },
    /// A tool call ended.
    ToolEnd {
        /// Turn that requested the call.
        turn: u32,
        /// Provider identifier of the call.
        call_id: String,
        /// Tool name the call resolves to.
        tool: String,
        /// True when the result is an error result.
        is_error: bool,
    },
    /// A turn ended with its assistant message and the ordered results of its tool calls.
    TurnEnd {
        /// Number of the turn that ended.
        turn: u32,
        /// Assistant message of the turn.
        message: AssistantMessage,
        /// Results in the order the assistant requested the calls.
        tool_results: Vec<ToolResultMessage>,
    },
}
