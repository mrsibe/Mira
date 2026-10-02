use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::State;

pub struct CancellationState {
    pub cancel_requested: Arc<AtomicBool>,
}

impl CancellationState {
    pub fn new() -> Self {
        Self {
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn reset(&self) {
        self.cancel_requested.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub fn cancel_message(state: State<'_, CancellationState>) -> Result<(), String> {
    state.cancel_requested.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_flag_starts_cleared() {
        let state = CancellationState::new();
        assert!(!state.cancel_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn reset_clears_a_shared_cancel_request() {
        let state = CancellationState::new();
        // The streaming loop reads a clone of this Arc, so the reset must be
        // visible through the shared handle.
        let shared = Arc::clone(&state.cancel_requested);

        state.cancel_requested.store(true, Ordering::SeqCst);
        assert!(shared.load(Ordering::SeqCst));

        state.reset();
        assert!(!shared.load(Ordering::SeqCst));
    }
}
