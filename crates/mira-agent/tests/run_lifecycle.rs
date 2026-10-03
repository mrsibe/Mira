//! Run lifecycle: streaming, message commit order, tool turns and outcome kinds.

use std::sync::Arc;

use mira_agent::mira_ai::{AssistantContent, Credential, InputContent, Message, StopReason};
use mira_agent::{AgentEvent, RunOutcome, Tool, ToolRegistry};
use serde_json::json;

mod support;
use support::*;

#[tokio::test]
async fn single_turn_text_run_streams_deltas_and_completes() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "hello",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    let message = expect_completed(outcome);
    assert_eq!(message.text(), "hello");
    assert_eq!(message.stop_reason, StopReason::EndTurn);

    assert_eq!(
        kinds(&events),
        [
            "run-start",
            "turn-start",
            "user-start",
            "user-end",
            "assistant-start",
            "update",
            "update",
            "update",
            "assistant-end",
            "turn-end",
        ]
    );

    // The prompt and the authoritative assistant turn are committed in order.
    let state = agent.snapshot();
    assert!(!state.busy);
    assert!(state.partial.is_none());
    assert_eq!(state.messages.len(), 2);
    assert!(
        matches!(&state.messages[0], Message::User(user) if user.content == vec![InputContent::text("hi")])
    );
    assert!(
        matches!(&state.messages[1], Message::Assistant(assistant) if assistant.text() == "hello")
    );

    // The provider saw the system prompt, the transcript and no tools.
    let contexts = contexts(&provider);
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].system_prompt.as_deref(), Some("be brief"));
    assert_eq!(contexts[0].messages.len(), 1);
    assert!(contexts[0].tools.is_empty());
}

#[tokio::test]
async fn a_terminal_without_deltas_still_commits_a_message() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Silent(text_message(
        "quiet",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "quiet");
    assert_eq!(
        kinds(&events),
        [
            "run-start",
            "turn-start",
            "user-start",
            "user-end",
            "assistant-start",
            "assistant-end",
            "turn-end",
        ]
    );
}

#[tokio::test]
async fn tool_turn_executes_in_order_and_replays_the_full_context() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![
                parsed_call("call-1", "calculator", json!({ "a": 1, "b": 2 })),
                parsed_call(
                    "call-2",
                    "calculator",
                    json!({ "a": 5, "b": 3, "op": "sub" }),
                ),
            ],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("done", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("compute", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "done");

    // Both calls executed, in the order the model requested them.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            json!({ "a": 1, "b": 2 }),
            json!({ "a": 5, "b": 3, "op": "sub" }),
        ]
    );

    let started: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStart { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["call-1", "call-2"]);
    assert_eq!(
        kinds(&events),
        [
            "run-start",
            "turn-start",
            "user-start",
            "user-end",
            "assistant-start",
            "update",
            "update",
            "update",
            "update",
            "update",
            "update",
            "assistant-end",
            "tool-start",
            "tool-end",
            "tool-result-start",
            "tool-result-end",
            "tool-start",
            "tool-end",
            "tool-result-start",
            "tool-result-end",
            "turn-end",
            "turn-start",
            "assistant-start",
            "update",
            "update",
            "update",
            "assistant-end",
            "turn-end",
        ]
    );

    let turn_results = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::TurnEnd { tool_results, .. } => Some(tool_results.clone()),
            _ => None,
        })
        .expect("first turn ends with results");
    assert_eq!(
        turn_results
            .iter()
            .map(|result| result.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
    assert!(turn_results.iter().all(|result| !result.is_error));

    // The second request replays the assistant turn and the ordered results.
    let contexts = contexts(&provider);
    assert_eq!(contexts.len(), 2);
    let second = &contexts[1];
    assert_eq!(second.messages.len(), 4);
    assert!(matches!(&second.messages[0], Message::User(_)));
    assert!(
        matches!(&second.messages[1], Message::Assistant(assistant) if assistant.tool_calls().count() == 2)
    );
    match (&second.messages[2], &second.messages[3]) {
        (Message::ToolResult(first), Message::ToolResult(second)) => {
            assert_eq!(first.tool_call_id, "call-1");
            assert_eq!(first.content, vec![InputContent::text("3")]);
            assert_eq!(second.tool_call_id, "call-2");
            assert_eq!(second.content, vec![InputContent::text("2")]);
        }
        other => panic!("expected ordered tool results, got {other:?}"),
    }
    assert_eq!(second.tools.len(), 1);
    assert_eq!(second.tools[0].name, "calculator");

    // The committed transcript holds the prompt, the assistant turn, its two results and the
    // final assistant turn.
    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 5);
    assert!(state.pending_tool_calls.is_empty());
}

#[tokio::test]
async fn max_tokens_without_tool_calls_is_a_truncated_outcome() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "half a sen",
        StopReason::MaxTokens,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("write", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_truncated(outcome).text(), "half a sen");
}

#[tokio::test]
async fn tool_use_without_tool_calls_ends_the_run() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "nothing to call",
        StopReason::ToolUse,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "nothing to call");
    assert_eq!(provider.request_count(), 1);
    assert_eq!(
        kinds(&events),
        [
            "run-start",
            "turn-start",
            "user-start",
            "user-end",
            "assistant-start",
            "update",
            "update",
            "update",
            "assistant-end",
            "turn-end",
        ]
    );
}

#[tokio::test]
async fn a_text_turn_with_calls_keeps_content_and_streams_both_blocks() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(mixed_message(
            "adding",
            vec![parsed_call(
                "call-1",
                "calculator",
                json!({ "a": 2, "b": 2 }),
            )],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("4", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "4");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::MessageEnd {
            message: Message::Assistant(assistant),
            ..
        } if assistant.content.iter().any(|block| matches!(block, AssistantContent::Text(text) if text.text == "adding"))
    )));
}

#[tokio::test]
async fn run_outcome_exposes_the_final_message() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "answer",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("q", Credential::new("sk-test"))
        .expect("run starts");
    let outcome = run.outcome().await.expect("completed");
    assert_eq!(outcome.message().text(), "answer");
    assert!(matches!(outcome, RunOutcome::Completed { .. }));
}
