//! FSEvents adapter — the second ambient event source.
//!
//! Watches one or more root directories (typically `~`) for filesystem
//! changes and publishes [`EventKind::FileCreated`] / [`EventKind::FileModified`]
//! / [`EventKind::FileDeleted`] events onto the [`EventBus`]. Cross-platform
//! via the `notify` crate (macOS FSEvents, Linux inotify, Windows
//! ReadDirectoryChangesW).
//!
//! Path filtering is done here, before publishing — paths matching any
//! configured `ignore_prefix` are dropped. The matcher consumer task
//! shouldn't have to know about Library/Trash/cache noise.
//!
//! ## `size_bytes` semantics (read carefully)
//!
//! When `emit_size_metadata` is on:
//! - `file_created` / `file_modified` events include `data.size_bytes` from
//!   a `stat()` call **at event time**. The size reflects the file as it
//!   exists when the event arrives, not when the change happened.
//! - `file_deleted` events include `data.size_bytes` **only if** the path
//!   was recently observed in a create or modify event — see
//!   [`RecentSizes`]. By the time the delete callback fires the file is
//!   gone, so we can't stat() it; the cache is populated on create/modify
//!   and consulted (and emptied) on delete. Capacity is bounded so the
//!   cache can't grow without bound on long-running daemons. Rules that
//!   want to filter deletions by size (e.g., Scenario 2 in
//!   `cellar-app-v1.md` §5) now work for files modified within the cache
//!   window before their deletion.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cellar_types::{Event, EventKind, EventSource};
use notify::{EventKind as NotifyKind, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::bus::EventBus;

/// FSEvents adapter configuration.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Roots to watch (recursive). Each is registered as a separate watch.
    pub watched_roots: Vec<PathBuf>,
    /// Paths matching any of these prefixes are dropped at publish time.
    pub ignore_prefixes: Vec<PathBuf>,
    /// When `true`, `file_created` and `file_modified` events include
    /// `data.size_bytes` from a `stat()` at event time. `file_deleted`
    /// events include `data.size_bytes` only if the path is in the
    /// [`RecentSizes`] cache (populated by prior create/modify events).
    pub emit_size_metadata: bool,
    /// Capacity for the [`RecentSizes`] cache that populates `size_bytes`
    /// on `file_deleted` events. Each entry is path + size + age tick
    /// (roughly 80–120 bytes). Default 10,000 ≈ ~1 MB worst case.
    pub recent_sizes_capacity: usize,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self {
            watched_roots: vec![home.clone()],
            ignore_prefixes: vec![
                home.join("Library"),
                home.join(".Trash"),
                home.join(".cache"),
                home.join(".cellar"),
            ],
            emit_size_metadata: true,
            recent_sizes_capacity: 10_000,
        }
    }
}

/// Bounded LRU cache of recently observed file sizes.
///
/// Populated by `file_created` / `file_modified` events when
/// `emit_size_metadata` is on; drained by `file_deleted` events so the
/// resulting cellar event can include `size_bytes` even though stat()
/// would fail by the time the delete callback fires.
///
/// Eviction is true LRU: insertions and lookups stamp the entry with a
/// monotonically increasing tick; the lowest-tick entry is evicted when
/// the cache reaches `capacity`. Take is destructive — once a delete
/// consumes the cached size, the entry is gone (we won't see the same
/// path deleted twice without an intervening create/modify, and if we
/// do, the second delete returns no size, which is correct behaviour).
///
/// **Concurrency**: the spawned consumer task is the sole writer/reader,
/// but the cache is wrapped in `Mutex` so [`translate`] (called from
/// tests, possibly other consumers) can take `&Self` rather than
/// `&mut Self`. Contention is irrelevant — one task, one lock.
#[derive(Debug)]
pub struct RecentSizes {
    inner: Mutex<RecentSizesInner>,
}

#[derive(Debug)]
struct RecentSizesInner {
    /// `path → (size, tick)`. The tick lets us update the LRU order on
    /// re-insert by removing the old `order` entry.
    map: HashMap<PathBuf, (u64, u64)>,
    /// `tick → path`. BTreeMap so `iter().next()` is the LRU entry.
    order: BTreeMap<u64, PathBuf>,
    /// Monotonic insertion counter. Overflow at u64::MAX is unreachable
    /// for any realistic daemon lifetime (~292 billion years at 1 GHz).
    next_tick: u64,
    /// Maximum number of cached entries.
    capacity: usize,
}

impl RecentSizes {
    /// Construct an empty cache with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(RecentSizesInner {
                map: HashMap::new(),
                order: BTreeMap::new(),
                next_tick: 0,
                capacity: capacity.max(1),
            }),
        }
    }

    /// Cache the given size for `path`. If the path is already cached,
    /// the entry is refreshed (moved to MRU position). If the cache is at
    /// capacity, the least-recently-used entry is evicted to make room.
    pub fn insert(&self, path: PathBuf, size: u64) {
        let mut g = self.inner.lock().expect("RecentSizes mutex poisoned");
        // Refresh existing entry: drop its old order row first.
        if let Some(&(_, old_tick)) = g.map.get(&path) {
            g.order.remove(&old_tick);
        } else if g.map.len() >= g.capacity {
            // New key, at capacity → evict the lowest-tick entry.
            if let Some((&lru_tick, _)) = g.order.iter().next() {
                if let Some(lru_path) = g.order.remove(&lru_tick) {
                    g.map.remove(&lru_path);
                }
            }
        }
        let tick = g.next_tick;
        g.next_tick = g.next_tick.wrapping_add(1);
        g.map.insert(path.clone(), (size, tick));
        g.order.insert(tick, path);
    }

    /// Look up and remove a path's size. Returns `Some(size)` if the
    /// path was tracked, `None` otherwise. Take semantics: the cache
    /// entry is removed so a second delete for the same path returns
    /// `None` (correct — the file is already gone).
    pub fn take(&self, path: &Path) -> Option<u64> {
        let mut g = self.inner.lock().expect("RecentSizes mutex poisoned");
        if let Some((size, tick)) = g.map.remove(path) {
            g.order.remove(&tick);
            Some(size)
        } else {
            None
        }
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.map.len()).unwrap_or(0)
    }

    /// Returns true iff there are no cached entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for RecentSizes {
    fn default() -> Self {
        Self::with_capacity(10_000)
    }
}

/// FSEvents adapter error type. Most failures bubble up from `notify`.
#[derive(Debug, Error)]
pub enum FsEventsError {
    /// `notify` failed to construct a watcher or register a watch.
    #[error("notify watcher error: {0}")]
    Watcher(#[from] notify::Error),
}

/// Spawn the FSEvents adapter as a regular tokio task.
///
/// **Why not `spawn_blocking`:** `notify`'s recommended watcher delivers
/// events via a callback on its own OS thread, so we need a bridge into
/// tokio. The obvious approach — `spawn_blocking` + `std::sync::mpsc::recv`
/// — has a fatal flaw: blocking tasks cannot be cancelled (`JoinHandle::abort`
/// is a no-op for them), and the runtime *waits* for them on shutdown. That
/// causes test runtimes to hang.
///
/// Instead we use `tokio::sync::mpsc` and call `blocking_send` from the
/// notify callback (safe — the callback thread is outside tokio). The
/// consumer is a regular `tokio::spawn` task that `await`s on `rx.recv`;
/// it's fully cancellable and the runtime can drop it on shutdown.
///
/// The watcher is moved into the task so its lifetime matches the task —
/// when the task is dropped, the watcher's `Drop` impl unregisters every
/// watch and the notify callback thread shuts down on its own.
///
/// Returns an error immediately if the watcher can't be constructed or if
/// any of the configured roots can't be watched.
pub fn spawn(bus: &EventBus, cfg: AdapterConfig) -> Result<JoinHandle<()>, FsEventsError> {
    // Validate up front — warn but don't fail; the daemon should still
    // start even if one configured root is missing.
    for root in &cfg.watched_roots {
        if !root.exists() {
            tracing::warn!(
                path = %root.display(),
                "fsevents watched_root does not exist; skipping"
            );
        }
    }

    // Bridge: notify callback (OS thread) → tokio async consumer task.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<notify::Event>>(256);

    let mut watcher = notify::recommended_watcher(move |res| {
        // `blocking_send` is safe here: notify invokes this callback from
        // its own OS thread, not from inside the tokio runtime. If the
        // receiver has been dropped (task cancelled), the send errors —
        // ignore and the callback thread will exit on next watcher drop.
        let _ = tx.blocking_send(res);
    })?;

    for root in &cfg.watched_roots {
        if root.exists() {
            watcher.watch(root, RecursiveMode::Recursive)?;
            tracing::info!(
                path = %root.display(),
                "fsevents watching recursively"
            );
        }
    }

    tracing::info!(
        roots = cfg.watched_roots.len(),
        ignores = cfg.ignore_prefixes.len(),
        emit_size = cfg.emit_size_metadata,
        "fsevents adapter started"
    );

    let bus = bus.clone();
    let cache = RecentSizes::with_capacity(cfg.recent_sizes_capacity);
    let handle = tokio::spawn(async move {
        // Move the watcher into the task so dropping the task drops the
        // watcher (and unregisters every watch).
        let _watcher = watcher;
        while let Some(result) = rx.recv().await {
            match result {
                Ok(notify_event) => {
                    for cellar_event in translate_with_cache(&notify_event, &cfg, &cache) {
                        bus.publish(cellar_event);
                    }
                }
                Err(err) => {
                    // notify itself errored on a specific event (e.g., a single
                    // path couldn't be stat'd). Log and keep watching.
                    tracing::warn!(error = %err, "fsevents notify error; continuing");
                }
            }
        }
        tracing::info!("fsevents adapter event channel closed; exiting");
    });

    Ok(handle)
}

/// Translate one `notify::Event` into zero or more cellar events.
///
/// Paths matching any `cfg.ignore_prefixes` are silently dropped. Each
/// path in the notify event becomes its own cellar event, so a notify
/// event with `paths = [a, b]` (rare — e.g., a rename) emits two cellar
/// events.
///
/// Backward-compatible wrapper that constructs a throwaway
/// [`RecentSizes`] cache per call; tests for the create/modify size
/// surface use this. Delete events from this entry point will not carry
/// `size_bytes` because the cache has no prior observations. The spawned
/// adapter uses [`translate_with_cache`] with a persistent cache so
/// deletes do get size info.
pub fn translate(notify_event: &notify::Event, cfg: &AdapterConfig) -> Vec<Event> {
    let cache = RecentSizes::with_capacity(1);
    translate_with_cache(notify_event, cfg, &cache)
}

/// Translate one `notify::Event` into zero or more cellar events,
/// consulting and updating the supplied [`RecentSizes`] cache so that
/// `file_deleted` events carry `size_bytes` when the path was observed
/// in a recent create or modify event.
///
/// Effects on `cache` (only when `cfg.emit_size_metadata` is on):
/// - `FileCreated` / `FileModified` → stat() the path; on success,
///   insert `(path, size)` into the cache (LRU-evicting if at capacity).
/// - `FileDeleted` → take the cached size (if any) and attach it as
///   `data.size_bytes`. Take is destructive — the entry is removed.
///
/// Pure-ish: only touches the filesystem when `emit_size_metadata` is on
/// and the event kind is Created/Modified.
pub fn translate_with_cache(
    notify_event: &notify::Event,
    cfg: &AdapterConfig,
    cache: &RecentSizes,
) -> Vec<Event> {
    let kind = match map_kind(&notify_event.kind) {
        Some(k) => k,
        None => return Vec::new(), // Access, Other, etc. — not interesting
    };

    let mut out = Vec::new();
    for path in &notify_event.paths {
        if is_ignored(path, &cfg.ignore_prefixes) {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        let mut event = Event::now(EventSource::Fsevents, kind.clone()).with_data("path", path_str);

        if cfg.emit_size_metadata {
            match kind {
                EventKind::FileCreated | EventKind::FileModified => {
                    if let Ok(meta) = std::fs::metadata(path) {
                        let size = meta.len();
                        event = event.with_data("size_bytes", size);
                        cache.insert(path.clone(), size);
                    }
                }
                EventKind::FileDeleted => {
                    if let Some(size) = cache.take(path) {
                        event = event.with_data("size_bytes", size);
                    }
                }
                _ => {}
            }
        }

        out.push(event);
    }
    out
}

fn map_kind(notify_kind: &NotifyKind) -> Option<EventKind> {
    match notify_kind {
        NotifyKind::Create(_) => Some(EventKind::FileCreated),
        NotifyKind::Modify(_) => Some(EventKind::FileModified),
        NotifyKind::Remove(_) => Some(EventKind::FileDeleted),
        _ => None,
    }
}

fn is_ignored(path: &Path, prefixes: &[PathBuf]) -> bool {
    prefixes.iter().any(|prefix| path.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind, RemoveKind};

    fn cfg(roots: Vec<&str>, ignores: Vec<&str>) -> AdapterConfig {
        AdapterConfig {
            watched_roots: roots.into_iter().map(PathBuf::from).collect(),
            ignore_prefixes: ignores.into_iter().map(PathBuf::from).collect(),
            emit_size_metadata: false, // pure-path tests: don't touch fs
            recent_sizes_capacity: 10,
        }
    }

    fn notify_event(kind: NotifyKind, paths: Vec<&str>) -> notify::Event {
        notify::Event {
            kind,
            paths: paths.into_iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn translate_create_to_file_created() {
        let cfg = cfg(vec!["/Users/x"], vec![]);
        let events = translate(
            &notify_event(NotifyKind::Create(CreateKind::File), vec!["/Users/x/a.txt"]),
            &cfg,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::FileCreated);
        assert_eq!(events[0].source, EventSource::Fsevents);
        assert_eq!(events[0].data["path"], "/Users/x/a.txt");
        // emit_size_metadata is false → no size_bytes field
        assert!(!events[0].data.contains_key("size_bytes"));
    }

    #[test]
    fn translate_remove_to_file_deleted() {
        let cfg = cfg(vec!["/Users/x"], vec![]);
        let events = translate(
            &notify_event(
                NotifyKind::Remove(RemoveKind::File),
                vec!["/Users/x/gone.txt"],
            ),
            &cfg,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::FileDeleted);
        assert_eq!(events[0].data["path"], "/Users/x/gone.txt");
    }

    #[test]
    fn translate_modify_to_file_modified() {
        let cfg = cfg(vec!["/Users/x"], vec![]);
        let events = translate(
            &notify_event(
                NotifyKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
                vec!["/Users/x/b.txt"],
            ),
            &cfg,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::FileModified);
    }

    #[test]
    fn translate_ignored_prefix_drops_event() {
        let cfg = cfg(
            vec!["/Users/x"],
            vec!["/Users/x/Library", "/Users/x/.cache"],
        );
        let cached = translate(
            &notify_event(
                NotifyKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
                vec!["/Users/x/.cache/foo/bar"],
            ),
            &cfg,
        );
        assert!(cached.is_empty(), "events under ignored prefixes must drop");

        let lib = translate(
            &notify_event(
                NotifyKind::Create(CreateKind::File),
                vec!["/Users/x/Library/Preferences/y.plist"],
            ),
            &cfg,
        );
        assert!(lib.is_empty());
    }

    #[test]
    fn translate_unknown_kind_emits_nothing() {
        let cfg = cfg(vec!["/Users/x"], vec![]);
        let events = translate(
            &notify_event(
                NotifyKind::Access(notify::event::AccessKind::Read),
                vec!["/Users/x/a.txt"],
            ),
            &cfg,
        );
        assert!(events.is_empty());

        let other = translate(
            &notify_event(NotifyKind::Other, vec!["/Users/x/a.txt"]),
            &cfg,
        );
        assert!(other.is_empty());

        let any = translate(&notify_event(NotifyKind::Any, vec!["/Users/x/a.txt"]), &cfg);
        assert!(any.is_empty());
    }

    #[test]
    fn translate_multi_path_event_emits_per_path() {
        let cfg = cfg(vec!["/Users/x"], vec!["/Users/x/.cellar"]);
        let events = translate(
            &notify_event(
                NotifyKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)),
                vec![
                    "/Users/x/a.txt",
                    "/Users/x/b.txt",
                    "/Users/x/.cellar/state.db",
                ],
            ),
            &cfg,
        );
        // Three input paths, one ignored → two events
        assert_eq!(events.len(), 2);
        let paths: Vec<String> = events
            .iter()
            .map(|e| e.data["path"].as_str().unwrap().to_string())
            .collect();
        assert!(paths.contains(&"/Users/x/a.txt".to_string()));
        assert!(paths.contains(&"/Users/x/b.txt".to_string()));
        // .cellar path was filtered out
        assert!(!paths.iter().any(|p| p.contains(".cellar")));
    }

    #[test]
    fn translate_size_metadata_when_file_exists() {
        // Create a real tempfile, run translate with emit_size_metadata=true,
        // and confirm the size field appears.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sized.bin");
        std::fs::write(&path, b"hello world").unwrap();

        let cfg = AdapterConfig {
            watched_roots: vec![dir.path().to_path_buf()],
            ignore_prefixes: vec![],
            emit_size_metadata: true,
            recent_sizes_capacity: 10,
        };

        let events = translate(
            &notify_event(
                NotifyKind::Create(CreateKind::File),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["size_bytes"], 11);
    }

    #[test]
    fn translate_size_metadata_absent_on_delete_without_cache_hit() {
        // Without a prior create/modify observation in the cache, delete
        // events have no size_bytes. (The bare `translate` helper uses a
        // throwaway cache, so this exercises the cache-miss path.)
        let cfg = AdapterConfig {
            watched_roots: vec!["/tmp".into()],
            ignore_prefixes: vec![],
            emit_size_metadata: true,
            recent_sizes_capacity: 10,
        };
        let events = translate(
            &notify_event(
                NotifyKind::Remove(RemoveKind::File),
                vec!["/tmp/whatever.bin"],
            ),
            &cfg,
        );
        assert_eq!(events.len(), 1);
        assert!(
            !events[0].data.contains_key("size_bytes"),
            "delete events with no prior cache observation must not include size_bytes"
        );
    }

    // ───── RecentSizes cache unit tests ─────

    #[test]
    fn recent_sizes_take_after_insert_returns_size() {
        let c = RecentSizes::with_capacity(10);
        c.insert(PathBuf::from("/a"), 100);
        assert_eq!(c.take(Path::new("/a")), Some(100));
        // Take is destructive — second take returns None.
        assert_eq!(c.take(Path::new("/a")), None);
    }

    #[test]
    fn recent_sizes_take_missing_returns_none() {
        let c = RecentSizes::with_capacity(10);
        assert_eq!(c.take(Path::new("/never_inserted")), None);
    }

    #[test]
    fn recent_sizes_evicts_lru_at_capacity() {
        let c = RecentSizes::with_capacity(2);
        c.insert(PathBuf::from("/a"), 1);
        c.insert(PathBuf::from("/b"), 2);
        // Inserting /c with cap=2 must evict /a (LRU).
        c.insert(PathBuf::from("/c"), 3);
        assert_eq!(c.len(), 2);
        assert_eq!(c.take(Path::new("/a")), None, "LRU /a should be evicted");
        assert_eq!(c.take(Path::new("/b")), Some(2));
        assert_eq!(c.take(Path::new("/c")), Some(3));
    }

    #[test]
    fn recent_sizes_reinsert_refreshes_lru_order() {
        // Insert /a, then /b; touch /a again so /b becomes LRU.
        let c = RecentSizes::with_capacity(2);
        c.insert(PathBuf::from("/a"), 1);
        c.insert(PathBuf::from("/b"), 2);
        c.insert(PathBuf::from("/a"), 11); // refreshes /a
                                           // Inserting /c evicts /b (now LRU), not /a.
        c.insert(PathBuf::from("/c"), 3);
        assert_eq!(c.take(Path::new("/b")), None, "/b should be evicted");
        assert_eq!(c.take(Path::new("/a")), Some(11));
        assert_eq!(c.take(Path::new("/c")), Some(3));
    }

    #[test]
    fn recent_sizes_reinsert_updates_size_value() {
        let c = RecentSizes::with_capacity(2);
        c.insert(PathBuf::from("/a"), 1);
        c.insert(PathBuf::from("/a"), 99); // overwrite same path
        assert_eq!(c.len(), 1);
        assert_eq!(c.take(Path::new("/a")), Some(99));
    }

    #[test]
    fn recent_sizes_capacity_zero_clamps_to_one() {
        // capacity=0 would be useless; we clamp to 1 so the cache always
        // holds at least one entry. (Callers shouldn't pass 0 but we
        // guard against it.)
        let c = RecentSizes::with_capacity(0);
        c.insert(PathBuf::from("/a"), 1);
        assert_eq!(c.len(), 1);
    }

    // ───── translate_with_cache integration ─────

    #[test]
    fn translate_with_cache_delete_includes_size_from_prior_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doomed.bin");
        std::fs::write(&path, b"twelve bytes").unwrap(); // 12 bytes

        let cfg = AdapterConfig {
            watched_roots: vec![dir.path().to_path_buf()],
            ignore_prefixes: vec![],
            emit_size_metadata: true,
            recent_sizes_capacity: 10,
        };
        let cache = RecentSizes::with_capacity(10);

        // 1. Create event populates the cache.
        let create_events = translate_with_cache(
            &notify_event(
                NotifyKind::Create(CreateKind::File),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
            &cache,
        );
        assert_eq!(create_events[0].data["size_bytes"], 12);
        assert_eq!(cache.len(), 1);

        // 2. Now delete the file on disk, then translate the delete event.
        //    The cache should serve the size even though stat() would fail.
        std::fs::remove_file(&path).unwrap();
        let delete_events = translate_with_cache(
            &notify_event(
                NotifyKind::Remove(RemoveKind::File),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
            &cache,
        );
        assert_eq!(delete_events.len(), 1);
        assert_eq!(delete_events[0].kind, EventKind::FileDeleted);
        assert_eq!(
            delete_events[0].data["size_bytes"], 12,
            "delete event must carry size from cache populated by prior create"
        );
        // Cache was drained by the take.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn translate_with_cache_delete_omits_size_when_not_observed() {
        // The cache has nothing for this path → delete event has no size.
        let cfg = AdapterConfig {
            watched_roots: vec!["/tmp".into()],
            ignore_prefixes: vec![],
            emit_size_metadata: true,
            recent_sizes_capacity: 10,
        };
        let cache = RecentSizes::with_capacity(10);
        let events = translate_with_cache(
            &notify_event(
                NotifyKind::Remove(RemoveKind::File),
                vec!["/tmp/never_seen.bin"],
            ),
            &cfg,
            &cache,
        );
        assert_eq!(events.len(), 1);
        assert!(!events[0].data.contains_key("size_bytes"));
    }

    #[test]
    fn translate_with_cache_modify_then_delete_picks_up_latest_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.bin");
        let cfg = AdapterConfig {
            watched_roots: vec![dir.path().to_path_buf()],
            ignore_prefixes: vec![],
            emit_size_metadata: true,
            recent_sizes_capacity: 10,
        };
        let cache = RecentSizes::with_capacity(10);

        // Create at 3 bytes.
        std::fs::write(&path, b"abc").unwrap();
        translate_with_cache(
            &notify_event(
                NotifyKind::Create(CreateKind::File),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
            &cache,
        );

        // Modify to 7 bytes.
        std::fs::write(&path, b"abcdefg").unwrap();
        translate_with_cache(
            &notify_event(
                NotifyKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
            &cache,
        );

        // Delete — cache should report the modified size, not the created size.
        std::fs::remove_file(&path).unwrap();
        let delete_events = translate_with_cache(
            &notify_event(
                NotifyKind::Remove(RemoveKind::File),
                vec![path.to_str().unwrap()],
            ),
            &cfg,
            &cache,
        );
        assert_eq!(delete_events[0].data["size_bytes"], 7);
    }

    #[test]
    fn translate_with_cache_size_disabled_does_not_touch_cache() {
        // emit_size_metadata=false means the cache is never read or written.
        let cfg = AdapterConfig {
            watched_roots: vec!["/tmp".into()],
            ignore_prefixes: vec![],
            emit_size_metadata: false,
            recent_sizes_capacity: 10,
        };
        let cache = RecentSizes::with_capacity(10);
        // Pre-seed the cache to verify nothing reads from it either.
        cache.insert(PathBuf::from("/tmp/x.bin"), 999);
        let events = translate_with_cache(
            &notify_event(NotifyKind::Remove(RemoveKind::File), vec!["/tmp/x.bin"]),
            &cfg,
            &cache,
        );
        assert_eq!(events.len(), 1);
        // size_bytes must NOT appear because emit_size_metadata=false
        // short-circuits before consulting the cache.
        assert!(!events[0].data.contains_key("size_bytes"));
        // Cache is untouched.
        assert_eq!(cache.len(), 1);
    }

    /// End-to-end smoke test: spawn the adapter against a tempdir, create
    /// and delete a file, observe at least one create-or-modify event for
    /// the create and at least one delete event. The test is generous about
    /// duplicate or extra events — most platforms (especially macOS
    /// FSEvents) emit multiple events for one logical fs operation.
    ///
    /// `#[ignore]`: this test depends on OS-level event delivery (macOS
    /// FSEvents specifically). FSEvents has unpredictable latency for
    /// newly-registered watches on system temp directories, which makes
    /// this flaky in unattended runs. Run manually with
    /// `cargo test -p cel-cortex-daemon -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn spawn_adapter_observes_real_filesystem_change() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::with_capacity(256);
        let mut rx = bus.subscribe();

        let handle = spawn(
            &bus,
            AdapterConfig {
                watched_roots: vec![dir.path().to_path_buf()],
                ignore_prefixes: vec![],
                emit_size_metadata: true,
                recent_sizes_capacity: 10,
            },
        )
        .expect("watcher should start");

        // Give the watcher a generous moment to register the recursive
        // watch. macOS FSEvents in particular can be slow to start
        // delivering events for paths that were just `watch()`-ed.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let target = dir.path().join("smoke.bin");
        std::fs::write(&target, b"xyz").unwrap();

        let saw_create = await_event(&mut rx, Duration::from_secs(5), |e| {
            (e.kind == EventKind::FileCreated || e.kind == EventKind::FileModified)
                && e.data["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("smoke.bin"))
        })
        .await;

        std::fs::remove_file(&target).unwrap();
        let saw_delete = await_event(&mut rx, Duration::from_secs(5), |e| {
            e.kind == EventKind::FileDeleted
                && e.data["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("smoke.bin"))
        })
        .await;

        // Cancel the task BEFORE asserting so the runtime can tear down
        // cleanly even if an assertion fails. `tokio::spawn` tasks honor
        // `abort`; the watcher gets dropped, which unregisters the OS
        // watch and lets notify's callback thread exit on its own.
        handle.abort();
        let _ = handle.await; // wait for the JoinError; ignore it.

        assert!(
            saw_create,
            "expected a create-or-modify event for smoke.bin within 5s"
        );
        assert!(
            saw_delete,
            "expected a delete event for smoke.bin within 5s"
        );
    }

    async fn await_event<F>(
        rx: &mut tokio::sync::broadcast::Receiver<Event>,
        deadline: std::time::Duration,
        mut pred: F,
    ) -> bool
    where
        F: FnMut(&Event) -> bool,
    {
        let until = tokio::time::Instant::now() + deadline;
        loop {
            let remaining = until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) => {
                    if pred(&ev) {
                        return true;
                    }
                }
                Ok(Err(_)) => return false, // channel closed/lagged
                Err(_) => return false,
            }
        }
    }
}
