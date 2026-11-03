//! Agent-side lifecycle of inbound messages (TUI design 8.2).
//!
//! The agent loop reports [`MessageLifecycle::Started`] when it dequeues a
//! message and [`MessageLifecycle::Finished`] after any reply was handed to
//! `respond()`. Channels map the events to their wire format (gateway SSE
//! `turn_started` / `turn_finished`, REPLv3 `TurnStarted` / `TurnFinished`).
//!
//! The agent loop only sees the handler's result, so code that knows more (the
//! V1 result arms, the V2 completion, the V2 detach) records an outcome hint with
//! `ChannelManager::note_turn_outcome`; `ChannelManager::message_lifecycle`
//! applies it once when the message finishes.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use uuid::Uuid;

pub use lunarwing_common::TurnOutcome;

/// One lifecycle event of an inbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLifecycle {
    /// The agent dequeued the message and started handling it.
    Started,
    /// Handling ended; any reply was handed to `respond()` before this event.
    Finished(TurnOutcome),
}

/// `(message id, hint, recorded at)`.
type HintEntry = (Uuid, TurnOutcome, Instant);

/// Outcome refinements recorded by the code that knows (V1 result arms, V2 completion,
/// V2 detach) and applied once when the agent reports `Finished`. Bounded, std lock,
/// never held across `.await`.
#[derive(Debug, Default)]
pub(crate) struct OutcomeHints {
    inner: Mutex<VecDeque<HintEntry>>,
}

impl OutcomeHints {
    /// Hints kept at most; the oldest is evicted first.
    pub(crate) const MAX_ENTRIES: usize = 1024;
    /// A hint older than this is ignored and dropped.
    pub(crate) const TTL: Duration = Duration::from_secs(1800);

    /// Records `hint` for `message_id`, replacing an older hint for the same id.
    ///
    /// [`TurnOutcome::Unknown`] is a reader-side value and is ignored.
    pub(crate) fn note(&self, message_id: Uuid, hint: TurnOutcome) {
        self.note_at(message_id, hint, Instant::now());
    }

    /// Returns the outcome to report for `message_id`.
    ///
    /// Hints replace only `Replied` and `NoReply`; `Failed`, `TimedOut` and `Cancelled`
    /// computed by the agent loop are authoritative. The hint is removed either way, so a
    /// later `Finished` for the same message (the late watcher after `Detached`) sees only
    /// hints recorded after this call.
    pub(crate) fn resolve(&self, message_id: Uuid, computed: TurnOutcome) -> TurnOutcome {
        self.resolve_at(message_id, computed, Instant::now())
    }

    fn note_at(&self, message_id: Uuid, hint: TurnOutcome, now: Instant) {
        if hint == TurnOutcome::Unknown {
            return;
        }
        let mut hints = self.lock();
        purge_expired(&mut hints, now);
        hints.retain(|(id, _, _)| *id != message_id);
        hints.push_back((message_id, hint, now));
        while hints.len() > Self::MAX_ENTRIES {
            hints.pop_front();
        }
    }

    fn resolve_at(&self, message_id: Uuid, computed: TurnOutcome, now: Instant) -> TurnOutcome {
        let mut hints = self.lock();
        purge_expired(&mut hints, now);
        let hint = hints
            .iter()
            .position(|(id, _, _)| *id == message_id)
            .and_then(|index| hints.remove(index))
            .map(|(_, hint, _)| hint);
        match (hint, computed) {
            (Some(hint), TurnOutcome::Replied | TurnOutcome::NoReply) => hint,
            _ => computed,
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<HintEntry>> {
        // A poisoned lock only means another thread panicked between two complete
        // updates; the deque itself is always consistent, so keep using it.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

fn purge_expired(hints: &mut VecDeque<HintEntry>, now: Instant) {
    hints.retain(|(_, _, at)| now.saturating_duration_since(*at) < OutcomeHints::TTL);
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTHORITATIVE: [TurnOutcome; 3] = [
        TurnOutcome::Failed,
        TurnOutcome::TimedOut,
        TurnOutcome::Cancelled,
    ];
    const HINTS: [TurnOutcome; 4] = [
        TurnOutcome::Paused,
        TurnOutcome::Interrupted,
        TurnOutcome::Detached,
        TurnOutcome::Failed,
    ];

    #[test]
    fn hint_overrides_replied_and_no_reply() {
        let hints = OutcomeHints::default();
        for computed in [TurnOutcome::Replied, TurnOutcome::NoReply] {
            for hint in HINTS {
                let id = Uuid::new_v4();
                hints.note(id, hint);
                assert_eq!(hints.resolve(id, computed), hint, "{hint} over {computed}");
            }
        }
        // Without a hint the computed outcome is reported unchanged.
        assert_eq!(
            hints.resolve(Uuid::new_v4(), TurnOutcome::Replied),
            TurnOutcome::Replied
        );
        // A newer hint replaces an older one for the same message.
        let id = Uuid::new_v4();
        hints.note(id, TurnOutcome::Paused);
        hints.note(id, TurnOutcome::Interrupted);
        assert_eq!(hints.len(), 1);
        assert_eq!(
            hints.resolve(id, TurnOutcome::NoReply),
            TurnOutcome::Interrupted
        );
    }

    #[test]
    fn hint_never_overrides_failed_timed_out_cancelled() {
        let hints = OutcomeHints::default();
        for computed in AUTHORITATIVE {
            for hint in HINTS {
                let id = Uuid::new_v4();
                hints.note(id, hint);
                assert_eq!(
                    hints.resolve(id, computed),
                    computed,
                    "{hint} over {computed}"
                );
                // The hint was removed even though it did not apply.
                assert_eq!(
                    hints.resolve(id, TurnOutcome::NoReply),
                    TurnOutcome::NoReply
                );
            }
        }
        // `Unknown` is never recorded.
        let id = Uuid::new_v4();
        hints.note(id, TurnOutcome::Unknown);
        assert_eq!(hints.len(), 0);
        assert_eq!(
            hints.resolve(id, TurnOutcome::Replied),
            TurnOutcome::Replied
        );
    }

    #[test]
    fn hint_is_consumed_once() {
        let hints = OutcomeHints::default();
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        hints.note(id, TurnOutcome::Detached);
        hints.note(other, TurnOutcome::Paused);

        assert_eq!(
            hints.resolve(id, TurnOutcome::NoReply),
            TurnOutcome::Detached
        );
        assert_eq!(
            hints.resolve(id, TurnOutcome::NoReply),
            TurnOutcome::NoReply
        );
        // The late watcher's own hint (recorded after the first finish) applies to the second.
        hints.note(id, TurnOutcome::Failed);
        assert_eq!(hints.resolve(id, TurnOutcome::Replied), TurnOutcome::Failed);
        assert_eq!(
            hints.resolve(id, TurnOutcome::Replied),
            TurnOutcome::Replied
        );
        // Other messages keep their hints.
        assert_eq!(
            hints.resolve(other, TurnOutcome::NoReply),
            TurnOutcome::Paused
        );
        assert_eq!(hints.len(), 0);
    }

    #[test]
    fn hints_are_bounded_and_expire() {
        let hints = OutcomeHints::default();
        let start = Instant::now();
        let ids: Vec<Uuid> = (0..=OutcomeHints::MAX_ENTRIES)
            .map(|_| Uuid::new_v4())
            .collect();
        for id in &ids {
            hints.note_at(*id, TurnOutcome::Paused, start);
        }
        assert_eq!(hints.len(), OutcomeHints::MAX_ENTRIES);
        // The oldest hint was evicted; the newest is still there.
        assert_eq!(
            hints.resolve_at(ids[0], TurnOutcome::NoReply, start),
            TurnOutcome::NoReply
        );
        let newest = ids[OutcomeHints::MAX_ENTRIES];
        assert_eq!(
            hints.resolve_at(newest, TurnOutcome::NoReply, start),
            TurnOutcome::Paused
        );

        // A hint is honoured until its TTL and ignored afterwards.
        let fresh = Uuid::new_v4();
        let stale = Uuid::new_v4();
        hints.note_at(fresh, TurnOutcome::Interrupted, start);
        hints.note_at(stale, TurnOutcome::Interrupted, start);
        let almost = start + OutcomeHints::TTL - Duration::from_secs(1);
        assert_eq!(
            hints.resolve_at(fresh, TurnOutcome::NoReply, almost),
            TurnOutcome::Interrupted
        );
        let expired = start + OutcomeHints::TTL;
        assert_eq!(
            hints.resolve_at(stale, TurnOutcome::NoReply, expired),
            TurnOutcome::NoReply
        );
        // Expiry also purges every other stale entry.
        assert_eq!(hints.len(), 0);
    }
}
