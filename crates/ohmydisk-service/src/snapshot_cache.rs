//! Immutable snapshot/index pairs with a small LRU. A single oversized newest
//! snapshot is retained rather than decoding that same large tree per request.
use ohmydisk_core::{
    AppError, Node, Query, QueryIndex, ScanIssue, SizeMetric, Snapshot, query_indexed,
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    ops::Deref,
    sync::{Arc, Condvar, Mutex},
};

pub(crate) struct SnapshotView {
    snapshot: Snapshot,
    index: QueryIndex,
    estimated_bytes: usize,
}
impl SnapshotView {
    fn new(snapshot: Snapshot) -> Result<Self, AppError> {
        let index = QueryIndex::new(&snapshot)?;
        if snapshot.nodes.iter().any(|node| node.space_usage.is_some()) {
            index.prewarm_metric(&snapshot, SizeMetric::Attributed);
        }
        let mut bytes = size_of::<Self>()
            .saturating_add(snapshot.nodes.capacity().saturating_mul(size_of::<Node>()))
            .saturating_add(
                snapshot
                    .issues
                    .capacity()
                    .saturating_mul(size_of::<ScanIssue>()),
            )
            .saturating_add(index.estimated_max_heap_bytes());
        for node in &snapshot.nodes {
            let strings = [
                node.id.capacity(),
                node.parent_id.as_ref().map_or(0, String::capacity),
                node.name.capacity(),
                node.path.display.capacity(),
                node.path.encoded.capacity(),
                node.identity.capacity(),
                node.volume_id.capacity(),
            ];
            for size in strings {
                bytes = bytes.saturating_add(size);
            }
        }
        for issue in &snapshot.issues {
            for size in [
                issue.node_id.as_ref().map_or(0, String::capacity),
                issue.path.capacity(),
                issue.code.capacity(),
                issue.message.capacity(),
            ] {
                bytes = bytes.saturating_add(size);
            }
        }
        for size in [
            snapshot.id.capacity(),
            snapshot.scan_id.capacity(),
            snapshot.root_id.capacity(),
            snapshot.root_path.display.capacity(),
            snapshot.root_path.encoded.capacity(),
        ] {
            bytes = bytes.saturating_add(size);
        }
        if let Some(accounting) = &snapshot.space_accounting {
            bytes = bytes
                .saturating_add(accounting.reconciliation_status.capacity())
                .saturating_add(accounting.measurement_status.capacity());
            for observation in std::iter::once(&accounting.before).chain(accounting.after.iter()) {
                bytes = bytes
                    .saturating_add(observation.volume_id.capacity())
                    .saturating_add(observation.source.capacity())
                    .saturating_add(observation.capacity_scope.capacity());
            }
        }
        Ok(Self {
            snapshot,
            index,
            estimated_bytes: bytes,
        })
    }
    pub(crate) fn query(&self, query: &Query) -> Result<Value, AppError> {
        query_indexed(&self.snapshot, &self.index, query)
    }
}
impl Deref for SnapshotView {
    type Target = Snapshot;
    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

struct Flight {
    result: Mutex<Option<Result<Arc<SnapshotView>, AppError>>>,
    ready: Condvar,
}
impl Flight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
    fn complete(&self, result: Result<Arc<SnapshotView>, AppError>) {
        if let Ok(mut saved) = self.result.lock() {
            if saved.is_none() {
                *saved = Some(result);
            }
        }
        self.ready.notify_all();
    }
    fn wait(&self) -> Result<Arc<SnapshotView>, AppError> {
        let mut result = self.result.lock().map_err(cache_error)?;
        while result.is_none() {
            result = self.ready.wait(result).map_err(cache_error)?;
        }
        result.as_ref().expect("Result is ready").clone()
    }
}
struct State {
    entries: VecDeque<Arc<SnapshotView>>,
    bytes: usize,
    flights: HashMap<String, Arc<Flight>>,
}
pub(crate) struct SnapshotCache {
    state: Mutex<State>,
    budget: usize,
}
impl Default for SnapshotCache {
    fn default() -> Self {
        Self::new(128 * 1024 * 1024)
    }
}
impl SnapshotCache {
    fn new(budget: usize) -> Self {
        Self {
            state: Mutex::new(State {
                entries: VecDeque::new(),
                bytes: 0,
                flights: HashMap::new(),
            }),
            budget,
        }
    }
    pub(crate) fn insert(&self, snapshot: Snapshot) -> Result<Arc<SnapshotView>, AppError> {
        let view = Arc::new(SnapshotView::new(snapshot)?);
        let mut state = self.state.lock().map_err(cache_error)?;
        self.insert_locked(&mut state, view.clone());
        let flight = state.flights.remove(&view.id);
        drop(state);
        if let Some(flight) = flight {
            flight.complete(Ok(view.clone()));
        }
        Ok(view)
    }
    #[cfg(test)]
    pub(crate) fn get_or_load(
        &self,
        id: &str,
        load: impl FnOnce() -> Result<Snapshot, AppError>,
    ) -> Result<Arc<SnapshotView>, AppError> {
        self.get_or_load_guarded(id, || Ok(()), load)
    }
    pub(crate) fn get_or_load_guarded<G>(
        &self,
        id: &str,
        begin_build: impl FnOnce() -> Result<G, AppError>,
        load: impl FnOnce() -> Result<Snapshot, AppError>,
    ) -> Result<Arc<SnapshotView>, AppError> {
        let (flight, owner) = {
            let mut state = self.state.lock().map_err(cache_error)?;
            if let Some(position) = state.entries.iter().position(|entry| entry.id == id) {
                let view = state.entries.remove(position).expect("Position was found");
                state.entries.push_back(view.clone());
                return Ok(view);
            }
            if let Some(flight) = state.flights.get(id) {
                (flight.clone(), false)
            } else {
                let flight = Arc::new(Flight::new());
                state.flights.insert(id.to_owned(), flight.clone());
                (flight, true)
            }
        };
        if !owner {
            return flight.wait();
        }
        // Decode/index without the ready-cache mutex. The same ID shares a
        // flight; unrelated hits and loads remain independent.
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Retain the owner's budget through decoding AND index/prewarm.
            let build_guard = begin_build()?;
            let snapshot = load()?;
            if snapshot.id != id {
                return Err(AppError::new(
                    "invalid_snapshot",
                    "Stored snapshot identity does not match its key",
                ));
            }
            SnapshotView::new(snapshot).map(|view| (Arc::new(view), build_guard))
        }))
        .unwrap_or_else(|_| {
            Err(AppError::new(
                "worker_panic",
                "Snapshot loading stopped unexpectedly",
            ))
        });
        let (loaded, build_guard) = match built {
            Ok((view, guard)) => (Ok(view), Some(guard)),
            Err(error) => (Err(error), None),
        };
        let published = match self.state.lock() {
            Ok(mut state) => {
                if state
                    .flights
                    .get(id)
                    .is_some_and(|current| Arc::ptr_eq(current, &flight))
                {
                    state.flights.remove(id);
                    if let Ok(view) = &loaded {
                        self.insert_locked(&mut state, view.clone());
                    }
                    loaded
                } else {
                    // Release or an explicit newer insertion invalidated this
                    // load; its old decoded tree must never be resurrected.
                    state
                        .entries
                        .iter()
                        .find(|entry| entry.id == id)
                        .cloned()
                        .ok_or_else(|| {
                            AppError::new("not_found", "Snapshot was released during loading")
                        })
                }
            }
            Err(error) => Err(cache_error(error)),
        };
        flight.complete(published);
        drop(build_guard);
        flight.wait()
    }
    fn insert_locked(&self, state: &mut State, view: Arc<SnapshotView>) {
        if let Some(position) = state.entries.iter().position(|old| old.id == view.id) {
            if let Some(old) = state.entries.remove(position) {
                state.bytes = state.bytes.saturating_sub(old.estimated_bytes);
            }
        }
        while !state.entries.is_empty()
            && (state.entries.len() >= 2
                || state.bytes.saturating_add(view.estimated_bytes) > self.budget)
        {
            if let Some(old) = state.entries.pop_front() {
                state.bytes = state.bytes.saturating_sub(old.estimated_bytes);
            }
        }
        // An oversized view lives alone. The bound is max(budget, one snapshot),
        // not a hard process-RSS ceiling; in-flight readers can retain old Arcs.
        state.bytes = state.bytes.saturating_add(view.estimated_bytes);
        state.entries.push_back(view);
    }
    pub(crate) fn remove(&self, id: &str) -> Result<(), AppError> {
        let mut state = self.state.lock().map_err(cache_error)?;
        if let Some(position) = state.entries.iter().position(|view| view.id == id) {
            if let Some(old) = state.entries.remove(position) {
                state.bytes = state.bytes.saturating_sub(old.estimated_bytes);
            }
        }
        let flight = state.flights.remove(id);
        drop(state);
        if let Some(flight) = flight {
            flight.complete(Err(AppError::new(
                "not_found",
                "Snapshot was released during loading",
            )));
        }
        Ok(())
    }
}
fn cache_error(error: impl std::fmt::Display) -> AppError {
    AppError::new("state_error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmydisk_core::{NativePath, NodeKind};
    use std::{
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    fn snapshot(id: &str) -> Snapshot {
        let root = Node {
            space_usage: None,
            id: "root".into(),
            parent_id: None,
            name: "root".into(),
            path: NativePath::new(Path::new("root")),
            kind: NodeKind::Directory,
            logical_bytes: 1,
            allocated_bytes: Some(4096),
            modified_ns: Some(0),
            identity: "root-id".into(),
            volume_id: "volume".into(),
            link_count: 1,
            complete: true,
            is_link_or_reparse: false,
        };
        let mut child = root.clone();
        child.id = "child".into();
        child.parent_id = Some(root.id.clone());
        child.name = "child".into();
        child.kind = NodeKind::File;
        child.identity = "child-id".into();
        Snapshot {
            space_accounting: None,
            id: id.into(),
            scan_id: id.into(),
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            created_at: 0,
            complete: true,
            cancelled: false,
            nodes: vec![root, child],
            issues: vec![],
        }
    }
    #[test]
    fn concurrent_readers_share_the_same_loaded_pair() {
        let cache = Arc::new(SnapshotCache::default());
        let loads = Arc::new(AtomicUsize::new(0));
        let workers = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let loads = loads.clone();
                std::thread::spawn(move || {
                    cache
                        .get_or_load("shared", || {
                            loads.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(10));
                            Ok(snapshot("shared"))
                        })
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let views = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        for view in &views {
            assert!(Arc::ptr_eq(&views[0], view));
            assert_eq!(view.query(&Query::default()).unwrap()["total_matches"], 1);
        }
    }
    #[test]
    fn lru_is_bounded_and_oversized_snapshot_stays_alone() {
        let cache = SnapshotCache::new(usize::MAX);
        cache.insert(snapshot("a")).unwrap();
        cache.insert(snapshot("b")).unwrap();
        cache.get_or_load("a", || panic!("Should hit")).unwrap();
        cache.insert(snapshot("c")).unwrap();
        let state = cache.state.lock().unwrap();
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].id, "a");
        assert_eq!(state.entries[1].id, "c");
        drop(state);
        let cache = SnapshotCache::new(1);
        cache.insert(snapshot("a")).unwrap();
        cache.insert(snapshot("b")).unwrap();
        let state = cache.state.lock().unwrap();
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].id, "b");
    }
    #[test]
    fn eviction_does_not_mutate_an_in_flight_view_or_resurrect_released_data() {
        let cache = SnapshotCache::default();
        let view = cache.insert(snapshot("a")).unwrap();
        cache.remove("a").unwrap();
        assert_eq!(
            view.query(&Query::default()).unwrap()["items"][0]["id"],
            "child"
        );
        assert!(
            cache
                .get_or_load("a", || Err(AppError::new("not_found", "Released")))
                .is_err()
        );
    }
    #[test]
    fn index_is_rebuilt_together_with_a_replacement_snapshot() {
        let cache = SnapshotCache::default();
        let old = cache.insert(snapshot("a")).unwrap();
        let mut replacement = snapshot("a");
        replacement.nodes[1].name = "new child".into();
        let new = cache.insert(replacement).unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        assert_eq!(
            old.query(&Query::default()).unwrap()["items"][0]["name"],
            "child"
        );
        assert_eq!(
            new.query(&Query::default()).unwrap()["items"][0]["name"],
            "new child"
        );
    }

    #[test]
    fn cold_load_does_not_hold_up_a_different_ready_snapshot() {
        let cache = Arc::new(SnapshotCache::default());
        cache.insert(snapshot("ready")).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let loader_cache = cache.clone();
        let loader = std::thread::spawn(move || {
            loader_cache.get_or_load("cold", || {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(snapshot("cold"))
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (hit_tx, hit_rx) = std::sync::mpsc::channel();
        let hit = std::thread::spawn(move || {
            hit_tx
                .send(
                    cache
                        .get_or_load("ready", || panic!("Already cached"))
                        .unwrap()
                        .id
                        .clone(),
                )
                .unwrap()
        });
        let result = hit_rx.recv_timeout(Duration::from_millis(500));
        finish_tx.send(()).unwrap();
        loader.join().unwrap().unwrap();
        hit.join().unwrap();
        assert_eq!(result.unwrap(), "ready");
    }

    #[test]
    fn owner_build_budget_is_retained_until_indexed_view_is_published() {
        struct Guard {
            cache: Arc<SnapshotCache>,
            published: Arc<AtomicUsize>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                let state = self.cache.state.lock().unwrap();
                self.published.store(
                    usize::from(state.entries.iter().any(|view| view.id == "guarded")),
                    Ordering::SeqCst,
                );
            }
        }
        let cache = Arc::new(SnapshotCache::default());
        let published = Arc::new(AtomicUsize::new(0));
        let guard_cache = cache.clone();
        let guard_published = published.clone();
        let view = cache
            .get_or_load_guarded(
                "guarded",
                || {
                    Ok(Guard {
                        cache: guard_cache,
                        published: guard_published,
                    })
                },
                || Ok(snapshot("guarded")),
            )
            .unwrap();
        assert_eq!(published.load(Ordering::SeqCst), 1);
        assert_eq!(view.query(&Query::default()).unwrap()["total_matches"], 1);
    }

    #[test]
    fn release_during_a_load_does_not_resurrect_the_view() {
        let cache = Arc::new(SnapshotCache::default());
        let (started, entered) = std::sync::mpsc::channel();
        let (finish, released) = std::sync::mpsc::channel();
        let loading = cache.clone();
        let worker = std::thread::spawn(move || {
            loading.get_or_load("released", || {
                started.send(()).unwrap();
                released.recv().unwrap();
                Ok(snapshot("released"))
            })
        });
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        cache.remove("released").unwrap();
        finish.send(()).unwrap();
        assert_eq!(worker.join().unwrap().err().unwrap().code, "not_found");
        assert!(cache.state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn failed_or_panicking_owner_notifies_waiters_and_allows_a_retry() {
        let cache = SnapshotCache::default();
        let error = cache
            .get_or_load("retry", || panic!("Simulated loader failure"))
            .err()
            .unwrap();
        assert_eq!(error.code, "worker_panic");
        assert_eq!(
            cache
                .get_or_load("retry", || Ok(snapshot("retry")))
                .unwrap()
                .id,
            "retry"
        );
    }
}
