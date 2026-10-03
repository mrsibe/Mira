//! Operational state, snapshot contents, busy atomicity and credential containment.

use std::num::NonZeroUsize;
use std::sync::Arc;

use mira_agent::mira_ai::{Credential, Message, RequestOptions, StopReason, UserMessage};
use mira_agent::{
    AgentConfig, AgentError, AgentEvent, AgentLimits, Tool, ToolRegistry, MAX_EVENT_BUFFER,
};

mod support;
use support::*;

#[tokio::test]
async fn a_second_start_is_refused_while_a_run_is_active() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("first", Credential::new("sk-test"))
        .expect("run starts");
    assert!(agent.is_busy());
    assert_eq!(
        agent.prompt("second", Credential::new("sk-test")).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent
            .start(UserMessage::text("third"), Credential::new("sk-test"))
            .err(),
        Some(AgentError::Busy)
    );

    run.cancel();
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert!(!agent.is_busy());

    // The agent accepts a new run as soon as the previous outcome was published.
    let run = agent
        .prompt("again", Credential::new("sk-test"))
        .expect("run starts");
    run.cancel();
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
}

#[tokio::test]
async fn cloned_handles_share_one_agent() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());
    let clone = agent.clone();

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    assert!(clone.is_busy());
    assert_eq!(
        clone.prompt("other", Credential::new("sk-test")).err(),
        Some(AgentError::Busy)
    );

    run.cancel();
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    assert!(!clone.is_busy());
    assert!(clone.snapshot().messages.is_empty());
}

#[tokio::test]
async fn configuration_is_refused_while_a_run_is_active() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    assert_eq!(agent.set_model(test_model()).err(), Some(AgentError::Busy));
    assert_eq!(
        agent.set_system_prompt(Some("changed".to_string())).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent.set_tools(ToolRegistry::new()).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent.set_context_transform(None).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent.set_options(RequestOptions::default()).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent.set_limits(AgentLimits::default()).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent.set_event_buffer(NonZeroUsize::new(8).unwrap()).err(),
        Some(AgentError::Busy)
    );
    assert_eq!(
        agent
            .append_message(Message::User(UserMessage::text("late")))
            .err(),
        Some(AgentError::Busy)
    );
    assert_eq!(agent.set_history(Vec::new()).err(), Some(AgentError::Busy));
    assert_eq!(agent.clear_history().err(), Some(AgentError::Busy));

    run.cancel();
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);

    agent
        .set_system_prompt(Some("after".to_string()))
        .expect("configuration is allowed once idle");
    assert!(agent.snapshot().messages.is_empty());
}

#[tokio::test]
async fn a_history_set_while_idle_drives_the_next_request() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "ok",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());
    agent
        .append_message(Message::User(UserMessage::text("earlier")))
        .expect("the agent is idle");

    let run = agent
        .prompt("now", Credential::new("sk-test"))
        .expect("run starts");
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "ok");

    let contexts = contexts(&provider);
    assert_eq!(contexts[0].messages.len(), 2);
    assert_eq!(agent.snapshot().messages.len(), 3);
}

#[tokio::test]
async fn the_snapshot_shows_the_partial_turn_while_it_streams() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Partial(text_message(
        "streaming text",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let mut run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    loop {
        let event = run.recv().await.expect("an event arrives");
        if matches!(event, AgentEvent::MessageUpdate { .. }) {
            break;
        }
    }

    let state = agent.snapshot();
    assert!(state.busy);
    let partial = state.partial.as_ref().expect("a partial turn is visible");
    assert_eq!(partial.text(), "streaming text");
    assert_eq!(state.messages.len(), 1, "the partial turn is not committed");

    run.cancel();
    let (_, outcome) = finish(run).await;
    assert_eq!(expect_error(outcome), AgentError::Cancelled);
    let state = agent.snapshot();
    assert!(!state.busy);
    assert!(state.partial.is_none());
    assert_eq!(state.messages.len(), 1);
}

#[tokio::test]
async fn a_credential_stays_inside_its_run() {
    const SECRET: &str = "sk-super-secret-credential-value";

    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "ok",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), ToolRegistry::new());

    let run = agent
        .prompt("hi", Credential::new(SECRET))
        .expect("run starts");
    assert!(!format!("{agent:?}").contains(SECRET));
    assert!(!format!("{run:?}").contains(SECRET));

    let (events, outcome) = finish(run).await;
    assert!(!format!("{events:?}").contains(SECRET));
    let outcome = expect_completed(outcome);
    assert!(!format!("{outcome:?}").contains(SECRET));
    assert!(!format!("{:?}", agent.snapshot()).contains(SECRET));

    // The provider received the credential for this run only, and even the request debug output
    // is redacted.
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].credential.expose_secret(), SECRET);
    assert!(!format!("{:?}", requests[0]).contains(SECRET));
}

#[tokio::test]
async fn separate_agents_are_independent() {
    let first_provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "one",
        StopReason::EndTurn,
    ))]));
    let second_provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "two",
        StopReason::EndTurn,
    ))]));
    let first = agent(first_provider, ToolRegistry::new());
    let second = agent(second_provider, ToolRegistry::new());

    let first_run = first
        .prompt("first", Credential::new("sk-test"))
        .expect("run starts");
    let second_run = second
        .prompt("second", Credential::new("sk-test"))
        .expect("run starts");

    assert_eq!(expect_completed(finish(first_run).await.1).text(), "one");
    assert_eq!(expect_completed(finish(second_run).await.1).text(), "two");

    assert_eq!(first.snapshot().messages.len(), 2);
    assert_eq!(second.snapshot().messages.len(), 2);
    assert!(!first.is_busy());
    assert!(!second.is_busy());
}

#[tokio::test]
async fn tools_are_snapshotted_per_run() {
    let calculator = Arc::new(Calculator::new());
    let tools = registry(vec![Arc::clone(&calculator) as Arc<dyn Tool>]);
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "ok",
        StopReason::EndTurn,
    ))]));
    let agent = agent(Arc::clone(&provider), tools);

    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    // Replacing the executable tools is refused while the run is active, so the run keeps the
    // registry it started with.
    assert_eq!(
        agent.set_tools(ToolRegistry::new()).err(),
        Some(AgentError::Busy)
    );

    let (_, outcome) = finish(run).await;
    assert_eq!(expect_completed(outcome).text(), "ok");
    let contexts = contexts(&provider);
    assert_eq!(contexts[0].tools.len(), 1);
    assert_eq!(contexts[0].tools[0].name, "calculator");
}

/// An event buffer larger than the runtime supports is refused without taking the run slot.
#[tokio::test]
async fn an_oversized_event_buffer_is_rejected_without_taking_the_run_slot() {
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(text_message("ok", StopReason::EndTurn)),
        Script::Stream(text_message("ok", StopReason::EndTurn)),
        Script::Stream(text_message("ok", StopReason::EndTurn)),
    ]));
    let oversized = NonZeroUsize::new(usize::MAX).expect("non-zero");
    let expected = AgentError::InvalidEventBuffer {
        requested: usize::MAX,
        maximum: MAX_EVENT_BUFFER,
    };
    let agent = mira_agent::Agent::new(
        AgentConfig::new(provider.clone(), test_model()).with_event_buffer(oversized),
    );

    assert_eq!(
        agent
            .prompt("hi", Credential::new("sk-test"))
            .expect_err("the buffer is rejected"),
        expected
    );
    assert!(!agent.is_busy(), "a refused start leaves the run slot free");
    assert_eq!(provider.request_count(), 0);

    // The agent stays usable once a supported buffer is configured.
    agent
        .set_event_buffer(NonZeroUsize::new(4).expect("non-zero"))
        .expect("a supported buffer is accepted");
    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    assert_eq!(expect_completed(finish(run).await.1).text(), "ok");

    // The setter rejects an oversized buffer and the configured one keeps working.
    assert_eq!(agent.set_event_buffer(oversized), Err(expected));
    let run = agent
        .prompt("hi", Credential::new("sk-test"))
        .expect("run starts");
    assert_eq!(expect_completed(finish(run).await.1).text(), "ok");
}
