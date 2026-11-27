//! Tool-result images waiting for the next Engine V2 model request.
//!
//! The effect adapter parks the validated images a tool returned (see
//! `crate::tools::images`) under the engine thread that ran the action; the
//! engine takes them, per thread, right before its next main model request
//! (`EffectExecutor::take_action_images`) and shows them next to the step
//! that produced them. The text result the engine receives is unchanged, and
//! nothing here is persisted.
//!
//! The mailbox is bounded by count, bytes and age, so images whose thread
//! never makes another model request (a stopped run, a gate pause that is
//! never answered) cannot pile up. Its lock is a std mutex that is never
//! held across `.await`.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lunarwing_engine::{ActionImages, ThreadId};

/// Image batches kept at most across all threads; the oldest go first.
pub(crate) const MAX_PENDING_BATCHES: usize = 16;
/// Decoded image bytes kept at most across all threads.
pub(crate) const MAX_PENDING_BYTES: usize = 32 * 1024 * 1024;
/// A batch nobody took within this time is dropped.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(300);

struct Pending {
    thread_id: ThreadId,
    at: Instant,
    bytes: usize,
    images: ActionImages,
}

/// Images returned by actions, waiting for their thread's next model request.
#[derive(Default)]
pub(crate) struct ActionImageMailbox {
    inner: Mutex<VecDeque<Pending>>,
}

impl ActionImageMailbox {
    /// Park `images` for `thread_id`. Empty batches are ignored.
    pub(crate) fn put(&self, thread_id: ThreadId, images: ActionImages) {
        self.put_at(thread_id, images, Instant::now());
    }

    /// Remove and return every batch parked for `thread_id`, oldest first.
    pub(crate) fn take(&self, thread_id: ThreadId) -> Vec<ActionImages> {
        self.take_at(thread_id, Instant::now())
    }

    fn put_at(&self, thread_id: ThreadId, images: ActionImages, now: Instant) {
        if images.parts.is_empty() {
            return;
        }
        let bytes = images.byte_len();
        if bytes > MAX_PENDING_BYTES {
            return;
        }
        let mut pending = self.lock();
        sweep_expired(&mut pending, now);
        pending.push_back(Pending {
            thread_id,
            at: now,
            bytes,
            images,
        });
        let mut total: usize = pending.iter().map(|entry| entry.bytes).sum();
        while pending.len() > MAX_PENDING_BATCHES || total > MAX_PENDING_BYTES {
            let Some(evicted) = pending.pop_front() else {
                break;
            };
            total -= evicted.bytes;
        }
    }

    fn take_at(&self, thread_id: ThreadId, now: Instant) -> Vec<ActionImages> {
        let mut pending = self.lock();
        sweep_expired(&mut pending, now);
        let mut taken = Vec::new();
        pending.retain_mut(|entry| {
            if entry.thread_id == thread_id {
                taken.push(std::mem::replace(
                    &mut entry.images,
                    ActionImages {
                        call_id: String::new(),
                        action_name: String::new(),
                        parts: Vec::new(),
                    },
                ));
                false
            } else {
                true
            }
        });
        taken
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Pending>> {
        // A poisoned lock only means another thread panicked mid-update; the
        // queue itself is still structurally valid.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn sweep_expired(pending: &mut VecDeque<Pending>, now: Instant) {
    pending.retain(|entry| now.saturating_duration_since(entry.at) < PENDING_TTL);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunarwing_engine::TransientContentPart;

    fn batch(call_id: &str, size: usize) -> ActionImages {
        ActionImages {
            call_id: call_id.to_string(),
            action_name: "desktop_screenshot".to_string(),
            parts: vec![TransientContentPart::Image {
                mime_type: "image/png".to_string(),
                data: vec![0u8; size],
            }],
        }
    }

    #[test]
    fn take_returns_only_that_threads_batches_in_order() {
        let mailbox = ActionImageMailbox::default();
        let (a, b) = (ThreadId::new(), ThreadId::new());
        mailbox.put(a, batch("a1", 4));
        mailbox.put(b, batch("b1", 4));
        mailbox.put(a, batch("a2", 4));

        let taken: Vec<String> = mailbox
            .take(a)
            .into_iter()
            .map(|images| images.call_id)
            .collect();
        assert_eq!(taken, vec!["a1", "a2"]);
        assert!(mailbox.take(a).is_empty(), "taken once");
        assert_eq!(mailbox.take(b).len(), 1);
    }

    #[test]
    fn bounded_by_count_bytes_and_age() {
        let mailbox = ActionImageMailbox::default();
        let thread = ThreadId::new();
        let start = Instant::now();
        for index in 0..(MAX_PENDING_BATCHES + 3) {
            mailbox.put_at(thread, batch(&format!("c{index}"), 1), start);
        }
        let taken = mailbox.take_at(thread, start);
        assert_eq!(taken.len(), MAX_PENDING_BATCHES);
        assert_eq!(taken[0].call_id, "c3", "oldest evicted first");

        mailbox.put_at(thread, batch("big1", MAX_PENDING_BYTES / 2 + 1), start);
        mailbox.put_at(thread, batch("big2", MAX_PENDING_BYTES / 2 + 1), start);
        let taken = mailbox.take_at(thread, start);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].call_id, "big2");

        mailbox.put_at(thread, batch("old", 1), start);
        assert!(
            mailbox
                .take_at(thread, start + PENDING_TTL + Duration::from_secs(1))
                .is_empty(),
            "expired batches are dropped"
        );
        mailbox.put_at(thread, batch("empty", 0), start);
        let empty = ActionImages {
            parts: Vec::new(),
            ..batch("none", 0)
        };
        mailbox.put_at(thread, empty, start);
        assert_eq!(
            mailbox.take_at(thread, start).len(),
            1,
            "zero-byte image kept, no parts ignored"
        );
    }
}
