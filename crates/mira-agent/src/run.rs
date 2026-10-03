//! The consumer half of a run and its terminal outcome.

use std::fmt;

use mira_ai::AssistantMessage;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::event::AgentEvent;

/// Terminal result of a run that was not cancelled and did not fail.
///
/// The outcome is delivered exactly once, separately from the event stream, so a consumer that
/// stops reading events still learns how the run ended.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RunOutcome {
    /// The agent stopped after an assistant turn that requested no tool calls.
    Completed {
        /// Final assistant message of the run.
        message: AssistantMessage,
    },
    /// The agent stopped because the model hit its output token limit and requested no tool
    /// calls. The message may be cut off mid-sentence; it is not an error, and no tool call of
    /// this message was executed.
    Truncated {
        /// Final, possibly truncated, assistant message of the run.
        message: AssistantMessage,
    },
}

impl RunOutcome {
    /// The final assistant message of the run.
    pub fn message(&self) -> &AssistantMessage {
        match self {
            Self::Completed { message } | Self::Truncated { message } => message,
        }
    }
}

/// Handle of one run: its event stream, its cancellation and its terminal outcome.
///
/// A run owns a Tokio task started by [`Agent::start`](crate::Agent::start) and is cancellable
/// on its own. Dropping this handle — including dropping a half-polled
/// [`AgentRun::outcome`] future — cancels the run and releases the provider, tool and context
/// futures it was waiting on.
pub struct AgentRun {
    events: mpsc::Receiver<AgentEvent>,
    terminal: oneshot::Receiver<Result<RunOutcome, AgentError>>,
    cancellation: CancellationToken,
}

impl AgentRun {
    pub(crate) fn new(
        events: mpsc::Receiver<AgentEvent>,
        terminal: oneshot::Receiver<Result<RunOutcome, AgentError>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            events,
            terminal,
            cancellation,
        }
    }

    /// Wait for the next event.
    ///
    /// Returns `None` when no further events will arrive. The terminal outcome is delivered
    /// through [`AgentRun::outcome`], not through this iterator, so this method alone never
    /// reports how the run ended.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.events.recv().await
    }

    /// Cancel the run. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Whether the run's cancellation signal is set.
    ///
    /// True after [`AgentRun::cancel`], and true once the run has ended for any reason: the
    /// token is also the lifetime signal that releases provider, tool and context-transform work
    /// started by the run.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Cancellation handle of this run, for callers that need to observe it.
    ///
    /// Cancelling it stops the run. It is also cancelled when the run ends, whatever the
    /// outcome, so work that watches it never outlives the run it belongs to.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Consume the run and return its terminal outcome.
    ///
    /// Pending events are discarded, but they are drained first so a run waiting for buffer
    /// space can finish. The outcome arrives even when the consumer never reads an event: a run
    /// never depends on event delivery to clean up after itself.
    ///
    /// Dropping this future before it resolves drops the run and therefore cancels it.
    pub async fn outcome(mut self) -> Result<RunOutcome, AgentError> {
        loop {
            tokio::select! {
                biased;
                event = self.events.recv() => {
                    if event.is_none() {
                        break;
                    }
                }
                outcome = &mut self.terminal => {
                    return outcome.unwrap_or(Err(AgentError::Internal));
                }
            }
        }
        (&mut self.terminal)
            .await
            .unwrap_or(Err(AgentError::Internal))
    }
}

impl Drop for AgentRun {
    fn drop(&mut self) {
        // Unconditional: a dropped handle releases the run even when the terminal result was
        // already obtained (the agent's cleanup cancels the same token).
        self.cancellation.cancel();
    }
}

impl fmt::Debug for AgentRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRun")
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}
