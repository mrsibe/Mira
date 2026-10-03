//! Preparation must not start after cancellation, drop or an expired deadline.

mod support;

use std::sync::Arc;
use std::time::Duration;

use mira_runtime::{RuntimeError, RuntimeLimits, Session, SessionConfig, DEFAULT_RUN_TIME};
use support::*;

fn fixture(total_time: Duration) -> (Session, Arc<FakeResolver>, Arc<FakeProvider>) {
    let provider = Arc::new(FakeProvider::new(vec![Script::Stream(text_message(
        "unused",
    ))]));
    // Panics synchronously if even the resolver's preparation is invoked.
    let resolver = Arc::new(FakeResolver::new(vec![ResolveScript::Panic]));
    let registry = registry(vec![binding("main", provider.clone(), "key")]);
    let session = Session::new(
        SessionConfig::new(registry, "main", resolver.clone()).with_runtime(RuntimeLimits {
            total_time,
            ..RuntimeLimits::default()
        }),
    )
    .expect("valid session");
    (session, resolver, provider)
}

fn assert_no_preparation(session: &Session, resolver: &FakeResolver, provider: &FakeProvider) {
    assert!(
        resolver.requests().is_empty(),
        "resolver must not be called"
    );
    assert_eq!(provider.request_count(), 0);
    assert!(session.snapshot().messages.is_empty());
    assert!(!session.is_busy());
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_the_first_driver_poll_skips_preparation() {
    let (session, resolver, provider) = fixture(DEFAULT_RUN_TIME);
    let run = session.prompt("cancel before polling").expect("starts");
    run.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), run.outcome())
        .await
        .expect("cancelled preparation must finish");
    assert_eq!(result.unwrap_err(), RuntimeError::Cancelled);
    assert_no_preparation(&session, &resolver, &provider);
}

#[tokio::test(flavor = "current_thread")]
async fn drop_before_the_first_driver_poll_skips_preparation() {
    let (session, resolver, provider) = fixture(DEFAULT_RUN_TIME);
    drop(session.prompt("drop before polling").expect("starts"));
    wait_idle(&session).await;
    assert_no_preparation(&session, &resolver, &provider);
}

#[tokio::test(flavor = "current_thread")]
async fn an_expired_deadline_skips_preparation() {
    let (session, resolver, provider) = fixture(Duration::ZERO);
    let run = session.prompt("no remaining time").expect("starts");
    let result = tokio::time::timeout(Duration::from_secs(1), run.outcome())
        .await
        .expect("expired preparation must finish");
    assert_eq!(result.unwrap_err(), RuntimeError::Timeout);
    assert_no_preparation(&session, &resolver, &provider);
}
