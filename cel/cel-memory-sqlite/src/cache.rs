//! Small TTL + LRU cache for [`crate::SqliteMemoryProvider::retrieve`].
//!
//! Hybrid retrieval is *expensive*: an embedding pass (CPU-bound, sub-ms
//! for `MockEmbedder` but ~10–30 ms for `fastembed` on first call) plus
//! three SQLite sub-queries (sqlite-vec k-NN, FTS5 BM25, recency scan)
//! followed by an in-memory RRF merge. The embedded agent and the NL
//! compiler both call `retrieve` repeatedly with near-identical queries
//! inside a single user turn — agent re-ranking, compiler re-prompting,
//! UI typeahead — and pay that cost again every time.
//!
//! This cache short-circuits the second-and-later calls within a short
//! window. It is intentionally tiny (no `lru` crate dependency) and
//! conservative:
//!
//! - **Capacity:** 256 entries. A heavy session is ~30 retrievals/min;
//!   256 buys us ~8 minutes of distinct queries before LRU eviction.
//! - **TTL:** 30 seconds. Long enough to absorb a user turn cluster,
//!   short enough that a write 30 s ago will be visible to the next
//!   matching read even if the eager clear is missed.
//! - **Invalidation:** every mutator (`write`, `write_batch`, `delete`,
//!   `delete_matching`, `update_importance`, `supersede`,
//!   `run_aging_sweep`, `purge_all`, plus `pin`) calls
//!   [`RetrieveCache::clear`]. The TTL is a backstop only — if a mutator
//!   forgets to invalidate, stale reads cap at 30 s.
//!
//! See `cellar-memory-manager.md` §8 for the retrieve hot path discussion.
//!
//! The cache is generic over the value type so tests don't need real
//! `MemoryChunk` values.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// FIFO + TTL bounded cache. Each `get` promotes the key to the back of
/// the eviction queue (so frequently-read keys outlive cold ones) — the
/// LRU half of the name. The TTL ensures a write that misses an explicit
/// invalidation can't return data older than `ttl`.
pub(crate) struct RetrieveCache<V: Clone> {
    inner: Mutex<Inner<V>>,
    capacity: usize,
    ttl: Duration,
}

struct Inner<V> {
    map: HashMap<u64, (Instant, V)>,
    /// Least-recently-used at the front; most-recently-used at the back.
    order: VecDeque<u64>,
}

impl<V: Clone> RetrieveCache<V> {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            capacity,
            ttl,
        }
    }

    /// Lookup. Returns `None` if missing, expired, or the lock is poisoned
    /// (a poisoned mutex should never happen for this in-memory cache, but
    /// we degrade gracefully rather than panic on a hot path).
    pub fn get(&self, key: u64) -> Option<V> {
        let mut g = self.inner.lock().ok()?;
        let now = Instant::now();
        let entry = g.map.get(&key).cloned();
        match entry {
            Some((inserted, v)) if now.duration_since(inserted) <= self.ttl => {
                // Promote to MRU position.
                if let Some(pos) = g.order.iter().position(|k| *k == key) {
                    g.order.remove(pos);
                }
                g.order.push_back(key);
                Some(v)
            }
            Some(_) => {
                // Expired — drop on the way out so the next reader doesn't
                // pay the TTL check again.
                g.map.remove(&key);
                if let Some(pos) = g.order.iter().position(|k| *k == key) {
                    g.order.remove(pos);
                }
                None
            }
            None => None,
        }
    }

    /// Insert or refresh. Evicts the LRU entry if at capacity.
    pub fn insert(&self, key: u64, value: V) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        let now = Instant::now();
        // If already present: refresh value + bump to MRU position.
        if let std::collections::hash_map::Entry::Occupied(mut e) = g.map.entry(key) {
            e.insert((now, value));
            if let Some(pos) = g.order.iter().position(|k| *k == key) {
                g.order.remove(pos);
            }
            g.order.push_back(key);
            return;
        }
        while g.order.len() >= self.capacity {
            match g.order.pop_front() {
                Some(k) => {
                    g.map.remove(&k);
                }
                None => break,
            }
        }
        g.map.insert(key, (now, value));
        g.order.push_back(key);
    }

    /// Drop everything. Called by every mutator on the provider.
    pub fn clear(&self) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        g.map.clear();
        g.order.clear();
    }

    /// Test-only: current entry count.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.map.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn hit_then_miss_after_clear() {
        let c: RetrieveCache<i32> = RetrieveCache::new(8, Duration::from_secs(30));
        c.insert(1, 100);
        assert_eq!(c.get(1), Some(100));
        c.clear();
        assert_eq!(c.get(1), None);
    }

    #[test]
    fn expired_entry_drops() {
        let c: RetrieveCache<i32> = RetrieveCache::new(8, Duration::from_millis(20));
        c.insert(1, 100);
        sleep(Duration::from_millis(40));
        assert_eq!(c.get(1), None);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn lru_eviction_at_capacity() {
        let c: RetrieveCache<i32> = RetrieveCache::new(2, Duration::from_secs(30));
        c.insert(1, 1);
        c.insert(2, 2);
        // Promote 1 to MRU.
        assert_eq!(c.get(1), Some(1));
        // Now inserting 3 should evict 2 (the LRU).
        c.insert(3, 3);
        assert_eq!(c.get(2), None);
        assert_eq!(c.get(1), Some(1));
        assert_eq!(c.get(3), Some(3));
    }

    #[test]
    fn refresh_does_not_grow() {
        let c: RetrieveCache<i32> = RetrieveCache::new(8, Duration::from_secs(30));
        c.insert(1, 100);
        c.insert(1, 101);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(1), Some(101));
    }
}
