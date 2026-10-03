//! An independent session with a fake model, a fake credential resolver and a fake context
//! source. It performs no network request and needs no database, keyring or environment variable.
//!
//! Run it with `cargo run -p mira-runtime --example offline_session`.

use std::num::NonZeroUsize;
use std::sync::Arc;

use mira_runtime::mira_ai::{
    AiError, Api, AssistantEmitter, AssistantMessage, AssistantSource, BlockKind, Model, Provider,
    StopReason, StreamHandle, StreamRequest, TextBlock,
};
use mira_runtime::{
    AgentEvent, AssistantEvent, CancellationToken, ContextItem, ContextManager, ContextProvider,
    ContextProviderFuture, ContextRequest, Credential, CredentialFuture, CredentialId,
    CredentialRequest, CredentialResolver, ModelBinding, ModelRegistry, RuntimeError, Session,
    SessionConfig,
};

/// A provider that publishes one canned assistant turn.
struct FakeModel;

impl Provider for FakeModel {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
        if let Some(prompt) = request.context.system_prompt.as_ref() {
            println!("--- derived system prompt ---\n{prompt}\n-----------------------------");
        }
        let source = AssistantSource {
            api: request.model.api,
            provider: request.model.provider.clone(),
            model: request.model.id.clone(),
            response_model: None,
            response_id: None,
        };
        let message = AssistantMessage {
            source: source.clone(),
            content: vec![mira_runtime::mira_ai::AssistantContent::Text(
                TextBlock::new("The note says: water the plants."),
            )],
            stop_reason: StopReason::EndTurn,
            raw_stop_reason: None,
            usage: None,
            error_message: None,
        };
        let (mut producer, handle) =
            AssistantEmitter::new(NonZeroUsize::new(8).expect("a non-zero buffer"));
        tokio::spawn(async move {
            let events = [
                AssistantEvent::Start { source },
                AssistantEvent::BlockStart {
                    index: 0,
                    kind: BlockKind::Text,
                },
                AssistantEvent::BlockDelta {
                    index: 0,
                    delta: "The note says: water the plants.".into(),
                },
            ];
            for event in events {
                if producer.emit(event).await.is_err() {
                    return;
                }
            }
            producer.finish(Ok(message));
        });
        Ok(handle)
    }
}

/// A resolver that returns a credential the caller already owns.
struct OwnedCredential;

impl CredentialResolver for OwnedCredential {
    fn resolve<'a>(
        &'a self,
        _request: &'a CredentialRequest,
        _cancellation: CancellationToken,
    ) -> CredentialFuture<'a> {
        Box::pin(async { Ok(Credential::new("example-credential-value")) })
    }
}

/// A context provider that offers one note.
struct Notes;

impl ContextProvider for Notes {
    fn provide<'a>(
        &'a self,
        _request: &'a ContextRequest<'a>,
        _cancellation: CancellationToken,
    ) -> ContextProviderFuture<'a> {
        Box::pin(async {
            Ok(vec![ContextItem::new(
                "notes",
                "note-1",
                "Water the plants on Friday.",
            )])
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), RuntimeError> {
    // A registry with one explicitly declared binding. Nothing is discovered or guessed.
    let registry = Arc::new(ModelRegistry::single(ModelBinding::new(
        "local",
        Model::new(Api::OpenAiCompletions, "example-provider", "example-model"),
        Arc::new(FakeModel),
        CredentialId::new("example-credential"),
    ))?);

    let context = Arc::new(ContextManager::new(vec![Arc::new(Notes)]));
    let session = Session::new(
        SessionConfig::new(registry, "local", Arc::new(OwnedCredential))
            .with_system_prompt("Answer in one sentence.")
            .with_context(context),
    )?;

    let mut run = session.prompt("What is on my note?")?;
    while let Some(event) = run.recv().await {
        if let AgentEvent::MessageUpdate {
            update: AssistantEvent::BlockDelta { delta, .. },
            ..
        } = event
        {
            print!("{delta}");
        }
    }
    println!();

    let outcome = run.outcome().await?;
    println!("stop reason: {:?}", outcome.message().stop_reason);

    let usage = session.context_usage();
    println!(
        "injected {} snippet(s), {} characters (ceiling {})",
        usage.injected_items, usage.injected_chars, usage.max_injected_chars
    );
    Ok(())
}
