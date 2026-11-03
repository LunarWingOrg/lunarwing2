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
//! applies it once when the message finishes. The table also remembers which
//! messages finished, so a hint that arrives too late is dropped and the V2 late
//! watcher can tell that the agent loop already ended a message
//! (`ChannelManager::has_finished`).

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
/// V2 detach) and applied once when the agent reports `Finished`, plus the messages that
/// already finished. Bounded, std lock, never held across `.await`.
#[derive(Debug, Default)]
pub(crate) struct OutcomeHints {
    inner: Mutex<HintTable>,
}

#[derive(Debug, Default)]
struct HintTable {
    hints: VecDeque<HintEntry>,
    /// `(message id, reported at)` of every message whose `Finished` ended it: any
    /// outcome but `Detached`, which another `Finished` follows.
    finished: VecDeque<(Uuid, Instant)>,
}

impl OutcomeHints {
    /// Hints kept at most, and finished messages remembered at most; the oldest entry is
    /// evicted first.
    pub(crate) const MAX_ENTRIES: usize = 1024;
    /// A hint or finished entry older than this is ignored and dropped.
    pub(crate) const TTL: Duration = Duration::from_secs(1800);

    /// Records `hint` for `message_id`, replacing an older hint for the same id.
    ///
    /// [`TurnOutcome::Unknown`] is a reader-side value and is ignored, and so is a hint
    /// for a message that already finished: no later `Finished` would consume it.
    pub(crate) fn note(&self, message_id: Uuid, hint: TurnOutcome) {
        self.note_at(message_id, hint, Instant::now());
    }

    /// Returns the outcome to report for `message_id`.
    ///
    /// Hints replace only `Replied` and `NoReply`; `Failed`, `TimedOut` and `Cancelled`
    /// computed by the agent loop are authoritative. The hint is removed either way, so a
    /// later `Finished` for the same message (the late watcher after `Detached`) sees only
    /// hints recorded after this call. Any outcome but `Detached` marks the message
    /// finished ([`Self::has_finished`]).
    pub(crate) fn resolve(&self, message_id: Uuid, computed: TurnOutcome) -> TurnOutcome {
        self.resolve_at(message_id, computed, Instant::now())
    }

    /// True once a `Finished` other than `Detached` was resolved for `message_id`, within
    /// [`Self::TTL`] and the [`Self::MAX_ENTRIES`] most recent finishes.
    pub(crate) fn has_finished(&self, message_id: Uuid) -> bool {
        self.has_finished_at(message_id, Instant::now())
    }

    fn note_at(&self, message_id: Uuid, hint: TurnOutcome, now: Instant) {
        if hint == TurnOutcome::Unknown {
            return;
        }
        let mut table = self.lock();
        table.purge_expired(now);
        if table.finished.iter().any(|(id, _)| *id == message_id) {
            return;
        }
        let hints = &mut table.hints;
        hints.retain(|(id, _, _)| *id != message_id);
        hints.push_back((message_id, hint, now));
        while hints.len() > Self::MAX_ENTRIES {
            hints.pop_front();
        }
    }

    fn resolve_at(&self, message_id: Uuid, computed: TurnOutcome, now: Instant) -> TurnOutcome {
        let mut table = self.lock();
        table.purge_expired(now);
        let hints = &mut table.hints;
        let hint = hints
            .iter()
            .position(|(id, _, _)| *id == message_id)
            .and_then(|index| hints.remove(index))
            .map(|(_, hint, _)| hint);
        let outcome = match (hint, computed) {
            (Some(hint), TurnOutcome::Replied | TurnOutcome::NoReply) => hint,
            _ => computed,
        };
        if outcome != TurnOutcome::Detached {
            let finished = &mut table.finished;
            finished.retain(|(id, _)| *id != message_id);
            finished.push_back((message_id, now));
            while finished.len() > Self::MAX_ENTRIES {
                finished.pop_front();
            }
        }
        outcome
    }

    fn has_finished_at(&self, message_id: Uuid, now: Instant) -> bool {
        let mut table = self.lock();
        table.purge_expired(now);
        table.finished.iter().any(|(id, _)| *id == message_id)
    }

    fn lock(&self) -> MutexGuard<'_, HintTable> {
        // A poisoned lock only means another thread panicked between two complete
        // updates; the table itself is always consistent, so keep using it.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().hints.len()
    }
}

impl HintTable {
    fn purge_expired(&mut self, now: Instant) {
        let fresh = |at: &Instant| now.saturating_duration_since(*at) < OutcomeHints::TTL;
        self.hints.retain(|(_, _, at)| fresh(at));
        self.finished.retain(|(_, at)| fresh(at));
    }
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
        // The late watcher's own hint (recorded after the first finish) applies to the second.
        hints.note(id, TurnOutcome::Failed);
        assert_eq!(hints.resolve(id, TurnOutcome::Replied), TurnOutcome::Failed);
        // Consumed: nothing is left for the message.
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

    #[test]
    fn finished_message_drops_later_hints() {
        let hints = OutcomeHints::default();
        for outcome in [
            TurnOutcome::Replied,
            TurnOutcome::NoReply,
            TurnOutcome::Paused,
            TurnOutcome::Interrupted,
            TurnOutcome::Failed,
            TurnOutcome::TimedOut,
            TurnOutcome::Cancelled,
        ] {
            let id = Uuid::new_v4();
            assert!(!hints.has_finished(id));
            assert_eq!(hints.resolve(id, outcome), outcome);
            assert!(hints.has_finished(id), "{outcome}");
            // A hint recorded after the finish (Engine V2 detaching after the loop's soft
            // timeout) is dropped instead of waiting for a finish that never comes.
            hints.note(id, TurnOutcome::Detached);
            assert_eq!(hints.len(), 0, "{outcome}");
        }
    }

    #[test]
    fn detached_finish_keeps_the_message_open_for_the_late_finish() {
        let hints = OutcomeHints::default();
        let id = Uuid::new_v4();
        hints.note(id, TurnOutcome::Detached);
        assert_eq!(
            hints.resolve(id, TurnOutcome::NoReply),
            TurnOutcome::Detached
        );
        assert!(!hints.has_finished(id));
        // The late completion's hint still applies to the late finish, which ends it.
        hints.note(id, TurnOutcome::Interrupted);
        assert_eq!(
            hints.resolve(id, TurnOutcome::NoReply),
            TurnOutcome::Interrupted
        );
        assert!(hints.has_finished(id));
    }

    #[test]
    fn finished_messages_are_bounded_and_expire() {
        let hints = OutcomeHints::default();
        let start = Instant::now();
        let ids: Vec<Uuid> = (0..=OutcomeHints::MAX_ENTRIES)
            .map(|_| Uuid::new_v4())
            .collect();
        for id in &ids {
            hints.resolve_at(*id, TurnOutcome::Replied, start);
        }
        // The oldest finish is forgotten; the newest is remembered.
        assert!(!hints.has_finished_at(ids[0], start));
        let newest = ids[OutcomeHints::MAX_ENTRIES];
        assert!(hints.has_finished_at(newest, start));
        // Remembered until the TTL, forgotten afterwards.
        let almost = start + OutcomeHints::TTL - Duration::from_secs(1);
        assert!(hints.has_finished_at(newest, almost));
        assert!(!hints.has_finished_at(newest, start + OutcomeHints::TTL));
    }
}
