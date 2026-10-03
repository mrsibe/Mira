//! Minimal offline example: a fake provider drives the portable protocol.
//!
//! Run with `cargo run -p mira-ai --example offline_stream`. Nothing here contacts a provider;
//! the example shows what a `mira-agent` fake provider looks like and how a consumer reads
//! events and the terminal result. The same protocol is produced by
//! `OpenAiCompletionsProvider` over HTTP.
//!
//! To exercise a real endpoint, construct
//! `mira_ai::api::openai_completions::OpenAiCompletionsProvider` with a base URL and pass an
//! explicit `Credential`.

use std::sync::Arc;

use mira_ai::{
    AiError, Api, AssistantContent, AssistantEmitter, AssistantEvent, AssistantMessage,
    AssistantSource, BlockKind, Context, Credential, Model, Provider, StopReason, StreamHandle,
    StreamRequest, TextBlock,
};

/// A provider that replays a fixed script instead of calling a model.
struct ScriptedProvider {
    script: Vec<String>,
}

impl Provider for ScriptedProvider {
    fn stream(&self, request: StreamRequest) -> Result<StreamHandle, AiError> {
        let (mut emitter, stream) = AssistantEmitter::new(mira_ai::stream::DEFAULT_EVENT_BUFFER);
        let script = self.script.clone();
        let source = AssistantSource {
            api: request.model.api,
            provider: request.model.provider.clone(),
            model: request.model.id.clone(),
            response_model: Some(format!("{}-fake", request.model.id)),
            response_id: Some("fake-response-1".to_string()),
        };
        tokio::spawn(async move {
            let outcome = scripted_response(&emitter, source, script).await;
            emitter.finish(outcome);
        });
        Ok(stream)
    }
}

async fn scripted_response(
    emitter: &AssistantEmitter,
    source: AssistantSource,
    script: Vec<String>,
) -> Result<AssistantMessage, AiError> {
    let cancelled = emitter.cancellation();
    let mut text = String::new();
    emitter
        .emit(AssistantEvent::Start {
            source: source.clone(),
        })
        .await
        .map_err(|_| AiError::Cancelled)?;
    emitter
        .emit(AssistantEvent::BlockStart {
            index: 0,
            kind: BlockKind::Text,
        })
        .await
        .map_err(|_| AiError::Cancelled)?;
    for fragment in &script {
        tokio::select! {
            _ = cancelled.cancelled() => return Err(AiError::Cancelled),
            result = emitter.emit(AssistantEvent::BlockDelta {
                index: 0,
                delta: Arc::from(fragment.as_str()),
            }) => result.map_err(|_| AiError::Cancelled)?,
        }
        text.push_str(fragment);
    }
    let block = AssistantContent::Text(TextBlock::new(text.clone()));
    emitter
        .emit(AssistantEvent::BlockEnd { index: 0, block })
        .await
        .map_err(|_| AiError::Cancelled)?;
    Ok(AssistantMessage {
        source,
        content: vec![AssistantContent::Text(TextBlock::new(text))],
        stop_reason: StopReason::EndTurn,
        raw_stop_reason: None,
        usage: None,
        error_message: None,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), AiError> {
    let provider = ScriptedProvider {
        script: vec![
            "Mira ".to_string(),
            "runs ".to_string(),
            "offline.".to_string(),
        ],
    };
    let request = StreamRequest::new(
        Model::new(Api::OpenAiCompletions, "scripted", "scripted-model"),
        Context {
            system_prompt: Some("Be brief.".to_string()),
            ..Context::user_text("Say something")
        },
        Credential::new("not-a-real-credential"),
    );

    let mut stream = provider.stream(request)?;
    print!("streamed: ");
    while let Some(event) = stream.recv().await {
        if let AssistantEvent::BlockDelta { delta, .. } = event {
            print!("{delta}");
        }
    }
    println!();

    let message = stream.result().await?;
    println!("provider: {}", message.source.provider);
    println!("text: {}", message.text());
    println!("stop reason: {:?}", message.stop_reason);
    println!("usage reported: {}", message.usage.is_some());
    Ok(())
}
