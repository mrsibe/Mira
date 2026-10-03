//! Provider and setup failures: every one of them exits before any tool runs.

use std::sync::Arc;

use mira_agent::mira_ai::{AiError, Credential, Message, StopReason, TimeoutStage};
use mira_agent::{AgentConfig, AgentError, ProviderFailure, Tool, ToolRegistry};
use serde_json::json;

mod support;
use support::*;

#[tokio::test]
async fn a_setup_failure_ends_the_run_before_any_tool_runs() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![Script::SetupFailure]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Provider(ProviderFailure::Setup)
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!events
        .iter()
        .any(|event| matches!(event, mira_agent::AgentEvent::ToolStart { .. })));
    // The prompt is committed; the failed turn never starts.
    assert_eq!(agent.snapshot().messages.len(), 1);
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn a_stream_failure_ends_the_run() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Failure(
        AiError::Transport("connection reset".to_string()),
    )]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Provider(ProviderFailure::Stream)
    );
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn a_provider_timeout_is_reported_as_a_timeout() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Failure(AiError::Timeout(
        TimeoutStage::Total,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Provider(ProviderFailure::Timeout)
    );
}

#[tokio::test]
async fn a_stream_that_ends_without_a_terminal_is_a_failure() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Abandon]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Provider(ProviderFailure::Stream)
    );
    assert!(!agent.is_busy(), "the agent is idle after a failed run");
}

#[tokio::test]
async fn a_panicking_provider_is_contained_and_clears_busy() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Panic]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    // The panic is contained: it is reported as a category and its payload is dropped.
    assert_eq!(expect_error(outcome), AgentError::Internal);
    assert!(!agent.is_busy());
    let state = agent.snapshot();
    assert!(state.partial.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(state.messages.len(), 1, "only the prompt was committed");
}

#[tokio::test]
async fn a_failed_stop_reason_ends_the_run_without_executing_tools() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);

    let mut message = call_message(
        vec![parsed_call(
            "call-1",
            "calculator",
            json!({ "a": 1, "b": 2 }),
        )],
        StopReason::Failed,
    );
    message.error_message = Some("the provider filtered this turn".to_string());
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(message)]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Provider(ProviderFailure::FailedTurn)
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!events
        .iter()
        .any(|event| matches!(event, mira_agent::AgentEvent::ToolStart { .. })));
    assert_eq!(provider.request_count(), 1);
    assert!(!agent.is_busy());

    // A failed turn is not replayed, so its calls cannot be paired either: cleanup removes the
    // turn and keeps the prompt, leaving a transcript the same agent can replay.
    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 1);
    assert!(matches!(&state.messages[0], Message::User(_)));
    assert!(calls_are_paired_in_order(&state.messages));
}

#[tokio::test]
async fn a_failing_context_transform_ends_the_run_before_the_request() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "unused",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_context_transform(Arc::new(FailingTransform)),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::ContextTransform);
    assert_eq!(provider.request_count(), 0);
    assert!(!agent.is_busy());
}

/// A panic in the middle of a run must still leave a replayable transcript behind.
#[tokio::test]
async fn a_mid_run_panic_keeps_the_history_replayable() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call(
                "call-1",
                "calculator",
                json!({ "a": 2, "b": 2 }),
            )],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("after the panic", StopReason::EndTurn)),
    ]));
    // The transform answers the tool turn and panics before the follow-up request.
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_tools(tools)
            .with_context_transform(Arc::new(PanickingTransform::new(2))),
    );

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert!(!format!("{outcome:?}").contains("PANIC-PAYLOAD"));
    assert_eq!(expect_error(outcome), AgentError::Internal);
    assert!(!agent.is_busy());
    assert_eq!(provider.request_count(), 1);
    assert_eq!(calls.lock().unwrap().len(), 1);

    // The completed tool turn stays replayable and keeps exactly one ordered result.
    let state = agent.snapshot();
    assert!(calls_are_paired_in_order(&state.messages));
    assert_eq!(
        result_counts(&state.messages),
        vec![("call-1".to_string(), 1)]
    );

    // The same agent continues once the panicking transform is gone, with no repeated effect.
    agent
        .set_context_transform(None)
        .expect("configuration is allowed once idle");
    let run = agent
        .prompt("again", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "after the panic");
    assert!(!events
        .iter()
        .any(|event| matches!(event, mira_agent::AgentEvent::ToolStart { .. })));
    assert_eq!(calls.lock().unwrap().len(), 1);
    let replay = &contexts(&provider)[1].messages;
    assert!(calls_are_paired_in_order(replay));
    assert_eq!(result_counts(replay), vec![("call-1".to_string(), 1)]);
}
