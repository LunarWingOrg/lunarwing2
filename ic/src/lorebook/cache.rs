use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use lru::LruCache;
use uuid::Uuid;

use crate::lorebook::LorebookEntry;
use crate::workspace::WorkspaceEntry;

const CACHE_SCOPES: usize = 8;
const EMPTY_CACHE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ScopeKey {
    user_id: String,
    agent_id: Option<Uuid>,
}

impl ScopeKey {
    pub(super) fn new(user_id: &str, agent_id: Option<Uuid>) -> Self {
        Self {
            user_id: user_id.to_string(),
            agent_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DocumentRevision {
    path: String,
    updated_at: Option<DateTime<Utc>>,
}

impl From<&WorkspaceEntry> for DocumentRevision {
    fn from(document: &WorkspaceEntry) -> Self {
        Self {
            path: document.path.clone(),
            updated_at: document.updated_at,
        }
    }
}

struct CachedCorpus {
    revisions: Vec<DocumentRevision>,
    entries: Arc<[LorebookEntry]>,
    cached_at: Instant,
}

pub(super) struct LorebookCache {
    scopes: Mutex<LruCache<ScopeKey, CachedCorpus>>,
}

impl LorebookCache {
    fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity clamped to one");
        Self {
            scopes: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub(super) fn get(
        &self,
        key: &ScopeKey,
        revisions: &[DocumentRevision],
    ) -> Option<Arc<[LorebookEntry]>> {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let cached = scopes.get(key)?;
        (cached.revisions == revisions).then(|| Arc::clone(&cached.entries))
    }

    pub(super) fn get_recent_empty(&self, key: &ScopeKey) -> Option<Arc<[LorebookEntry]>> {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let cached = scopes.get(key)?;
        (cached.revisions.is_empty() && cached.cached_at.elapsed() < EMPTY_CACHE_TTL)
            .then(|| Arc::clone(&cached.entries))
    }

    pub(super) fn insert(
        &self,
        key: ScopeKey,
        revisions: Vec<DocumentRevision>,
        entries: Arc<[LorebookEntry]>,
    ) {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(
                key,
                CachedCorpus {
                    revisions,
                    entries,
                    cached_at: Instant::now(),
                },
            );
    }

    pub(super) fn invalidate(&self, key: &ScopeKey) {
        self.scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop(key);
    }
}

pub(super) static CACHE: std::sync::LazyLock<LorebookCache> =
    std::sync::LazyLock::new(|| LorebookCache::new(CACHE_SCOPES));

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(path: &str, updated_at: DateTime<Utc>) -> DocumentRevision {
        DocumentRevision {
            path: path.to_string(),
            updated_at: Some(updated_at),
        }
    }

    #[test]
    fn cache_hits_only_matching_document_versions() {
        let cache = LorebookCache::new(2);
        let key = ScopeKey::new("primary", None);
        let first = revision("lorebooks/a.json", Utc::now());
        cache.insert(key.clone(), vec![first.clone()], Arc::from([]));

        assert!(cache.get(&key, std::slice::from_ref(&first)).is_some());
        assert!(
            cache
                .get(
                    &key,
                    &[revision(
                        "lorebooks/a.json",
                        first.updated_at.expect("revision timestamp")
                            + chrono::Duration::seconds(1),
                    )],
                )
                .is_none()
        );
    }

    #[test]
    fn empty_results_are_negative_cached_and_invalidated() {
        let cache = LorebookCache::new(2);
        let key = ScopeKey::new("primary", None);
        cache.insert(key.clone(), Vec::new(), Arc::from([]));
        assert!(cache.get_recent_empty(&key).is_some());

        cache.invalidate(&key);
        assert!(cache.get_recent_empty(&key).is_none());
    }

    #[test]
    fn scope_cache_is_bounded_and_isolated_by_agent() {
        let cache = LorebookCache::new(2);
        let first = ScopeKey::new("primary", Some(Uuid::new_v4()));
        let second = ScopeKey::new("primary", Some(Uuid::new_v4()));
        let third = ScopeKey::new("other", None);
        for key in [first.clone(), second.clone(), third.clone()] {
            cache.insert(key, Vec::new(), Arc::from([]));
        }

        assert!(cache.get_recent_empty(&first).is_none());
        assert!(cache.get_recent_empty(&second).is_some());
        assert!(cache.get_recent_empty(&third).is_some());
    }
}
