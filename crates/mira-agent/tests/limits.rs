//! Finite budgets and context derivation.

use std::sync::Arc;
use std::time::Duration;

use mira_agent::mira_ai::{Credential, Message, StopReason, UserMessage};
use mira_agent::{AgentConfig, AgentError, AgentLimits, RunLimit, Tool};
use serde_json::json;

mod support;
use support::*;

#[tokio::test]
async fn the_turn_limit_stops_a_tool_loop() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let tool_turn = || {
        Script::Stream(call_message(
            vec![parsed_call("call", "calculator", json!({ "a": 1, "b": 1 }))],
            StopReason::ToolUse,
        ))
    };
    let provider = Arc::new(FakeProvider::new(vec![
        tool_turn(),
        tool_turn(),
        tool_turn(),
    ]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_tools(tools)
            .with_limits(AgentLimits {
                max_turns: 2,
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("loop", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Turns));
    assert_eq!(provider.request_count(), 2);
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn the_tool_call_limit_refuses_a_whole_batch() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![
            parsed_call("call-1", "calculator", json!({ "a": 1, "b": 1 })),
            parsed_call("call-2", "calculator", json!({ "a": 2, "b": 2 })),
        ],
        StopReason::ToolUse,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model())
            .with_tools(tools)
            .with_limits(AgentLimits {
                max_tool_calls: 1,
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("batch", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Limit(RunLimit::ToolCalls)
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "no call of the batch runs"
    );
}

#[tokio::test(start_paused = true)]
async fn the_time_limit_stops_a_stalled_provider() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = mira_agent::Agent::new(AgentConfig::new(provider, test_model()).with_limits(
        AgentLimits {
            total_time: Duration::from_secs(1),
            ..AgentLimits::default()
        },
    ));

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Time));
    assert!(!agent.is_busy());
}

#[tokio::test(start_paused = true)]
async fn the_time_limit_stops_a_stalled_tool() {
    let (tool, observed) = StallingTool::new();
    let tools = registry(vec![Arc::new(tool) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![parsed_call("call-1", "stalling", json!({}))],
        StopReason::ToolUse,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model())
            .with_tools(tools)
            .with_limits(AgentLimits {
                total_time: Duration::from_secs(1),
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Time));
    // Cleanup signals the run's cancellation token, so a watcher inside the tool is released
    // even though the run ended on its deadline rather than by cancellation.
    observed
        .await
        .expect("the stalled tool was released by the deadline");
    assert!(agent.snapshot().pending_tool_calls.is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_time_limit_releases_provider_watchers() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model()).with_limits(AgentLimits {
            total_time: Duration::from_secs(1),
            ..AgentLimits::default()
        }),
    );

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    while let Some(event) = run.recv().await {
        if matches!(
            event,
            mira_agent::AgentEvent::MessageStart {
                message: Message::Assistant(_),
                ..
            }
        ) {
            break;
        }
    }
    let stalled = provider
        .take_stall_signal()
        .expect("the provider is streaming");

    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Time));
    stalled
        .await
        .expect("the stalled provider was released by the deadline");
}

#[tokio::test(start_paused = true)]
async fn the_time_limit_stops_a_stalled_context_transform() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "never",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_context_transform(Arc::new(StallingTransform))
            .with_limits(AgentLimits {
                total_time: Duration::from_secs(1),
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Time));
    assert_eq!(provider.request_count(), 0);
}

#[tokio::test]
async fn a_zero_turn_budget_fails_before_the_first_request() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "never",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model()).with_limits(AgentLimits {
            max_turns: 0,
            ..AgentLimits::default()
        }),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Limit(RunLimit::Turns));
    assert_eq!(provider.request_count(), 0);
}

#[tokio::test]
async fn an_unrepresentable_time_budget_is_clamped_and_the_run_still_works() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "ok",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(AgentConfig::new(provider, test_model()).with_limits(
        AgentLimits {
            total_time: Duration::MAX,
            ..AgentLimits::default()
        },
    ));

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "ok");
}

#[tokio::test]
async fn a_transform_derives_every_request_without_touching_the_transcript() {
    let calculator = Arc::new(Calculator::new());
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let transform = Arc::new(RecordingTransform::new(|context| {
        let mut derived = context.clone();
        derived
            .messages
            .push(Message::User(UserMessage::text("derived-context")));
        derived
    }));
    let recorded = transform.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call(
                "call-1",
                "calculator",
                json!({ "a": 1, "b": 1 }),
            )],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("done", StopReason::EndTurn)),
    ]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_tools(tools)
            .with_context_transform(transform),
    );

    let run = agent
        .prompt("compute", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "done");

    // The transform ran once per request and always saw the canonical transcript.
    let recorded = recorded.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].messages.len(), 1);
    assert_eq!(recorded[1].messages.len(), 3);
    assert!(!format!("{recorded:?}").contains("derived-context"));

    // Every request used the derived context.
    for context in contexts(&provider) {
        assert!(
            matches!(context.messages.last(), Some(Message::User(user)) if user.content == vec![mira_agent::mira_ai::InputContent::text("derived-context")]),
            "the provider received the derived message"
        );
    }

    // The canonical transcript never gained it.
    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 4);
    assert!(!format!("{:?}", state.messages).contains("derived-context"));
}

/// Refused calls consume the call budget exactly like executed ones.
#[tokio::test]
async fn the_tool_call_budget_covers_refused_calls() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call(
                "call-1",
                "calculator",
                json!({ "a": 1, "b": 1 }),
            )],
            StopReason::MaxTokens,
        )),
        Script::Stream(text_message("unused", StopReason::EndTurn)),
    ]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_tools(tools)
            .with_limits(AgentLimits {
                max_tool_calls: 0,
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    // A zero budget refuses the whole batch, so the run never reaches the follow-up turn.
    assert_eq!(
        expect_error(outcome),
        AgentError::Limit(RunLimit::ToolCalls)
    );
    assert_eq!(provider.request_count(), 1);
    assert!(calls.lock().unwrap().is_empty());

    // The refused batch is still paired for replay.
    let state = agent.snapshot();
    assert!(calls_are_paired_in_order(&state.messages));
    assert_eq!(
        result_counts(&state.messages),
        vec![("call-1".to_string(), 1)]
    );
    let result = state
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("the call has a result");
    assert!(result.is_error);
    assert!(result_text(result).contains("did not report a result"));
}

/// Budget accounting is cumulative across turns, including turns that only refuse their calls.
#[tokio::test]
async fn refusal_batches_consume_the_budget_cumulatively() {
    let truncated = |id: &str| {
        Script::Stream(call_message(
            vec![parsed_call(id, "calculator", json!({ "a": 1, "b": 1 }))],
            StopReason::MaxTokens,
        ))
    };
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![
        truncated("call-1"),
        truncated("call-2"),
        truncated("call-3"),
        Script::Stream(text_message("unused", StopReason::EndTurn)),
    ]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_tools(tools)
            .with_limits(AgentLimits {
                max_tool_calls: 2,
                ..AgentLimits::default()
            }),
    );

    let run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(
        expect_error(outcome),
        AgentError::Limit(RunLimit::ToolCalls)
    );
    assert_eq!(provider.request_count(), 3);
    assert!(calls.lock().unwrap().is_empty());

    let state = agent.snapshot();
    assert!(calls_are_paired_in_order(&state.messages));
    assert_eq!(
        result_counts(&state.messages),
        vec![
            ("call-1".to_string(), 1),
            ("call-2".to_string(), 1),
            ("call-3".to_string(), 1),
        ]
    );
}

/// A transform that cancels its own run must not start a provider request.
#[tokio::test]
async fn a_self_cancelling_transform_starts_no_request() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "never",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_context_transform(Arc::new(SelfCancellingTransform)),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert_eq!(provider.request_count(), 0);
    assert_eq!(agent.snapshot().messages.len(), 1);
}
