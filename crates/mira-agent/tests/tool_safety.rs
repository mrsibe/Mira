//! Tool safety: what may execute, what must be refused, and how refusals are paired.

use std::sync::Arc;

use mira_agent::mira_ai::{Credential, Message, StopReason, ToolDefinition};
use mira_agent::{AgentError, Tool, ToolRegistry};
use serde_json::json;

mod support;
use support::*;

fn calculator_tools() -> (Arc<Calculator>, ToolRegistry) {
    let calculator = Arc::new(Calculator::new());
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    (calculator, tools)
}

#[tokio::test]
async fn a_truncated_turn_never_executes_and_pairs_every_call() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![
                parsed_call("call-1", "calculator", json!({ "a": 1, "b": 2 })),
                parsed_call("call-2", "calculator", json!({ "a": 3, "b": 4 })),
            ],
            StopReason::MaxTokens,
        )),
        Script::Stream(text_message("reissued", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    // Nothing from a truncated turn executes, even though the arguments parsed and validated.
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(expect_completed(outcome).text(), "reissued");
    assert_eq!(provider.request_count(), 2);

    let results = tool_results(&events);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.is_error));
    assert_eq!(results[0].tool_call_id, "call-1");
    assert_eq!(results[1].tool_call_id, "call-2");
    assert!(result_text(&results[0]).contains("output token limit"));

    // The bounded follow-up turn sees the assistant turn and both error results in order.
    let contexts = contexts(&provider);
    let second = &contexts[1];
    assert_eq!(second.messages.len(), 4);
    assert!(matches!(second.messages[2], Message::ToolResult(_)));
    assert!(matches!(second.messages[3], Message::ToolResult(_)));
}

#[tokio::test]
async fn a_turn_that_did_not_stop_for_tools_never_executes() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call(
                "call-1",
                "calculator",
                json!({ "a": 1, "b": 2 }),
            )],
            StopReason::EndTurn,
        )),
        Script::Stream(text_message("reissued", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(expect_completed(outcome).text(), "reissued");

    let results = tool_results(&events);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert!(result_text(&results[0]).contains("did not stop for tool use"));
}

#[tokio::test]
async fn an_unregistered_tool_becomes_an_error_result() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "missing-tool", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("call", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert_eq!(results[0].tool_name, "missing-tool");
    assert!(results[0].is_error);
    assert!(result_text(&results[0]).contains("no tool with that name is registered"));
}

#[tokio::test]
async fn unparsed_arguments_never_execute() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![unparsed_call("call-1", "calculator", "{\"a\": 1")],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert!(results[0].is_error);
    assert!(result_text(&results[0]).contains("not a complete JSON object"));
}

#[tokio::test]
async fn arguments_that_do_not_match_the_schema_never_execute() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![
                // Wrong type for `a`, missing required `b`, and unknown `extra`.
                parsed_call("call-1", "calculator", json!({ "a": "one" })),
                parsed_call(
                    "call-2",
                    "calculator",
                    json!({ "a": 1, "b": 2, "extra": true }),
                ),
            ],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.is_error));
    assert!(result_text(&results[0]).contains("tool schema"));
    assert!(result_text(&results[1]).contains("tool schema"));
}

#[tokio::test]
async fn nested_arguments_validate_before_execution() {
    let tool = Arc::new(SchemaTool::new(ToolDefinition::new(
        "nested",
        "Takes a point.",
        json!({
            "type": "object",
            "properties": {
                "point": {
                    "type": "object",
                    "properties": { "x": { "type": "integer" }, "y": { "type": "integer" } },
                    "required": ["x", "y"],
                    "additionalProperties": false
                }
            },
            "required": ["point"],
            "additionalProperties": false
        }),
    )));
    let tools = registry(vec![tool as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![
                parsed_call("call-ok", "nested", json!({ "point": { "x": 1, "y": 2 } })),
                parsed_call("call-missing", "nested", json!({ "point": { "x": 1 } })),
                parsed_call(
                    "call-fraction",
                    "nested",
                    json!({ "point": { "x": 1.5, "y": 2 } }),
                ),
            ],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("point", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert_eq!(results.len(), 3);
    assert!(!results[0].is_error, "valid nested arguments execute");
    assert_eq!(result_text(&results[0]), "ok");
    assert!(results[1].is_error, "a missing required member is refused");
    assert!(results[2].is_error, "a non-integer member is refused");
}

#[tokio::test]
async fn duplicate_tool_call_ids_fail_closed_without_executing() {
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![
                parsed_call("call-1", "calculator", json!({ "a": 1, "b": 2 })),
                parsed_call("call-1", "calculator", json!({ "a": 3, "b": 4 })),
            ],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::AmbiguousToolCalls);
    assert!(calls.lock().unwrap().is_empty());
    assert!(!events
        .iter()
        .any(|event| matches!(event, mira_agent::AgentEvent::ToolStart { .. })));

    // The un-replayable turn is removed from canonical history; the prompt stays.
    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 1);
    assert!(matches!(&state.messages[0], Message::User(_)));

    // The same agent can run again, and its request replays no ambiguous call.
    let run = agent
        .prompt("try again", Credential::new("sk-test"))
        .expect("run starts");
    let (second_events, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "recovered");
    assert!(!second_events
        .iter()
        .any(|event| matches!(event, mira_agent::AgentEvent::ToolStart { .. })));
    let contexts = contexts(&provider);
    let replay = &contexts[1].messages;
    assert_eq!(replay.len(), 2);
    assert!(matches!(replay[0], Message::User(_)));
    assert!(matches!(replay[1], Message::User(_)));
    assert!(calls_are_paired_in_order(replay));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_missing_tool_call_id_fails_closed() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![parsed_call("", "calculator", json!({ "a": 1, "b": 2 }))],
        StopReason::ToolUse,
    ))]));
    let (calculator, tools) = calculator_tools();
    let calls = calculator.calls();
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;

    assert_eq!(expect_error(outcome), AgentError::AmbiguousToolCalls);
    assert!(calls.lock().unwrap().is_empty());
    // Nothing of the invalid turn remains in replayable history.
    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 1);
    assert!(calls_are_paired_in_order(&state.messages));
}

#[tokio::test]
async fn a_synchronously_panicking_tool_is_contained() {
    let tool = Arc::new(SyncPanickingTool::new());
    let tools = registry(vec![tool as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "sync-panicking", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("panic", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    // A panic in the method body is contained per tool: the run continues and the call is
    // answered by one generic paired result instead of failing the run as `Internal`.
    assert_eq!(expect_completed(outcome).text(), "recovered");
    assert_eq!(provider.request_count(), 2);
    let results = tool_results(&events);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(
        result_text(&results[0]),
        "Tool call \"sync-panicking\" failed."
    );
    assert!(!format!("{events:?}").contains(SyncPanickingTool::PANIC_TEXT));
    assert!(calls_are_paired_in_order(&agent.snapshot().messages));
}

#[tokio::test]
async fn a_panicking_tool_becomes_a_generic_error_result() {
    let tool = Arc::new(PanickingTool::new());
    let tools = registry(vec![tool as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "panicking", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("panic", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert_eq!(result_text(&results[0]), "Tool call \"panicking\" failed.");
    assert!(!format!("{events:?}").contains(PanickingTool::PANIC_TEXT));
}

#[tokio::test]
async fn a_reported_tool_failure_is_kept_as_an_error_result() {
    let tool = Arc::new(FailingTool::new());
    let tools = registry(vec![tool as Arc<dyn Tool>]);

    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "failing", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("recovered", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("fail", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;

    assert_eq!(expect_completed(outcome).text(), "recovered");
    let results = tool_results(&events);
    assert!(results[0].is_error);
    assert_eq!(result_text(&results[0]), "scripted tool failure");
}

#[tokio::test]
async fn the_registry_rejects_duplicate_names() {
    let mut tools = ToolRegistry::new();
    tools
        .insert(Arc::new(Calculator::new()) as Arc<dyn Tool>)
        .expect("first registration succeeds");
    let error = tools
        .insert(Arc::new(Calculator::new()) as Arc<dyn Tool>)
        .expect_err("duplicate name is refused");
    assert_eq!(error, AgentError::DuplicateTool("calculator".to_string()));
    assert_eq!(tools.len(), 1);
}

#[tokio::test]
async fn the_registry_validates_schemas_once_without_io() {
    let mut tools = ToolRegistry::new();

    let external = Arc::new(SchemaTool::new(ToolDefinition::new(
        "external",
        "Needs a remote schema.",
        json!({ "$ref": "https://example.invalid/schema.json" }),
    ))) as Arc<dyn Tool>;
    assert_eq!(
        tools
            .insert(external)
            .expect_err("remote reference is refused"),
        AgentError::InvalidToolSchema("external".to_string())
    );

    let missing = Arc::new(SchemaTool::new(ToolDefinition::new(
        "missing",
        "Needs an unknown local schema.",
        json!({ "type": "object", "properties": { "a": { "$ref": "#/$defs/absent" } } }),
    ))) as Arc<dyn Tool>;
    assert_eq!(
        tools
            .insert(missing)
            .expect_err("unresolvable local reference is refused"),
        AgentError::InvalidToolSchema("missing".to_string())
    );

    let not_a_schema = Arc::new(SchemaTool::new(ToolDefinition::new(
        "boolean",
        "Not a schema object.",
        json!(true),
    ))) as Arc<dyn Tool>;
    assert_eq!(
        tools
            .insert(not_a_schema)
            .expect_err("a non-object schema is refused"),
        AgentError::InvalidToolSchema("boolean".to_string())
    );

    assert!(tools.is_empty());

    let ok = Arc::new(SchemaTool::new(ToolDefinition::new(
        "local",
        "Uses local definitions.",
        json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/word" } },
            "$defs": { "word": { "type": "string" } }
        }),
    ))) as Arc<dyn Tool>;
    tools.insert(ok).expect("a local reference compiles");
    assert_eq!(tools.names().collect::<Vec<_>>(), ["local"]);
    assert_eq!(tools.definitions().len(), 1);
    assert!(tools.get("local").is_some());
    assert!(tools.get("absent").is_none());
}
