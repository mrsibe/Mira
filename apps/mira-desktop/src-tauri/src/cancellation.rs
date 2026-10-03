use std::sync::{Arc, Mutex, MutexGuard};

use mira_runtime::CancellationToken;
use tauri::State;

/// The cancellation signal of the foreground chat run.
///
/// The frontend `cancel_message` command is push-driven: it cancels the registered runtime
/// token directly, so a stalled header, body or retry backoff stops immediately instead of
/// waiting for the next byte. At most one attempt owns the slot at a time, and every attempt
/// carries an opaque identity, so a superseded attempt can neither register against, release
/// nor cancel its successor.
pub struct CancellationState {
    slot: Mutex<CancelSlot>,
}

#[derive(Default)]
struct CancelSlot {
    /// Identity of the attempt allowed to register. A `reset` replaces it, which invalidates
    /// every earlier handle at once.
    attempt: Option<Arc<AttemptNonce>>,
    token: Option<Arc<CancellationToken>>,
    /// A cancel that arrived before the current attempt registered its token.
    requested: bool,
}

/// Opaque identity of one foreground attempt.
///
/// Minted by [`CancellationState::reset`] and carried from the start of message preparation to
/// [`CancellationState::register`]. Identity is pointer equality on an `Arc` nonce, so there is
/// no counter to overflow and no timestamp to compare. The handle is deliberately neither
/// `Clone` nor `Copy`: only the caller that began the attempt can present it.
pub(crate) struct Attempt {
    nonce: Arc<AttemptNonce>,
}

/// The allocation whose address identifies one attempt.
struct AttemptNonce;

/// Reported when a superseded attempt tries to register.
///
/// The attempt must cancel and drain its own run and stop; the current attempt's token and
/// latched cancel are left untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StaleAttempt;

impl CancellationState {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(CancelSlot::default()),
        }
    }

    /// Begin a new attempt, invalidating every earlier attempt handle atomically.
    ///
    /// The returned handle must be carried through message preparation and presented to
    /// [`CancellationState::register`]. The registered token and the latched cancel are cleared
    /// with the previous attempt identity.
    pub(crate) fn reset(&self) -> Attempt {
        let mut slot = lock(&self.slot);
        let nonce = Arc::new(AttemptNonce);
        slot.attempt = Some(Arc::clone(&nonce));
        slot.token = None;
        slot.requested = false;
        Attempt { nonce }
    }

    /// Whether `attempt` is still the current attempt.
    ///
    /// A cheap pre-check; [`CancellationState::register`] repeats it atomically.
    pub(crate) fn is_current(&self, attempt: &Attempt) -> bool {
        let slot = lock(&self.slot);
        matches_current(&slot, attempt)
    }

    /// Register the cancellation token of the current attempt's run.
    ///
    /// A cancel that arrived between [`CancellationState::reset`] and this call is applied to
    /// the new token immediately, so a run cannot ignore a cancel that raced its start. A stale
    /// attempt is refused with [`StaleAttempt`] without touching the current token or latch.
    pub(crate) fn register(
        &self,
        attempt: &Attempt,
        token: CancellationToken,
    ) -> Result<ActiveRun<'_>, StaleAttempt> {
        let mut slot = lock(&self.slot);
        if !matches_current(&slot, attempt) {
            return Err(StaleAttempt);
        }
        let token = Arc::new(token);
        if slot.requested {
            token.cancel();
            slot.requested = false;
        }
        slot.token = Some(Arc::clone(&token));
        Ok(ActiveRun {
            state: self,
            attempt: Arc::clone(&attempt.nonce),
            token,
        })
    }

    /// Cancel the registered run, or latch the request for the current attempt.
    pub(crate) fn cancel(&self) {
        let mut slot = lock(&self.slot);
        match slot.token.as_ref() {
            Some(token) => token.cancel(),
            None => slot.requested = true,
        }
    }
}

/// Whether the slot still belongs to `attempt`.
fn matches_current(slot: &CancelSlot, attempt: &Attempt) -> bool {
    slot.attempt
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &attempt.nonce))
}

/// Clears a run's registration when the run ends.
///
/// It only releases the slot when the slot still holds this attempt's token, so a finished run
/// can neither release nor cancel a successor that has already registered.
pub(crate) struct ActiveRun<'a> {
    state: &'a CancellationState,
    attempt: Arc<AttemptNonce>,
    token: Arc<CancellationToken>,
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        let mut slot = lock(&self.state.slot);
        let same_attempt = slot
            .attempt
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.attempt));
        let same_token = slot
            .token
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.token));
        if same_attempt && same_token {
            slot.token = None;
        }
    }
}

/// Lock the slot, ignoring poisoning: no lock is held across an await.
fn lock(slot: &Mutex<CancelSlot>) -> MutexGuard<'_, CancelSlot> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
pub fn cancel_message(state: State<'_, CancellationState>) -> Result<(), String> {
    state.cancel();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_a_registered_run() {
        let state = CancellationState::new();
        let attempt = state.reset();
        let run = CancellationToken::new();
        let _registration = state
            .register(&attempt, run.clone())
            .expect("current attempt");
        state.cancel();
        assert!(run.is_cancelled());

        // A fresh attempt is not cancelled by the cleared request.
        let next_attempt = state.reset();
        let next = CancellationToken::new();
        let _next_registration = state
            .register(&next_attempt, next.clone())
            .expect("current attempt");
        assert!(!next.is_cancelled());
    }

    #[test]
    fn cancel_reaches_the_registered_token() {
        let state = CancellationState::new();
        let attempt = state.reset();
        let run = CancellationToken::new();
        let _registration = state
            .register(&attempt, run.clone())
            .expect("current attempt");

        state.cancel();

        assert!(run.is_cancelled());
    }

    #[test]
    fn a_cancel_before_registration_is_applied_to_the_next_run() {
        let state = CancellationState::new();
        let attempt = state.reset();
        state.cancel();

        let run = CancellationToken::new();
        let _registration = state
            .register(&attempt, run.clone())
            .expect("current attempt");

        assert!(run.is_cancelled());
    }

    #[test]
    fn a_finished_run_does_not_release_a_successor() {
        let state = CancellationState::new();
        let first_attempt = state.reset();
        let first = CancellationToken::new();
        let first_registration = state
            .register(&first_attempt, first.clone())
            .expect("current attempt");

        let second_attempt = state.reset();
        let second = CancellationToken::new();
        let _second_registration = state
            .register(&second_attempt, second.clone())
            .expect("current attempt");
        drop(first_registration);

        // The successor is still registered and still cancellable.
        state.cancel();
        assert!(second.is_cancelled());
        assert!(!first.is_cancelled());
    }

    #[test]
    fn a_finished_run_does_not_cancel_a_successor() {
        let state = CancellationState::new();
        let first_attempt = state.reset();
        let first = CancellationToken::new();
        let first_registration = state
            .register(&first_attempt, first.clone())
            .expect("current attempt");

        let second_attempt = state.reset();
        let second = CancellationToken::new();
        let _second_registration = state
            .register(&second_attempt, second.clone())
            .expect("current attempt");
        drop(first_registration);

        assert!(!second.is_cancelled());
    }

    #[test]
    fn a_stale_registration_is_refused_and_cannot_steal_the_successor_token() {
        let state = CancellationState::new();
        let attempt_a = state.reset();
        let attempt_b = state.reset();

        let token_b = CancellationToken::new();
        let _registration_b = state
            .register(&attempt_b, token_b.clone())
            .expect("current attempt");

        // A resumes after B registered: its registration is refused and touches nothing.
        let token_a = CancellationToken::new();
        assert!(matches!(
            state.register(&attempt_a, token_a.clone()),
            Err(StaleAttempt)
        ));
        assert!(!token_a.is_cancelled());

        state.cancel();
        assert!(token_b.is_cancelled());
        assert!(!token_a.is_cancelled());
    }

    #[test]
    fn a_latched_cancel_survives_a_stale_registration() {
        let state = CancellationState::new();
        let attempt_a = state.reset();
        let attempt_b = state.reset();
        state.cancel();

        let token_a = CancellationToken::new();
        assert!(matches!(
            state.register(&attempt_a, token_a.clone()),
            Err(StaleAttempt)
        ));
        assert!(!token_a.is_cancelled());

        let token_b = CancellationToken::new();
        let _registration_b = state
            .register(&attempt_b, token_b.clone())
            .expect("current attempt");
        assert!(token_b.is_cancelled());
    }

    #[test]
    fn is_current_tracks_only_the_latest_attempt() {
        let state = CancellationState::new();
        let attempt_a = state.reset();
        assert!(state.is_current(&attempt_a));

        let attempt_b = state.reset();
        assert!(!state.is_current(&attempt_a));
        assert!(state.is_current(&attempt_b));
    }
}
