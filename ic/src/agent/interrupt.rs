//! Mid-turn interrupt plumbing: per-thread cancellation tokens.
//!
//! The classic agentic loop used to notice interrupts only at iteration
//! boundaries (`ThreadState::Interrupted` check in `check_signals`). This
//! registry hands every running chat turn a [`tokio_util::sync::CancellationToken`]
//! so `/interrupt` can abort an in-flight LLM stream or tool batch
//! mid-iteration via `tokio::select!` rather than waiting for the step to
//! finish.
//!
//! Lifecycle: the loop registers a token when a turn starts (replacing any
//! stale token for that thread) and unregisters it when the turn ends via an
//! RAII guard. `process_interrupt` cancels the token; the guard then removes
//! it on drop if it is still the live registration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Registry of live turn cancellation tokens, keyed by thread ID.
///
/// Cloning shares the underlying map. Uses a std mutex because the critical
/// sections are tiny (map get/insert/remove) and never hold the lock across
/// an `.await`.
#[derive(Clone, Default)]
pub(crate) struct TurnInterrupts {
    tokens: Arc<Mutex<HashMap<Uuid, CancellationToken>>>,
}

impl TurnInterrupts {
    /// Register a fresh token for `thread_id`, replacing any stale entry.
    ///
    /// Returns an RAII registration whose drop unregisters the token (only if
    /// it is still the live one for the thread).
    pub(crate) fn register(&self, thread_id: Uuid) -> InterruptRegistration {
        let token = CancellationToken::new();
        {
            let mut guard = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(thread_id, token.clone());
        }
        InterruptRegistration {
            registry: self.clone(),
            thread_id,
            token,
        }
    }

    /// Cancel the live token for `thread_id`, if one is registered.
    ///
    /// Returns `true` when a token was found and cancelled. A `false` result
    /// means no turn loop holds a token for the thread (e.g. the loop already
    /// left the agentic loop and is awaiting approval or finished), in which
    /// case the turn-boundary state flag remains the interrupt path.
    pub(crate) fn cancel(&self, thread_id: Uuid) -> bool {
        let token = {
            let guard = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(&thread_id).cloned()
        };
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Remove `token` for `thread_id` only if it is still the live entry, so a
    /// lagging guard cannot evict a newer turn's registration.
    fn unregister(&self, thread_id: Uuid, token: &CancellationToken) {
        let mut guard = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if guard.get(&thread_id).is_some_and(|live| live == token) {
            guard.remove(&thread_id);
        }
    }

    /// Number of live registrations (diagnostics/tests only).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        let guard = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        guard.len()
    }
}

/// RAII handle for a thread's turn token. Dropping unregisters the token.
pub(crate) struct InterruptRegistration {
    registry: TurnInterrupts,
    thread_id: Uuid,
    token: CancellationToken,
}

impl InterruptRegistration {
    /// The token the agentic loop should watch for mid-turn interrupts.
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for InterruptRegistration {
    fn drop(&mut self) {
        self.registry.unregister(self.thread_id, &self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_cancel_marks_token_cancelled() {
        let registry = TurnInterrupts::default();
        let thread_id = Uuid::new_v4();

        assert!(!registry.cancel(thread_id), "no token registered yet");

        let registration = registry.register(thread_id);
        assert!(!registration.token().is_cancelled());
        assert_eq!(registry.len(), 1);

        assert!(registry.cancel(thread_id));
        assert!(registration.token().is_cancelled());

        drop(registration);
        assert_eq!(registry.len(), 0);
        assert!(!registry.cancel(thread_id), "token already unregistered");
    }

    #[test]
    fn dropping_guard_does_not_evict_newer_registration() {
        let registry = TurnInterrupts::default();
        let thread_id = Uuid::new_v4();

        let stale = registry.register(thread_id);
        let fresh = registry.register(thread_id);

        drop(stale);
        assert_eq!(registry.len(), 1, "fresh registration must survive");

        assert!(registry.cancel(thread_id));
        assert!(fresh.token().is_cancelled());

        drop(fresh);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn registry_clones_share_the_same_map() {
        let registry = TurnInterrupts::default();
        let clone = registry.clone();
        let thread_id = Uuid::new_v4();

        let registration = registry.register(thread_id);
        assert!(clone.cancel(thread_id));
        assert!(registration.token().is_cancelled());
    }
}
