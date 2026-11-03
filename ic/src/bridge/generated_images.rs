//! Generated images waiting for their `ActionExecuted` event (TUI design 8.3.6).
//!
//! Engine V2 sanitizes and truncates tool output before the engine sees it, and the
//! `ActionExecuted` event carries only a short preview, so the image a tool returned
//! never reaches the router intact. The effect adapter parks the image here before
//! sanitizing, keyed by thread and call id; the router takes it when it forwards the
//! matching event and sends `StatusUpdate::ImageGenerated` instead of the preview.
//!
//! The stash is bounded by count, bytes and age, so images whose event never arrives
//! (a lagged event receiver, a thread nobody waits for) cannot pile up. Its lock is a
//! std mutex that is never held across `.await`.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lunarwing_engine::ThreadId;

use crate::tools::builtin::image_gen::GeneratedImage;

/// Images kept at most; the oldest is evicted first.
pub(crate) const MAX_STASHED_IMAGES: usize = 8;
/// Bytes kept at most (data URLs, paths and call ids); the oldest images go first.
pub(crate) const MAX_STASHED_BYTES: usize = 32 * 1024 * 1024;
/// An image nobody took within this time is dropped.
pub(crate) const STASH_TTL: Duration = Duration::from_secs(600);

struct StashedImage {
    thread_id: ThreadId,
    call_id: String,
    at: Instant,
    image: GeneratedImage,
}

impl StashedImage {
    fn bytes(&self) -> usize {
        self.image.data_url.len()
            + self.image.path.as_ref().map_or(0, String::len)
            + self.call_id.len()
    }

    fn is_for(&self, thread_id: ThreadId, call_id: &str) -> bool {
        self.thread_id == thread_id && self.call_id == call_id
    }
}

/// Images returned by image tools in channel threads, waiting for the router.
#[derive(Default)]
pub(crate) struct GeneratedImageStash {
    inner: Mutex<VecDeque<StashedImage>>,
}

impl GeneratedImageStash {
    /// Parks `image` for the call `call_id` of `thread_id`, replacing an older image
    /// for the same call.
    pub(crate) fn put(&self, thread_id: ThreadId, call_id: String, image: GeneratedImage) {
        self.put_at(thread_id, call_id, image, Instant::now());
    }

    /// Removes and returns the image parked for the call `call_id` of `thread_id`.
    pub(crate) fn take(&self, thread_id: ThreadId, call_id: &str) -> Option<GeneratedImage> {
        self.take_at(thread_id, call_id, Instant::now())
    }

    fn put_at(&self, thread_id: ThreadId, call_id: String, image: GeneratedImage, now: Instant) {
        let mut images = self.lock();
        sweep_expired(&mut images, now);
        images.retain(|stashed| !stashed.is_for(thread_id, &call_id));
        images.push_back(StashedImage {
            thread_id,
            call_id,
            at: now,
            image,
        });
        evict_over_budget(&mut images);
    }

    fn take_at(&self, thread_id: ThreadId, call_id: &str, now: Instant) -> Option<GeneratedImage> {
        let mut images = self.lock();
        sweep_expired(&mut images, now);
        let index = images
            .iter()
            .position(|stashed| stashed.is_for(thread_id, call_id))?;
        images.remove(index).map(|stashed| stashed.image)
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<StashedImage>> {
        // A poisoned lock only means another thread panicked between two complete
        // updates; the deque itself is always consistent, so keep using it.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

fn sweep_expired(images: &mut VecDeque<StashedImage>, now: Instant) {
    images.retain(|stashed| now.saturating_duration_since(stashed.at) < STASH_TTL);
}

fn evict_over_budget(images: &mut VecDeque<StashedImage>) {
    let mut bytes: usize = images.iter().map(StashedImage::bytes).sum();
    while images.len() > MAX_STASHED_IMAGES || bytes > MAX_STASHED_BYTES {
        let Some(oldest) = images.pop_front() else {
            break;
        };
        bytes = bytes.saturating_sub(oldest.bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "data:image/png;base64,";

    /// An image whose data URL is `bytes` long (at least the prefix).
    fn image(bytes: usize, tag: char) -> GeneratedImage {
        let body = tag.to_string().repeat(bytes.saturating_sub(PREFIX.len()));
        GeneratedImage {
            data_url: format!("{PREFIX}{body}"),
            path: None,
        }
    }

    #[test]
    fn stash_take_matches_thread_and_call() {
        let stash = GeneratedImageStash::default();
        let thread = ThreadId::new();
        let other_thread = ThreadId::new();
        stash.put(thread, "call-1".into(), image(64, 'A'));
        stash.put(other_thread, "call-1".into(), image(64, 'B'));

        // Another call of the same thread takes nothing.
        assert_eq!(stash.take(thread, "call-2"), None);
        // The same call id in two threads names two images.
        assert_eq!(stash.take(thread, "call-1"), Some(image(64, 'A')));
        assert_eq!(stash.take(other_thread, "call-1"), Some(image(64, 'B')));
        // Taking removes the image.
        assert_eq!(stash.take(thread, "call-1"), None);
        assert_eq!(stash.len(), 0);

        // A second image for the same call replaces the first.
        stash.put(thread, "call-3".into(), image(64, 'C'));
        stash.put(thread, "call-3".into(), image(80, 'D'));
        assert_eq!(stash.len(), 1);
        assert_eq!(stash.take(thread, "call-3"), Some(image(80, 'D')));
    }

    #[test]
    fn stash_evicts_the_oldest_image_by_count() {
        let thread = ThreadId::new();
        let start = Instant::now();

        // The ninth image evicts the oldest.
        let stash = GeneratedImageStash::default();
        for index in 0..=MAX_STASHED_IMAGES {
            stash.put_at(thread, format!("call-{index}"), image(64, 'A'), start);
        }
        assert_eq!(stash.len(), MAX_STASHED_IMAGES);
        assert_eq!(stash.take_at(thread, "call-0", start), None);
        assert!(stash.take_at(thread, "call-1", start).is_some());
        assert!(
            stash
                .take_at(thread, &format!("call-{MAX_STASHED_IMAGES}"), start)
                .is_some()
        );
    }

    #[test]
    fn stash_evicts_the_oldest_images_by_bytes() {
        let thread = ThreadId::new();
        let start = Instant::now();

        // Three images of a third of the budget plus one byte do not fit.
        let stash = GeneratedImageStash::default();
        let third = MAX_STASHED_BYTES / 3 + 1;
        for index in 0..3 {
            stash.put_at(thread, format!("big-{index}"), image(third, 'B'), start);
        }
        assert_eq!(stash.len(), 2);
        assert_eq!(stash.take_at(thread, "big-0", start), None);
        assert!(stash.take_at(thread, "big-1", start).is_some());
        assert!(stash.take_at(thread, "big-2", start).is_some());

        // An image larger than the whole budget is not kept either.
        let stash = GeneratedImageStash::default();
        stash.put_at(
            thread,
            "huge".into(),
            image(MAX_STASHED_BYTES + 1, 'C'),
            start,
        );
        assert_eq!(stash.len(), 0);
    }

    #[test]
    fn stash_drops_images_once_they_are_ttl_old() {
        let thread = ThreadId::new();
        let start = Instant::now();

        // An image is dropped once it is STASH_TTL old, by take and by put.
        let stash = GeneratedImageStash::default();
        let expired = start + STASH_TTL;
        stash.put_at(thread, "old".into(), image(64, 'D'), start);
        assert!(
            stash
                .take_at(thread, "old", expired - Duration::from_millis(1))
                .is_some()
        );
        stash.put_at(thread, "old".into(), image(64, 'D'), start);
        assert_eq!(stash.take_at(thread, "old", expired), None);
        stash.put_at(thread, "stale".into(), image(64, 'E'), start);
        stash.put_at(thread, "fresh".into(), image(64, 'F'), expired);
        assert_eq!(stash.len(), 1);
        assert!(stash.take_at(thread, "fresh", expired).is_some());
    }
}
