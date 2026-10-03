//! Context composition: budgets, ordering, transient injection and failures.
//!
//! Every test is offline: a scripted provider, a scripted resolver and scripted context providers.

mod support;

use std::sync::Arc;
use std::time::Duration;

use mira_runtime::mira_ai::Usage;
use mira_runtime::{
    ContextItem, ContextManager, RuntimeError, Session, SessionConfig, Tool,
    DEFAULT_MAX_INJECTED_CHARS,
};
use serde_json::json;
use support::*;

#[tokio::test]
async fn context_panics_terminate_and_leave_the_session_reusable() {
    for panic in [ProvideScript::Panic, ProvideScript::PolledPanic] {
        let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
        let context_provider = Arc::new(FakeContextProvider::new(vec![panic]));
        let manager = Arc::new(ContextManager::new(vec![context_provider]));
        let registry = registry(vec![binding("main", provider.clone(), "key")]);
        let resolver = Arc::new(FakeResolver::new(vec![
            ResolveScript::Credential("sk"),
            ResolveScript::Credential("sk"),
        ]));
        let session =
            Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
                .expect("valid session");

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            session.prompt("first").expect("starts").outcome(),
        )
        .await
        .expect("a context panic must terminate the run");
        assert_eq!(
            outcome.unwrap_err(),
            RuntimeError::Agent(mira_runtime::mira_agent::AgentError::Internal)
        );
        assert_eq!(provider.request_count(), 0);
        wait_idle(&session).await;

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            session.prompt("second").expect("starts").outcome(),
        )
        .await
        .expect("the session must be reusable after a context panic");
        assert!(matches!(
            outcome,
            Ok(mira_runtime::RunOutcome::Completed { .. })
        ));
        assert_eq!(provider.request_count(), 1);
        assert!(calls_are_paired_in_order(&session.snapshot().messages));
    }
}

#[tokio::test]
async fn context_is_injected_once_per_request_without_polluting_the_transcript() {
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            "working",
            vec![parsed_call("call-1", "calculator", json!({"a": 1, "b": 2}))],
        )),
        Script::Stream(text_message("done")),
    ]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![
        ProvideScript::Items(vec![ContextItem::new("memory", "1", MARKER)]),
        ProvideScript::Items(vec![ContextItem::new("memory", "1", MARKER)]),
    ]));
    let manager = Arc::new(ContextManager::new(vec![context_provider.clone()]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_context(manager.clone())
            .with_system_prompt("be brief")
            .with_tools(vec![Arc::new(Calculator::new()) as Arc<dyn Tool>]),
    )
    .expect("valid");

    session
        .prompt("add two numbers")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let contexts = provider.contexts();
    assert_eq!(contexts.len(), 2, "one request per turn");
    for context in &contexts {
        let prompt = context.system_prompt.clone().expect("system prompt");
        assert!(prompt.starts_with("be brief"), "base prompt kept: {prompt}");
        assert_eq!(
            prompt.matches(MARKER).count(),
            1,
            "snippet injected exactly once: {prompt}"
        );
    }
    assert!(
        !format!("{:?}", session.snapshot().messages).contains(MARKER),
        "the canonical transcript must not receive an injected snippet"
    );
    assert!(context_provider
        .requests()
        .iter()
        .all(|request| !request.transcript_contains_marker));
    let item = ContextItem::new("memory", "1", MARKER);
    let reported = session.context_usage();
    assert_eq!(reported.injected_items, 1);
    // The label and separators are part of what was injected, so they are part of the count.
    assert_eq!(reported.injected_chars, 2 + rendered_chars(&item));
    assert_eq!(
        contexts[0]
            .system_prompt
            .as_deref()
            .expect("system prompt")
            .chars()
            .count()
            - "be brief".chars().count(),
        reported.injected_chars
    );
}

#[tokio::test]
async fn context_usage_reports_reported_usage_and_injected_characters() {
    let usage = Usage {
        input_tokens: 11,
        output_tokens: 5,
        cached_input_tokens: None,
        reasoning_tokens: None,
    };
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(
        text_message_with_usage("ok", usage),
    )]));
    let item = ContextItem::new("memory", "1", "a snippet");
    let manager = Arc::new(ContextManager::new(vec![Arc::new(
        FakeContextProvider::new(vec![ProvideScript::Items(vec![item.clone()])]),
    )]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session =
        Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
            .expect("valid");

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let reported = session.context_usage();
    assert_eq!(reported.model_usage, Some(usage));
    assert_eq!(reported.injected_items, 1);
    // There is no base prompt, so the first snippet pays no separator.
    assert_eq!(reported.injected_chars, rendered_chars(&item));
    assert_eq!(reported.max_injected_chars, DEFAULT_MAX_INJECTED_CHARS);
}

#[tokio::test]
async fn a_provider_with_no_items_is_an_exact_no_op() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let provider_stub = Arc::new(FakeContextProvider::new(vec![ProvideScript::Items(
        Vec::new(),
    )]));
    let manager = Arc::new(ContextManager::new(vec![provider_stub]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_context(manager)
            .with_system_prompt("be brief"),
    )
    .expect("valid");

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    assert_eq!(
        provider.requests()[0].context.system_prompt.as_deref(),
        Some("be brief")
    );
    assert_eq!(session.context_usage().injected_chars, 0);
    assert_eq!(session.context_usage().injected_items, 0);
}

#[tokio::test]
async fn without_a_manager_the_request_context_is_untouched() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = session(registry, "main", resolver);

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let request = &provider.requests()[0];
    assert_eq!(request.context.system_prompt, None);
    assert_eq!(request.context.messages.len(), 1);
    assert!(request.context.tools.is_empty());
    assert_eq!(session.context_usage().injected_chars, 0);
    assert_eq!(session.context_usage().max_injected_chars, 0);
}

#[tokio::test]
async fn the_character_budget_keeps_high_priority_snippets_and_skips_the_rest() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let big = "x".repeat(100);
    let high = ContextItem::new("memory", "high", "KEEP-HIGH").with_priority(9);
    // The budget is exactly the high-priority snippet as it is rendered, label included.
    let budget = rendered_chars(&high);
    let context_provider = Arc::new(FakeContextProvider::new(vec![ProvideScript::Items(vec![
        ContextItem::new("memory", "low", &big).with_priority(1),
        high,
    ])]));
    let manager =
        Arc::new(ContextManager::new(vec![context_provider]).with_max_injected_chars(budget));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session =
        Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
            .expect("valid");

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let prompt = provider.requests()[0]
        .context
        .system_prompt
        .clone()
        .expect("system prompt");
    assert!(prompt.contains("KEEP-HIGH"));
    assert!(
        !prompt.contains(&big),
        "an oversized snippet is skipped whole"
    );
    assert_eq!(session.context_usage().injected_items, 1);
    // No base prompt exists, so the first snippet pays no separator.
    assert_eq!(session.context_usage().injected_chars, budget);
}

#[tokio::test]
async fn a_long_label_cannot_bypass_the_session_injection_budget() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![ProvideScript::Items(vec![
        ContextItem::new("s".repeat(200), "i".repeat(200), ""),
    ])]));
    let manager = Arc::new(ContextManager::new(vec![context_provider]).with_max_injected_chars(24));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_context(manager)
            .with_system_prompt("be brief"),
    )
    .expect("valid");

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let prompt = provider.requests()[0]
        .context
        .system_prompt
        .clone()
        .expect("system prompt");
    assert_eq!(prompt, "be brief", "the base prompt is preserved exactly");
    assert_eq!(session.context_usage().injected_items, 0);
    assert_eq!(session.context_usage().injected_chars, 0);
}

#[tokio::test]
async fn context_usage_is_owned_by_the_session_not_by_a_shared_manager() {
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(text_message("a")),
        Script::Stream(text_message("b")),
    ]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let context_provider = Arc::new(FakeContextProvider::new(vec![
        ProvideScript::Items(vec![ContextItem::new("memory", "1", "ONLY-FIRST")]),
        ProvideScript::Items(Vec::new()),
    ]));
    // Both sessions share one manager; their metrics must still be their own.
    let manager = Arc::new(ContextManager::new(vec![context_provider]));
    let resolver = Arc::new(FakeResolver::new(vec![
        ResolveScript::Credential("sk"),
        ResolveScript::Credential("sk"),
    ]));
    let first = Session::new(
        SessionConfig::new(registry.clone(), "main", resolver.clone())
            .with_context(manager.clone()),
    )
    .expect("valid");
    let second = Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
        .expect("valid");

    first
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");
    second
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    assert_eq!(first.context_usage().injected_items, 1);
    assert!(first.context_usage().injected_chars > 0);
    assert_eq!(second.context_usage().injected_items, 0);
    assert_eq!(second.context_usage().injected_chars, 0);
}

#[tokio::test]
async fn a_context_failure_fails_the_run_with_a_safe_category() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![ProvideScript::Failure]));
    let manager = Arc::new(ContextManager::new(vec![context_provider]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session =
        Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
            .expect("valid");

    let outcome = session.prompt("hi").expect("starts").outcome().await;
    assert_eq!(outcome.unwrap_err(), RuntimeError::Context);
    assert_eq!(provider.request_count(), 0);
}

#[tokio::test]
async fn cancelling_during_context_composition_returns_cancelled() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stall]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![ProvideScript::Stall]));
    let manager = Arc::new(ContextManager::new(vec![context_provider.clone()]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session =
        Session::new(SessionConfig::new(registry, "main", resolver).with_context(manager))
            .expect("valid");

    let run = session.prompt("hi").expect("starts");
    while context_provider.request_count() == 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    run.cancel();
    assert_eq!(run.outcome().await.unwrap_err(), RuntimeError::Cancelled);
    assert_eq!(provider.request_count(), 0);
    wait_idle(&session).await;
}

#[tokio::test]
async fn a_context_provider_never_adds_executable_tools() {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message("ok"))]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![ProvideScript::Items(vec![
        ContextItem::new("memory", "1", "text only"),
    ])]));
    let manager = Arc::new(ContextManager::new(vec![context_provider]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_context(manager)
            .with_tools(vec![Arc::new(Calculator::new()) as Arc<dyn Tool>]),
    )
    .expect("valid");

    session
        .prompt("hi")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let tools = &provider.requests()[0].context.tools;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "calculator");
}

#[tokio::test]
async fn the_query_of_a_context_request_is_the_latest_original_user_message() {
    let provider = Arc::new(FakeProvider::new(vec![
        Script::Stream(call_message(
            "working",
            vec![parsed_call("call-1", "calculator", json!({"a": 1, "b": 2}))],
        )),
        Script::Stream(text_message("done")),
    ]));
    let context_provider = Arc::new(FakeContextProvider::new(vec![
        ProvideScript::Items(Vec::new()),
        ProvideScript::Items(Vec::new()),
    ]));
    let manager = Arc::new(ContextManager::new(vec![context_provider.clone()]));
    let registry = registry(vec![binding("main", provider, "key")]);
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Credential("sk")]));
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver)
            .with_context(manager)
            .with_tools(vec![Arc::new(Calculator::new()) as Arc<dyn Tool>]),
    )
    .expect("valid");

    session
        .prompt("add two numbers")
        .expect("starts")
        .outcome()
        .await
        .expect("completes");

    let requests = context_provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.query.as_deref() == Some("add two numbers")));
    // The tool turn is committed: prompt, assistant call, tool result, assistant answer.
    let messages = session.snapshot().messages;
    assert_eq!(messages.len(), 4);
    assert!(matches!(
        messages.last(),
        Some(mira_runtime::Message::Assistant(_))
    ));
}
