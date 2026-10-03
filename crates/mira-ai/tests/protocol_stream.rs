//! Protocol-only tests for the emitter/consumer contract.
//!
//! This file deliberately does not enable the `openai-completions` feature: it proves that the
//! portable protocol, the injectable `Provider` seam and the fake-provider emitter work without
//! the HTTP transport.

use std::num::NonZeroUsize;
use std::sync::Arc;

use mira_ai::stream::DEFAULT_EVENT_BUFFER;
use mira_ai::{
    Api, AssistantContent, AssistantEmitter, AssistantEvent, AssistantMessage, AssistantSource,
    BlockKind, Context, Credential, EmitError, Model, Provider, StopReason, StreamHandle,
    StreamRequest, TextBlock,
};

/// A fake provider, as `mira-agent` will use in its own tests.
struct ScriptedProvider {
    text: &'static str,
}

impl Provider for ScriptedProvider {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, mira_ai::AiError> {
        let (mut producer, stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
        let source = source_for(&request);
        let text = self.text;
        // A producer that retains its emitter after finishing, to cover the documented contract.
        tokio::spawn(async move {
            let _ = producer
                .emit(AssistantEvent::Start {
                    source: source.clone(),
                })
                .await;
            let _ = producer
                .emit(AssistantEvent::BlockStart {
                    index: 0,
                    kind: BlockKind::Text,
                })
                .await;
            let _ = producer
                .emit(AssistantEvent::BlockDelta {
                    index: 0,
                    delta: Arc::from(text),
                })
                .await;
            let _ = producer
                .emit(AssistantEvent::BlockEnd {
                    index: 0,
                    block: AssistantContent::Text(TextBlock::new(text)),
                })
                .await;
            producer.finish(Ok(message_for(source, text)));
            // Deliberately hold the emitter: event delivery must already be closed.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        Ok(stream)
    }
}

fn source_for(request: &StreamRequest) -> AssistantSource {
    AssistantSource {
        api: request.model.api,
        provider: request.model.provider.clone(),
        model: request.model.id.clone(),
        response_model: None,
        response_id: None,
    }
}

fn message_for(source: AssistantSource, text: &str) -> AssistantMessage {
    AssistantMessage {
        source,
        content: vec![AssistantContent::Text(TextBlock::new(text))],
        stop_reason: StopReason::EndTurn,
        raw_stop_reason: Some("stop".to_string()),
        usage: None,
        error_message: None,
    }
}

fn request() -> StreamRequest {
    StreamRequest::new(
        Model::new(Api::OpenAiCompletions, "fake", "fake-model"),
        Context::user_text("hi"),
        Credential::new("sk-fake-credential"),
    )
}

#[test]
fn unsupported_event_capacity_is_rejected_without_panicking() {
    assert!(matches!(
        AssistantEmitter::try_new(NonZeroUsize::new(usize::MAX).unwrap()),
        Err(mira_ai::AiError::InvalidRequest(_))
    ));
    assert!(AssistantEmitter::try_new(DEFAULT_EVENT_BUFFER).is_ok());
}

#[tokio::test]
async fn a_retained_emitter_still_closes_event_delivery() {
    let provider = ScriptedProvider { text: "hello" };
    let mut stream = provider.stream(request()).expect("stream");

    let mut deltas = String::new();
    let mut events = 0;
    // The producer keeps its emitter alive for 30s; this loop must still end.
    while let Some(event) = stream.recv().await {
        events += 1;
        if let AssistantEvent::BlockDelta { delta, .. } = event {
            deltas.push_str(&delta);
        }
    }
    let message = stream.result().await.expect("result");

    assert_eq!(events, 4);
    assert_eq!(deltas, "hello");
    assert_eq!(message.text(), "hello");
}

#[tokio::test]
async fn emitting_after_the_terminal_result_is_rejected() {
    let (mut producer, mut stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
    producer.finish(Err(mira_ai::AiError::IncompleteStream));

    assert_eq!(
        producer
            .emit(AssistantEvent::Start {
                source: source_for(&request()),
            })
            .await,
        Err(EmitError::Finished)
    );
    // The consumer sees no events and the terminal failure.
    assert!(stream.recv().await.is_none());
    assert_eq!(
        stream.result().await.expect_err("terminal"),
        mira_ai::AiError::IncompleteStream
    );
}

#[tokio::test]
async fn cancellation_prevents_emission_into_an_empty_buffer() {
    let (producer, mut stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
    stream.cancel();
    assert_eq!(
        producer
            .emit(AssistantEvent::Start {
                source: source_for(&request())
            })
            .await,
        Err(EmitError::Cancelled)
    );
    drop(producer);
    assert!(stream.recv().await.is_none());
}

#[tokio::test]
async fn draining_after_cancellation_does_not_reenable_emission() {
    let (producer, mut stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
    producer
        .emit(AssistantEvent::Start {
            source: source_for(&request()),
        })
        .await
        .unwrap();
    stream.cancel();
    assert!(stream.recv().await.is_some());
    for _ in 0..10 {
        assert_eq!(
            producer
                .emit(AssistantEvent::BlockDelta {
                    index: 0,
                    delta: Arc::from("late")
                })
                .await,
            Err(EmitError::Cancelled)
        );
    }
    drop(producer);
    assert!(stream.recv().await.is_none());
}

#[tokio::test]
async fn a_full_buffer_can_be_cancelled() {
    let buffer = NonZeroUsize::new(1).expect("non-zero");
    let (producer, stream) = AssistantEmitter::new(buffer);
    let source = source_for(&request());

    // Fill the buffer, then block on the next event.
    producer
        .emit(AssistantEvent::Start {
            source: source.clone(),
        })
        .await
        .expect("first event");
    let blocked = tokio::spawn({
        let producer_source = source.clone();
        async move {
            producer
                .emit(AssistantEvent::BlockStart {
                    index: 0,
                    kind: BlockKind::Text,
                })
                .await
                .map(|_| producer_source)
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    stream.cancel();
    let outcome = blocked.await.expect("task");
    assert_eq!(outcome, Err(EmitError::Cancelled));
}

#[tokio::test]
async fn dropping_the_consumer_releases_a_blocked_emit() {
    let buffer = NonZeroUsize::new(1).expect("non-zero");
    let (producer, stream) = AssistantEmitter::new(buffer);
    let source = source_for(&request());
    producer
        .emit(AssistantEvent::Start { source })
        .await
        .expect("first event");

    let blocked = tokio::spawn(async move {
        producer
            .emit(AssistantEvent::BlockStart {
                index: 0,
                kind: BlockKind::Text,
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(stream);

    assert_eq!(
        blocked.await.expect("task"),
        Err(EmitError::ConsumerDropped)
    );
}

#[tokio::test]
async fn a_dropped_producer_fails_closed_without_a_terminal_result() {
    let (producer, mut stream) = AssistantEmitter::new(DEFAULT_EVENT_BUFFER);
    let source = source_for(&request());
    producer
        .emit(AssistantEvent::Start { source })
        .await
        .expect("event");
    // A producer that panics or is dropped must not look like a successful completion.
    drop(producer);

    assert!(stream.recv().await.is_some());
    assert!(stream.recv().await.is_none());
    assert_eq!(
        stream.result().await.expect_err("no terminal"),
        mira_ai::AiError::IncompleteStream
    );
}

#[tokio::test]
async fn a_finished_producer_delivers_buffered_events_then_the_terminal_result() {
    let buffer = NonZeroUsize::new(8).expect("non-zero");
    let (mut producer, mut stream) = AssistantEmitter::new(buffer);
    let source = source_for(&request());
    for index in 0..3 {
        producer
            .emit(AssistantEvent::BlockDelta {
                index,
                delta: Arc::from("x"),
            })
            .await
            .expect("event");
    }
    producer.finish(Ok(message_for(source, "done")));

    let mut seen = 0;
    while let Some(_event) = stream.recv().await {
        seen += 1;
    }
    assert_eq!(seen, 3);
    assert_eq!(stream.result().await.expect("result").text(), "done");
}

#[tokio::test]
async fn result_drains_a_backpressured_producer() {
    // A consumer that only asks for the outcome must not deadlock the producer.
    let buffer = NonZeroUsize::new(1).expect("non-zero");
    let (mut producer, stream) = AssistantEmitter::new(buffer);
    let source = source_for(&request());
    tokio::spawn(async move {
        for index in 0..12 {
            let _ = producer
                .emit(AssistantEvent::BlockDelta {
                    index,
                    delta: Arc::from("x"),
                })
                .await;
        }
        producer.finish(Ok(message_for(source, "done")));
    });

    assert_eq!(stream.result().await.expect("result").text(), "done");
}
