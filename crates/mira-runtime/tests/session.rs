//! Session lifecycle: selection, credential preparation, busy handling, cancellation and budgets.
//!
//! Every test is offline: scripted providers, a scripted credential resolver and scripted context
//! providers. No test uses a live provider, a database or a keyring.

mod support;

use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use mira_runtime::mira_ai::{ReasoningLevel, RequestOptions};
use mira_runtime::{
    ContextItem, ContextManager, Credential, RunOutcome, RuntimeError, RuntimeLimits, Session,
    SessionConfig, DEFAULT_RUN_TIME,
};
use serde_json::json;
use support::*;

#[tokio::test]
async fn a_single_turn_streams_events_and_returns_an_outcome() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "hello",
    ))]));
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential(
        "sk-test",
    )]));
    let registry = registry(vec![binding("main", provider.clone(), "main-key")]);
    let session = session(registry, "main", resolver.clone());

    let run = session.prompt("hi").expect("starts");
    let (deltas, outcome) = finish(run).await;

    assert_eq!(deltas, vec![Arc::from("hello")]);
    match outcome {
        Ok(RunOutcome::Completed { message }) => assert_eq!(message.text(), "hello"),
        other => panic!("expected a completed run, got {other:?}"),
    }
    assert_eq!(provider.request_count(), 1);
    assert_eq!(resolver.requests().len(), 1);
    assert_eq!(resolver.requests()[0].binding, "main");
    assert_eq!(resolver.requests()[0].credential_id.as_str(), "main-key");
    assert_eq!(provider.requests()[0].credential.expose_secret(), "sk-test");

    let snapshot = session.snapshot();
    assert!(!snapshot.busy);
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(snapshot.binding, "main");
}

#[tokio::test]
async fn model_selection_routes_to_the_selected_provider() {
    let first = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("a"))]));
    let second = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("b"))]));
    let registry = registry(vec![
        binding("first", first.clone(), "first-key"),
        binding("second", second.clone(), "second-key"),
    ]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential(
        "sk-test",
    )]));
    let session = session(registry, "second", resolver);

    let outcome = session.prompt("hi").expect("starts").outcome().await;
    assert!(matches!(outcome, Ok(RunOutcome::Completed { .. })));
    assert_eq!(second.request_count(), 1);
    assert_eq!(first.request_count(), 0);
}

#[tokio::test]
async fn an_idle_model_switch_preserves_history_and_uses_new_credentials_and_options() {
    let first = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "first answer",
    ))]));
    let second = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "second answer",
    ))]));
    let registry = registry(vec![
        binding("first", first.clone(), "first-key").with_options(RequestOptions {
            temperature: Some(0.1),
            ..Default::default()
        }),
        binding("second", second.clone(), "second-key").with_options(RequestOptions {
            temperature: Some(0.9),
            ..Default::default()
        }),
    ]);
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Credential("first-secret"),
        ResolveScript::Credential("second-secret"),
    ]));
    let session = session(registry, "first", resolver.clone());

    session
        .prompt("question")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");
    session.set_model("second").expect("idle switch");
    session
        .prompt("follow up")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let requests = second.requests();
    assert_eq!(requests.len(), 1);
    // History is preserved across the rebuild: question, answer, follow-up.
    assert_eq!(requests[0].context.messages.len(), 3);
    assert_eq!(requests[0].options.temperature, Some(0.9));
    assert_eq!(requests[0].credential.expose_secret(), "second-secret");
    assert_eq!(first.requests()[0].options.temperature, Some(0.1));
    assert_eq!(
        first.requests()[0].credential.expose_secret(),
        "first-secret"
    );
    assert_eq!(resolver.requests()[1].binding, "second");
    assert_eq!(session.binding(), "second");
    assert_eq!(session.snapshot().messages.len(), 4);
}

#[tokio::test]
async fn an_explicit_credential_bypasses_the_resolver() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(Vec::new()));
    let session = session(registry, "main", resolver.clone());

    session
        .prompt_with_credential("hi", Credential::new("explicit-secret"))
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    assert_eq!(resolver.requests().len(), 0);
    assert_eq!(
        provider.requests()[0].credential.expose_secret(),
        "explicit-secret"
    );
}

#[tokio::test]
async fn thinking_is_forwarded_to_the_provider() {
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(text_message("a")),
        Script::Stream(text_message("b")),
    ]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Credential("sk"),
        ResolveScript::Credential("sk"),
    ]));
    let session = session(registry, "main", resolver);

    session
        .set_thinking(Some(ReasoningLevel::High))
        .expect("idle");
    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");
    assert_eq!(
        provider.requests()[0].options.reasoning,
        Some(ReasoningLevel::High)
    );

    session.set_thinking(None).expect("idle");
    session
        .prompt("hi again")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");
    assert_eq!(provider.requests()[1].options.reasoning, None);
}

#[tokio::test]
async fn independent_sessions_do_not_share_credentials_selection_or_context() {
    let first_provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("a"))]));
    let second_provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("b"))]));
    let first_registry = registry(vec![binding("main", first_provider.clone(), "first-key")]);
    let second_registry = registry(vec![binding("main", second_provider.clone(), "second-key")]);
    let first_resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential(
        "first-sk",
    )]));
    let second_resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential(
        "second-sk",
    )]));
    let first_context = Arc::new(ContextManager::new(vec![Arc::new(
        FakeContextProvider::new(vec![ProvideScript::Items(vec![ContextItem::new(
            "first-source",
            "1",
            "FIRST-SNIPPET",
        )])]),
    )]));
    let second_context = Arc::new(ContextManager::new(vec![Arc::new(
        FakeContextProvider::new(vec![ProvideScript::Items(vec![ContextItem::new(
            "second-source",
            "1",
            "SECOND-SNIPPET",
        )])]),
    )]));

    let first = Session::new(
        SessionConfig::new(first_registry, "main", first_resolver)
            .with_context(first_context.clone()),
    )
    .expect("valid");
    let second = Session::new(
        SessionConfig::new(second_registry, "main", second_resolver)
            .with_context(second_context.clone()),
    )
    .expect("valid");

    let (first_outcome, second_outcome) = tokio::join!(
        first.prompt("hi").expect("starts").outcome(),
        second.prompt("hi").expect("starts").outcome()
    );
    assert!(matches!(first_outcome, Ok(RunOutcome::Completed { .. })));
    assert!(matches!(second_outcome, Ok(RunOutcome::Completed { .. })));

    let first_prompt = first_provider.requests()[0]
        .context
        .system_prompt
        .clone()
        .expect("system prompt");
    let second_prompt = second_provider.requests()[0]
        .context
        .system_prompt
        .clone()
        .expect("system prompt");
    assert!(first_prompt.contains("FIRST-SNIPPET"));
    assert!(!first_prompt.contains("SECOND-SNIPPET"));
    assert!(second_prompt.contains("SECOND-SNIPPET"));
    assert!(!second_prompt.contains("FIRST-SNIPPET"));
    assert_eq!(
        first_provider.requests()[0].credential.expose_secret(),
        "first-sk"
    );
    assert_eq!(
        second_provider.requests()[0].credential.expose_secret(),
        "second-sk"
    );
}

#[tokio::test]
async fn a_concurrent_prompt_and_every_setter_are_refused_while_busy() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Stall]));
    let session = session(registry, "main", resolver);

    let run = session.prompt("first").expect("starts");
    assert!(session.is_busy());
    assert_eq!(session.prompt("second").unwrap_err(), RuntimeError::Busy);
    assert_eq!(session.set_model("main").unwrap_err(), RuntimeError::Busy);
    assert_eq!(
        session.set_thinking(Some(ReasoningLevel::Low)).unwrap_err(),
        RuntimeError::Busy
    );
    assert_eq!(
        session.set_history(Vec::new()).unwrap_err(),
        RuntimeError::Busy
    );
    assert_eq!(
        session.set_context_manager(None).unwrap_err(),
        RuntimeError::Busy
    );

    run.cancel();
    wait_idle(&session).await;
    assert!(!session.is_busy());
    session.set_thinking(None).expect("idle again");
}

#[tokio::test]
async fn dropping_a_polled_outcome_future_cancels_the_run() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = session(registry, "main", resolver);

    let run = session.prompt("hi").expect("starts");
    let mut outcome = Box::pin(run.outcome());
    tokio::select! {
        _ = &mut outcome => panic!("a stalled run must not resolve"),
        () = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    drop(outcome);

    wait_idle(&session).await;
    assert!(!session.is_busy());
}

#[tokio::test]
async fn backpressure_is_bounded_by_the_total_run_deadline() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Flood(1000)]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(SessionConfig::new(registry, "main", resolver).with_runtime(
        RuntimeLimits {
            total_time: Duration::from_millis(50),
            event_buffer: NonZeroUsize::new(1).expect("non-zero"),
        },
    ))
    .expect("valid");

    let run = session.prompt("hi").expect("starts");
    // Never read an event: the bounded channel fills and the run must still end at its deadline.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(run.outcome().await.unwrap_err(), RuntimeError::Timeout);
    wait_idle(&session).await;
}

#[test]
fn out_of_bounds_run_budgets_are_safe_errors() {
    let provider = Arc::new(FakeProvider::new(Vec::new()));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::default());

    let too_long = Session::new(
        SessionConfig::new(registry.clone(), "main", resolver.clone()).with_runtime(
            RuntimeLimits {
                total_time: Duration::MAX,
                event_buffer: NonZeroUsize::new(4).expect("non-zero"),
            },
        ),
    );
    assert!(matches!(
        too_long.unwrap_err(),
        RuntimeError::InvalidRunTime { .. }
    ));

    let too_large = Session::new(SessionConfig::new(registry, "main", resolver).with_runtime(
        RuntimeLimits {
            total_time: DEFAULT_RUN_TIME,
            event_buffer: NonZeroUsize::MAX,
        },
    ));
    assert!(matches!(
        too_large.unwrap_err(),
        RuntimeError::InvalidEventBuffer { .. }
    ));
}

#[test]
fn an_unregistered_binding_is_refused_at_construction_and_on_switch() {
    let provider = Arc::new(FakeProvider::new(Vec::new()));
    let bindings = registry(vec![binding("main", provider, "key")]);
    let session = session(bindings.clone(), "main", Arc::new(FakeResolver::default()));

    let missing = Session::new(SessionConfig::new(
        bindings,
        "other",
        Arc::new(FakeResolver::default()),
    ));
    assert_eq!(
        missing.unwrap_err(),
        RuntimeError::UnknownBinding("other".to_string())
    );
    assert_eq!(
        session.set_model("other").unwrap_err(),
        RuntimeError::UnknownBinding("other".to_string())
    );
}

#[tokio::test]
async fn a_panicking_resolver_does_not_leak_busy() {
    for panic in [ResolveScript::Panic, ResolveScript::PolledPanic] {
        let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
        let registry = registry(vec![binding("main", provider.clone(), "key")]);
        let resolver = Arc::new(FakeResolver::new(vec![
            panic,
            ResolveScript::Credential("sk"),
        ]));
        let session = session(registry, "main", resolver);

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            session.prompt("first").expect("starts").outcome(),
        )
        .await
        .expect("a resolver panic must terminate the run");
        assert_eq!(outcome.unwrap_err(), RuntimeError::Internal);
        assert_eq!(provider.request_count(), 0);
        wait_idle(&session).await;

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            session.prompt("second").expect("starts").outcome(),
        )
        .await
        .expect("the session must be reusable after a resolver panic");
        assert!(matches!(outcome, Ok(RunOutcome::Completed { .. })));
        assert_eq!(provider.request_count(), 1);
    }
}

#[tokio::test]
async fn resolver_failure_and_cancellation_happen_before_the_agent_starts() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Failure,
        ResolveScript::Stall,
    ]));
    let session = session(registry, "main", resolver.clone());

    let outcome = session.prompt("first").expect("starts").outcome().await;
    assert_eq!(outcome.unwrap_err(), RuntimeError::Credential);
    assert_eq!(provider.request_count(), 0);

    let run = session.prompt("second").expect("starts");
    // Wait until the stalled resolution has been polled, so the test cancels credential
    // preparation in progress rather than before it starts.
    let polled = resolver.polled_count();
    for _ in 0..500 {
        if polled.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    run.cancel();
    assert_eq!(run.outcome().await.unwrap_err(), RuntimeError::Cancelled);
    assert_eq!(provider.request_count(), 0);
    // The resolver's own watcher observes the run's cancellation token; allow it to be scheduled.
    let seen = resolver.cancellation_count();
    for _ in 0..500 {
        if seen.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "the resolver never observed cancellation"
    );
    wait_idle(&session).await;
}

#[tokio::test]
async fn a_prompt_immediately_after_cancellation_is_busy_or_safe() {
    // The session must never report idle before the child agent has repaired its transcript.
    // Cancelled starts may consume zero or one scripts depending on scheduling.
    // Every possible surviving request must have a completed scripted response.
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(text_message("complete")),
        Script::Stream(text_message("complete")),
        Script::Stream(text_message("complete")),
    ]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Credential("sk"),
        ResolveScript::Credential("sk"),
        ResolveScript::Credential("sk"),
    ]));
    let session = session(registry, "main", resolver);

    let run = session.prompt("first").expect("starts");
    run.cancel();
    drop(run);

    // Either cleanup already settled and the prompt is accepted, or the session is still busy.
    match session.prompt("second") {
        Ok(run) => {
            run.cancel();
            drop(run);
        }
        Err(error) => assert_eq!(error, RuntimeError::Busy),
    }

    wait_idle(&session).await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        session.prompt("third").expect("starts").outcome(),
    )
    .await
    .expect("a resumed session must complete rather than hang");
    assert!(matches!(outcome, Ok(RunOutcome::Completed { .. })));
    assert!(calls_are_paired_in_order(&session.snapshot().messages));
}

#[tokio::test]
async fn a_cancelled_tool_run_leaves_paired_history_for_the_next_prompt() {
    let (tool, tool_cancelled) = StallingTool::new();
    let invocations = tool.invocations();
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            "working",
            vec![parsed_call("call-1", "stalling", json!({}))],
        )),
        Script::Stream(text_message("done")),
    ]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Credential("sk"),
        ResolveScript::Credential("sk"),
    ]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_tools(vec![Arc::new(tool) as Arc<dyn mira_runtime::Tool>]),
    )
    .expect("valid");

    let run = session.prompt("use the tool").expect("starts");
    while invocations.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    run.cancel();
    tool_cancelled.await.expect("tool observed cancellation");
    wait_idle(&session).await;

    session
        .prompt("are you done")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let contexts = provider.contexts();
    assert!(
        calls_are_paired_in_order(&contexts[1].messages),
        "unpaired history: {:?}",
        contexts[1].messages
    );
}

#[tokio::test]
async fn a_credential_never_appears_in_a_debug_rendering_an_event_or_an_error() {
    const SECRET: &str = "sk-never-print-this-value";
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "harmless answer",
    ))]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential(SECRET)]));
    let session = session(registry, "main", resolver);

    let run = session.prompt("hi").expect("starts");
    assert!(!format!("{session:?}").contains(SECRET));
    assert!(!format!("{run:?}").contains(SECRET));
    assert!(!format!("{:?}", RuntimeError::Credential).contains(SECRET));

    let (_, outcome) = finish(run).await;
    assert!(matches!(outcome, Ok(RunOutcome::Completed { .. })));
    assert!(!format!("{session:?}").contains(SECRET));
    assert!(!format!("{:?}", session.snapshot()).contains(SECRET));
    assert!(!format!("{:?}", provider.requests()).contains(SECRET));
}
