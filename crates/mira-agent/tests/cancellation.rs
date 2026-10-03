//! Cancellation, consumer drop and backpressure: a run always ends, always cleans up, and always
//! leaves a replayable transcript.

use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use mira_agent::mira_ai::{Credential, StopReason};
use mira_agent::{AgentConfig, AgentError, AgentEvent, Message, PendingToolCall, Tool};
use serde_json::json;

mod support;
use support::*;

/// Read events until `stop` matches, returning the events read so far.
async fn read_until(
    run: &mut mira_agent::AgentRun,
    stop: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let event = run.recv().await.expect("an event arrives");
        let matched = stop(&event);
        events.push(event);
        if matched {
            return events;
        }
    }
}

/// Read events until the run's event stream ends.
async fn drain(run: &mut mira_agent::AgentRun) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = run.recv().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn a_run_cancelled_before_its_first_request_never_calls_the_provider() {
    // This test relies on the current-thread test runtime: the run task is not polled before it
    // is cancelled.
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "never",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), mira_agent::ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    run.cancel();

    let (events, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert!(events.is_empty());
    assert_eq!(provider.request_count(), 0);
    assert!(!agent.is_busy());
    assert!(agent.snapshot().messages.is_empty());
}

#[tokio::test]
async fn cancellation_stops_a_stalled_provider_stream() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), mira_agent::ToolRegistry::new());

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let mut events = read_until(&mut run, |event| {
        matches!(
            event,
            AgentEvent::MessageStart {
                message: Message::Assistant(_),
                ..
            }
        )
    })
    .await;
    let stalled = provider
        .take_stall_signal()
        .expect("the provider is streaming");

    run.cancel();
    events.extend(drain(&mut run).await);
    let outcome = run.outcome().await;

    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    stalled.await.expect("the provider stream was dropped");
    // Nothing ran after cancellation was established: the last event is the partial turn.
    assert_eq!(
        kinds(&events),
        [
            "run-start",
            "turn-start",
            "user-start",
            "user-end",
            "assistant-start",
        ]
    );
    assert_eq!(agent.snapshot().messages.len(), 1);
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn cancellation_stops_a_stalled_context_transform() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "never",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model())
            .with_context_transform(Arc::new(StallingTransform)),
    );

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let mut events = read_until(&mut run, |event| {
        matches!(
            event,
            AgentEvent::MessageEnd {
                message: Message::User(_),
                ..
            }
        )
    })
    .await;

    run.cancel();
    events.extend(drain(&mut run).await);
    let outcome = run.outcome().await;

    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert_eq!(provider.request_count(), 0);
    assert_eq!(
        kinds(&events),
        ["run-start", "turn-start", "user-start", "user-end"]
    );
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn cancellation_stops_a_running_tool_and_clears_its_pending_state() {
    let (tool, observed) = StallingTool::new();
    let tools = registry(vec![Arc::new(tool) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![parsed_call("call-1", "stalling", json!({}))],
        StopReason::ToolUse,
    ))]));
    let agent = agent(Arc::clone(&provider), tools);

    let mut run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    let mut events = read_until(&mut run, |event| {
        matches!(event, AgentEvent::ToolStart { .. })
    })
    .await;

    // The call is executing and is visible in the snapshot.
    let state = agent.snapshot();
    assert!(state.busy);
    assert_eq!(
        state.pending_tool_calls,
        vec![PendingToolCall {
            call_id: "call-1".to_string(),
            name: "stalling".to_string(),
        }]
    );
    assert_eq!(state.messages.len(), 2);

    run.cancel();
    events.extend(drain(&mut run).await);
    let outcome = run.outcome().await;

    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    observed.await.expect("the tool observed cancellation");
    let state = agent.snapshot();
    assert!(!state.busy);
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.partial.is_none());
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnEnd { .. })));

    // The interrupted call is paired in canonical history before the agent goes idle.
    assert_eq!(state.messages.len(), 3);
    assert!(calls_are_paired_in_order(&state.messages));
    let results: Vec<_> = state
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-1");
    assert!(results[0].is_error);
    assert!(result_text(results[0]).contains("did not report a result"));
}

#[tokio::test]
async fn a_saturated_event_buffer_still_lets_a_run_be_cancelled() {
    // A one-slot buffer with a consumer that reads nothing cannot drain, so the run can never
    // finish publishing and can only end by cancellation.
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "streaming",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model()).with_event_buffer(NonZeroUsize::new(1).unwrap()),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    // Let the run task start and fill its single buffer slot.
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    run.cancel();

    let (events, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert_eq!(
        kinds(&events),
        ["run-start"],
        "the run is blocked behind its saturated buffer"
    );
    assert!(!agent.is_busy());
    assert!(
        agent.snapshot().messages.is_empty(),
        "the run stopped before it could commit the prompt"
    );
}

#[tokio::test]
async fn an_unread_event_buffer_does_not_block_the_terminal_outcome() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "finished",
        StopReason::EndTurn,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model()).with_event_buffer(NonZeroUsize::new(1).unwrap()),
    );

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    // No event is ever read: the outcome alone must arrive.
    let outcome = run.outcome().await;
    assert_eq!(expect_completed(outcome).text(), "finished");
    assert!(!agent.is_busy());
}

#[tokio::test]
async fn dropping_a_run_cancels_the_provider_and_clears_busy() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), mira_agent::ToolRegistry::new());

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    read_until(&mut run, |event| {
        matches!(
            event,
            AgentEvent::MessageStart {
                message: Message::Assistant(_),
                ..
            }
        )
    })
    .await;
    let stalled = provider
        .take_stall_signal()
        .expect("the provider is streaming");

    drop(run);
    stalled.await.expect("the provider stream was dropped");

    // The run task clears run state on its way out. Wait for that task rather than assuming it
    // finished synchronously with the drop.
    for _ in 0..1000 {
        if !agent.is_busy() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let state = agent.snapshot();
    assert!(!state.busy);
    assert!(state.partial.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(state.messages.len(), 1, "only the prompt was committed");
}

#[tokio::test]
async fn a_run_reports_cancellation_to_its_own_cancellation_token() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), mira_agent::ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    let token = run.cancellation();
    assert!(!token.is_cancelled());
    run.cancel();
    assert!(token.is_cancelled());
    assert!(run.is_cancelled());

    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
}

/// A cancelled tool call must not be replayed unanswered by the next prompt of the same agent.
#[tokio::test]
async fn a_cancelled_tool_call_is_paired_before_the_next_prompt() {
    let (tool, observed) = StallingTool::new();
    let invocations = tool.invocations();
    let tools = registry(vec![Arc::new(tool) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "stalling", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("after repair", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let mut run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    read_until(&mut run, |event| {
        matches!(event, AgentEvent::ToolStart { .. })
    })
    .await;
    run.cancel();
    let outcome = run.outcome().await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    observed.await.expect("the tool observed cancellation");
    assert_eq!(invocations.load(Ordering::SeqCst), 1);

    let state = agent.snapshot();
    assert_eq!(state.messages.len(), 3);
    assert!(calls_are_paired_in_order(&state.messages));
    assert_eq!(
        result_counts(&state.messages),
        vec![("call-1".to_string(), 1)]
    );

    // The same agent runs again: the replayed call keeps exactly one ordered result, and the
    // interrupted tool is not invoked a second time.
    let run = agent
        .prompt("again", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "after repair");
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStart { .. })));
    assert_eq!(invocations.load(Ordering::SeqCst), 1);

    let replay = &contexts(&provider)[1].messages;
    assert_eq!(replay.len(), 4);
    assert!(calls_are_paired_in_order(replay));
    assert_eq!(result_counts(replay), vec![("call-1".to_string(), 1)]);
}

/// Dropping a run must leave the same replay-safe transcript as cancelling it.
#[tokio::test]
async fn a_dropped_run_is_paired_before_the_next_prompt() {
    let (tool, observed) = StallingTool::new();
    let invocations = tool.invocations();
    let tools = registry(vec![Arc::new(tool) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            vec![parsed_call("call-1", "stalling", json!({}))],
            StopReason::ToolUse,
        )),
        Script::Stream(text_message("after repair", StopReason::EndTurn)),
    ]));
    let agent = agent(Arc::clone(&provider), tools);

    let mut run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    read_until(&mut run, |event| {
        matches!(event, AgentEvent::ToolStart { .. })
    })
    .await;
    drop(run);
    observed.await.expect("the tool observed cancellation");
    for _ in 0..1000 {
        if !agent.is_busy() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let state = agent.snapshot();
    assert!(!state.busy);
    assert!(calls_are_paired_in_order(&state.messages));
    assert_eq!(
        result_counts(&state.messages),
        vec![("call-1".to_string(), 1)]
    );

    let run = agent
        .prompt("again", Credential::new("sk-test"))
        .expect("run starts");
    let (events, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "after repair");
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStart { .. })));
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(
        result_counts(&contexts(&provider)[1].messages),
        vec![("call-1".to_string(), 1)]
    );
}

/// Dropping a half-polled `outcome()` future must cancel the run, not leave it stalled and busy.
#[tokio::test]
async fn dropping_a_polled_outcome_future_cancels_the_run() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), mira_agent::ToolRegistry::new());

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    read_until(&mut run, |event| {
        matches!(
            event,
            AgentEvent::MessageStart {
                message: Message::Assistant(_),
                ..
            }
        )
    })
    .await;
    let stalled = provider
        .take_stall_signal()
        .expect("the provider is streaming");
    let token = run.cancellation();
    assert!(!token.is_cancelled());

    // Poll the outcome future once — the inner future is polled before the zero-duration
    // deadline — and then drop it unresolved.
    let mut polled = Box::pin(tokio::time::timeout(Duration::ZERO, run.outcome()));
    let elapsed = (&mut polled).await;
    drop(polled);
    assert!(elapsed.is_err(), "the outcome was still pending");

    assert!(
        token.is_cancelled(),
        "dropping the outcome future cancels the run"
    );
    stalled.await.expect("the provider stream was dropped");
    for _ in 0..1000 {
        if !agent.is_busy() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(!agent.is_busy());
}

/// A tool result that already happened survives an interrupted publication.
#[tokio::test]
async fn an_executed_tool_result_survives_an_interrupted_publication() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![parsed_call(
            "call-1",
            "calculator",
            json!({ "a": 1, "b": 2 }),
        )],
        StopReason::ToolUse,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model())
            .with_tools(tools)
            .with_event_buffer(NonZeroUsize::new(1).unwrap()),
    );

    let mut run = agent
        .prompt("add", Credential::new("sk-test"))
        .expect("run starts");
    read_until(&mut run, |event| {
        matches!(event, AgentEvent::ToolStart { .. })
    })
    .await;
    // Stop reading. The one-slot buffer now blocks the run after the tool result was committed,
    // which is exactly where an interrupted publication used to lose it.
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(calls.lock().unwrap().len(), 1, "the tool executed");
    run.cancel();

    let outcome = run.outcome().await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);

    let state = agent.snapshot();
    assert!(calls_are_paired_in_order(&state.messages));
    let results: Vec<_> = state
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "exactly one result for the call");
    assert!(!results[0].is_error, "the actual result is preserved");
    assert_eq!(result_text(results[0]), "3");
}

/// Once cancellation is established, a pending tool call is never invoked.
#[tokio::test]
async fn a_cancelled_run_never_invokes_a_pending_tool() {
    let calculator = Arc::new(Calculator::new());
    let calls = calculator.calls();
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(call_message(
        vec![parsed_call(
            "call-1",
            "calculator",
            json!({ "a": 1, "b": 1 }),
        )],
        StopReason::ToolUse,
    ))]));
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider, test_model())
            .with_tools(tools)
            .with_event_buffer(NonZeroUsize::new(1).unwrap()),
    );

    let run = agent
        .prompt("go", Credential::new("sk-test"))
        .expect("run starts");
    // The one-slot buffer blocks the run while it publishes; cancel there.
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    run.cancel();

    let (events, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert!(
        calls.lock().unwrap().is_empty(),
        "a cancelled run invokes no tool"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStart { .. })));
    assert!(!agent.is_busy());
    assert!(calls_are_paired_in_order(&agent.snapshot().messages));
}
