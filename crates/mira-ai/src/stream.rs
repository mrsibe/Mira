//! The streamed response protocol: bounded immutable events plus exactly one terminal result.
//!
//! A request produces at most one [`AssistantEvent::Start`], then content-block events, then
//! exactly one terminal outcome. The terminal outcome is delivered separately from the event
//! channel, so it is never lost when a consumer stops reading events, and a consumer that
//! stops reading applies backpressure instead of dropping events.
//!
//! Dropping the [`StreamHandle`] cancels the request and releases its HTTP work.

use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::AiError;
use crate::types::{AssistantContent, AssistantMessage, AssistantSource};

/// Default number of buffered events before the producer waits for the consumer.
pub const DEFAULT_EVENT_BUFFER: NonZeroUsize = NonZeroUsize::new(64).unwrap();

/// Largest event capacity supported by Tokio's bounded channel.
pub const MAX_EVENT_BUFFER: usize = tokio::sync::Semaphore::MAX_PERMITS;

/// Kind of content block opened by [`AssistantEvent::BlockStart`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Text content: deltas are plain text fragments.
    Text,
    /// Thinking content: deltas are reasoning text fragments.
    Thinking,
    /// Tool call: deltas are raw JSON argument fragments.
    ToolCall,
}

/// One event of the streamed response protocol.
///
/// Events are immutable values; the protocol never re-sends a growing message snapshot.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AssistantEvent {
    /// The provider accepted the request. Emitted at most once, before any block event.
    ///
    /// `source.response_model` and `source.response_id` are filled in only on the terminal
    /// message, because the provider reports them while the response streams.
    Start {
        /// Who is producing this response.
        source: AssistantSource,
    },
    /// Content block `index` started and is now the target of deltas.
    BlockStart {
        /// Position of the block in the final [`AssistantMessage::content`].
        index: usize,
        /// Kind of block that started.
        kind: BlockKind,
    },
    /// Incremental content for the open block `index`.
    BlockDelta {
        /// Position of the block this fragment belongs to.
        index: usize,
        /// Text for [`BlockKind::Text`], reasoning for [`BlockKind::Thinking`] and a raw JSON
        /// argument fragment for [`BlockKind::ToolCall`].
        delta: Arc<str>,
    },
    /// Block `index` is complete; `block` is its final immutable value.
    BlockEnd {
        /// Position of the completed block in the final message.
        index: usize,
        /// Final immutable value of the block.
        block: AssistantContent,
    },
}

/// Why an event could not be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EmitError {
    /// The terminal result was already published; no further events are accepted.
    #[error("the event stream already finished")]
    Finished,
    /// The consumer cancelled the request.
    #[error("the consumer cancelled the event stream")]
    Cancelled,
    /// The consumer dropped the stream handle; the producer must stop.
    #[error("the consumer dropped the event stream")]
    ConsumerDropped,
}

/// Consumer half of a response stream.
///
/// Dropping the handle cancels the request. Use [`StreamHandle::result`] for the terminal
/// outcome; [`StreamHandle::recv`] alone does not report failures.
pub struct StreamHandle {
    events: mpsc::Receiver<AssistantEvent>,
    terminal: oneshot::Receiver<Result<AssistantMessage, AiError>>,
    cancellation: CancellationToken,
}

impl StreamHandle {
    /// Wait for the next event.
    ///
    /// Returns `None` when no further events will arrive. The terminal outcome is delivered
    /// through [`StreamHandle::result`], not through this iterator.
    pub async fn recv(&mut self) -> Option<AssistantEvent> {
        self.events.recv().await
    }

    /// Cancel the request. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Whether the request was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Cancellation handle for the request, for callers that need to observe it.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Consume the stream and return its terminal outcome.
    ///
    /// Pending events are discarded, but they are drained first so a producer waiting for
    /// buffer space can finish.
    pub async fn result(mut self) -> Result<AssistantMessage, AiError> {
        loop {
            tokio::select! {
                biased;
                event = self.events.recv() => {
                    if event.is_none() {
                        break;
                    }
                }
                outcome = &mut self.terminal => {
                    return outcome.unwrap_or(Err(AiError::IncompleteStream));
                }
            }
        }
        (&mut self.terminal)
            .await
            .unwrap_or(Err(AiError::IncompleteStream))
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Producer half of a response stream.
///
/// Provider adapters and test fakes publish events through an emitter and finish with exactly
/// one terminal result.
pub struct AssistantEmitter {
    /// `None` once [`AssistantEmitter::finish`] closed event delivery.
    events: Option<mpsc::Sender<AssistantEvent>>,
    terminal: Option<oneshot::Sender<Result<AssistantMessage, AiError>>>,
    cancellation: CancellationToken,
}

impl AssistantEmitter {
    /// Create a linked producer/consumer pair with a bounded event buffer.
    ///
    /// # Panics
    ///
    /// Panics when `buffer` exceeds [`MAX_EVENT_BUFFER`]. Use [`Self::try_new`] for
    /// caller-supplied capacities. Provider transports use the fallible constructor.
    pub fn new(buffer: NonZeroUsize) -> (Self, StreamHandle) {
        Self::try_new(buffer).expect("event buffer exceeds the runtime capacity limit")
    }

    /// Create a stream without panicking on an unsupported capacity.
    ///
    /// Returns [`AiError::InvalidRequest`] before allocating a channel if the capacity
    /// exceeds [`MAX_EVENT_BUFFER`].
    pub fn try_new(buffer: NonZeroUsize) -> Result<(Self, StreamHandle), AiError> {
        if buffer.get() > MAX_EVENT_BUFFER {
            return Err(AiError::InvalidRequest(
                "event buffer exceeds the runtime capacity limit".to_string(),
            ));
        }
        let (event_sender, events) = mpsc::channel(buffer.get());
        let (terminal_sender, terminal) = oneshot::channel();
        let cancellation = CancellationToken::new();
        Ok((
            Self {
                events: Some(event_sender),
                terminal: Some(terminal_sender),
                cancellation: cancellation.clone(),
            },
            StreamHandle {
                events,
                terminal,
                cancellation,
            },
        ))
    }

    /// Publish one event, waiting for buffer space.
    ///
    /// Every event is delivered; a slow consumer applies backpressure instead of losing
    /// content. The wait ends early with [`EmitError::Cancelled`] when the consumer cancels the
    /// request, with [`EmitError::ConsumerDropped`] when it drops the handle, and with
    /// [`EmitError::Finished`] once [`AssistantEmitter::finish`] has published the terminal
    /// result.
    pub async fn emit(&self, event: AssistantEvent) -> Result<(), EmitError> {
        let Some(events) = self.events.as_ref() else {
            return Err(EmitError::Finished);
        };
        // Consumer drop also cancels the token. Preserve that more specific outcome,
        // but never let an available buffer slot override an established cancellation.
        tokio::select! {
            biased;
            _ = events.closed() => Err(EmitError::ConsumerDropped),
            _ = self.cancellation.cancelled() => Err(EmitError::Cancelled),
            result = events.send(event) => result.map_err(|_| EmitError::ConsumerDropped),
        }
    }

    /// Publish the terminal outcome and close event delivery.
    ///
    /// Closing the event channel is what makes a consumer's `recv` loop end, so it happens even
    /// when the producer keeps the emitter alive afterwards. Buffered events stay readable;
    /// later [`AssistantEmitter::emit`] calls fail with [`EmitError::Finished`]. Extra `finish`
    /// calls are ignored.
    pub fn finish(&mut self, result: Result<AssistantMessage, AiError>) {
        let Some(terminal) = self.terminal.take() else {
            return;
        };
        self.events = None;
        let _ = terminal.send(result);
    }

    /// Cancellation handle for this request.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Whether the consumer cancelled the stream.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Whether the consumer dropped the handle, or event delivery was already closed.
    pub fn is_consumer_gone(&self) -> bool {
        match self.events.as_ref() {
            Some(events) => events.is_closed(),
            None => true,
        }
    }
}

// Deriving Debug on a channel receiver is not possible, so provide a compact manual form.
impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamHandle")
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AssistantEmitter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AssistantEmitter")
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("consumer_gone", &self.is_consumer_gone())
            .finish_non_exhaustive()
    }
}
