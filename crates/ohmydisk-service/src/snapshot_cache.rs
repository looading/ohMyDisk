//! Immutable snapshot/index pairs with a small LRU. A single oversized newest
//! snapshot is retained rather than decoding that same large tree per request.
use ohmydisk_core::{AppError, Node, Query, QueryIndex, ScanIssue, Snapshot, query_indexed};
use serde_json::Value;
use std::{
    collections::VecDeque,
    mem::size_of,
    ops::Deref,
    sync::{Arc, Mutex},
};

pub(crate) struct SnapshotView {
    snapshot: Snapshot,
    index: QueryIndex,
    estimated_bytes: usize,
}
impl SnapshotView {
    fn new(snapshot: Snapshot) -> Result<Self, AppError> {
        let index = QueryIndex::new(&snapshot)?;
        let mut bytes = size_of::<Self>()
            .saturating_add(snapshot.nodes.capacity().saturating_mul(size_of::<Node>()))
            .saturating_add(
                snapshot
                    .issues
                    .capacity()
                    .saturating_mul(size_of::<ScanIssue>()),
            )
            .saturating_add(index.estimated_heap_bytes());
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

struct State {
    entries: VecDeque<Arc<SnapshotView>>,
    bytes: usize,
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
            }),
            budget,
        }
    }
    pub(crate) fn insert(&self, snapshot: Snapshot) -> Result<Arc<SnapshotView>, AppError> {
        let view = Arc::new(SnapshotView::new(snapshot)?);
        let mut state = self.state.lock().map_err(cache_error)?;
        self.insert_locked(&mut state, view.clone());
        Ok(view)
    }
    pub(crate) fn get_or_load(
        &self,
        id: &str,
        load: impl FnOnce() -> Result<Snapshot, AppError>,
    ) -> Result<Arc<SnapshotView>, AppError> {
        // A miss is single-flight: callers cannot decode duplicate copies while
        // waiting. Engine invokes this on blocking workers, never the UI/runtime.
        let mut state = self.state.lock().map_err(cache_error)?;
        if let Some(position) = state.entries.iter().position(|entry| entry.id == id) {
            let view = state.entries.remove(position).expect("Position was found");
            state.entries.push_back(view.clone());
            return Ok(view);
        }
        let snapshot = load()?;
        if snapshot.id != id {
            return Err(AppError::new(
                "invalid_snapshot",
                "Stored snapshot identity does not match its key",
            ));
        }
        let view = Arc::new(SnapshotView::new(snapshot)?);
        self.insert_locked(&mut state, view.clone());
        Ok(view)
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
}
