use ohmydisk_core::{
    AppError, AttributionProvider, DirectoryEvent, FileInfo, NodeKind, Platform, QueryIndex,
    ScanEntry, ScanProgress, Snapshot, SpaceObservation, Volume,
    scan_async::{ScanOptions, scan_async},
};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

fn info(kind: NodeKind, identity: &str, logical: u64) -> FileInfo {
    FileInfo {
        private_bytes: Some(logical / 2),
        kind,
        logical_bytes: logical,
        allocated_bytes: Some(logical),
        modified_ns: Some(0),
        identity: identity.into(),
        volume_id: "volume".into(),
        link_count: 1,
        is_link_or_reparse: false,
    }
}

#[derive(Default)]
struct Gate {
    arrived: Mutex<usize>,
    changed: Condvar,
}
impl Gate {
    fn enter(&self) -> io::Result<()> {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        self.changed.notify_all();
        let (arrived, _) = self
            .changed
            .wait_timeout_while(arrived, Duration::from_secs(3), |n| *n < 2)
            .unwrap();
        if *arrived < 2 {
            Err(io::Error::other("directory workers did not overlap"))
        } else {
            Ok(())
        }
    }
}

struct Fake {
    root: PathBuf,
    infos: HashMap<PathBuf, FileInfo>,
    entries: HashMap<PathBuf, Vec<PathBuf>>,
    entered: Mutex<Vec<PathBuf>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    gate: Option<Gate>,
    change_after_parent: AtomicBool,
    foreign_after_parent: bool,
    read_error: bool,
    read_error_paths: Vec<PathBuf>,
    observations: bool,
    observe_count: AtomicUsize,
    cancel: Option<Arc<AtomicBool>>,
    panic_child: bool,
    emitted: AtomicUsize,
}
impl Fake {
    fn new(root: PathBuf) -> Self {
        let mut infos = HashMap::new();
        infos.insert(root.clone(), info(NodeKind::Directory, "root", 4096));
        Self {
            root,
            infos,
            entries: HashMap::new(),
            entered: Mutex::new(Vec::new()),
            active: 0.into(),
            max_active: 0.into(),
            gate: None,
            change_after_parent: false.into(),
            foreign_after_parent: false,
            read_error: false,
            read_error_paths: Vec::new(),
            observations: false,
            observe_count: 0.into(),
            cancel: None,
            panic_child: false,
            emitted: 0.into(),
        }
    }
    fn entry(&mut self, relative: &str, metadata: FileInfo) {
        let path = self.root.join(relative);
        self.entries
            .entry(path.parent().unwrap().to_path_buf())
            .or_default()
            .push(path.clone());
        self.infos.insert(path, metadata);
    }
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Platform for Fake {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        let mut metadata = self
            .infos
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "not readable"))?;
        if self.foreign_after_parent
            && path != self.root
            && metadata.kind == NodeKind::Directory
            && self.change_after_parent.load(Ordering::SeqCst)
        {
            metadata.volume_id = "foreign".into();
        }
        Ok(metadata)
    }
    fn read_directory(
        &self,
        path: &Path,
        visit: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        self.entered.lock().unwrap().push(path.to_path_buf());
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        if path != self.root {
            if let Some(gate) = &self.gate {
                gate.enter()?;
            }
            if self.panic_child && path.ends_with("panic") {
                panic!("modeled directory worker failure");
            }
            if self.panic_child {
                for i in 0..100_000 {
                    self.emitted.fetch_add(1, Ordering::SeqCst);
                    if !visit(DirectoryEvent::Entry(ScanEntry {
                        kind_hint: Some(NodeKind::File),
                        path: path.join(format!("file-{i}")),
                        info: Some(Ok(info(NodeKind::File, "busy-file", 1))),
                    })) {
                        break;
                    }
                }
                return Ok(());
            }
        }
        for (index, entry) in self.entries.get(path).into_iter().flatten().enumerate() {
            self.emitted.fetch_add(1, Ordering::SeqCst);
            if index == 15 {
                if let Some(cancel) = &self.cancel {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            let metadata = self.infos.get(entry).cloned();
            let kind_hint = metadata.as_ref().map(|i| i.kind);
            let info = metadata.filter(|i| i.kind != NodeKind::Directory).map(Ok);
            if !visit(DirectoryEvent::Entry(ScanEntry {
                path: entry.clone(),
                info,
                kind_hint,
            })) {
                break;
            }
        }
        if path == self.root {
            self.change_after_parent.store(true, Ordering::SeqCst);
        }
        if self.read_error || self.read_error_paths.iter().any(|entry| entry == path) {
            Err(io::Error::other("partial enumeration failure"))
        } else {
            Ok(())
        }
    }
    fn space_observation(&self, _: &Path) -> io::Result<Option<SpaceObservation>> {
        if !self.observations {
            return Ok(None);
        }
        let count = self.observe_count.fetch_add(1, Ordering::SeqCst);
        Ok(Some(SpaceObservation {
            volume_id: "volume".into(),
            total_bytes: 1000,
            available_bytes: if count == 0 { 500 } else { 400 },
            captured_at: count as u64,
            source: "test".into(),
            capacity_scope: "volume".into(),
            provider: AttributionProvider::MacPrivateSize,
            used_bytes: None,
            caller_available_bytes: None,
        }))
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(Vec::new())
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        panic!("scanner must not clean files")
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        panic!("scanner must not reveal files")
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}
fn options(workers: usize) -> ScanOptions {
    ScanOptions {
        workers,
        batch_entries: 2,
        batch_bytes: 512,
    }
}
async fn run(
    fake: Arc<Fake>,
    workers: usize,
    budget: usize,
    cancel: Arc<AtomicBool>,
) -> Result<Snapshot, AppError> {
    let (progress, _) = watch::channel(ScanProgress::default());
    tokio::time::timeout(
        Duration::from_secs(8),
        scan_async(
            fake.root.clone(),
            fake,
            cancel,
            options(workers),
            Arc::new(Semaphore::new(budget)),
            progress,
        ),
    )
    .await
    .expect("scanner did not stop")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn siblings_overlap_and_every_parent_precedes_its_children() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("left", info(NodeKind::Directory, "left", 4096));
    fake.entry("right", info(NodeKind::Directory, "right", 4096));
    fake.entry("left/one", info(NodeKind::File, "one", 11));
    fake.entry("right/two", info(NodeKind::File, "two", 17));
    fake.gate = Some(Gate::default());
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 2, 2, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.nodes[0].logical_bytes, 28);
    assert_eq!(fake.max_active.load(Ordering::SeqCst), 2);
    for (position, node) in snapshot.nodes.iter().enumerate().skip(1) {
        assert!(node.parent_id.as_ref().unwrap().parse::<usize>().unwrap() < position);
    }
    QueryIndex::new(&snapshot).unwrap();
}

#[tokio::test]
async fn filesystem_budget_limits_reads_even_with_more_workers() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    for i in 0..20 {
        fake.entry(
            &format!("dir-{i}"),
            info(NodeKind::Directory, &format!("dir-{i}"), 0),
        );
        fake.entry(
            &format!("dir-{i}/file"),
            info(NodeKind::File, &format!("file-{i}"), 1),
        );
    }
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 8, 1, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.nodes[0].logical_bytes, 20);
    assert_eq!(fake.max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn directory_identity_is_admitted_once_before_either_read() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    for name in ["original", "alias"] {
        fake.entry(name, info(NodeKind::Directory, "same", 4096));
        fake.entry(&format!("{name}/file"), info(NodeKind::File, "file", 17));
    }
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 4, 4, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(!snapshot.complete);
    assert_eq!(snapshot.nodes[0].logical_bytes, 17);
    assert_eq!(fake.entered.lock().unwrap().len(), 2);
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .filter(|n| n.identity == "same" && !n.complete && n.logical_bytes == 0)
            .count(),
        1
    );
    assert!(
        snapshot
            .issues
            .iter()
            .any(|issue| issue.code == "boundary_skipped")
    );
    let alias = snapshot
        .nodes
        .iter()
        .find(|node| node.identity == "same" && !node.complete)
        .unwrap();
    assert_eq!(alias.space_usage.as_ref().unwrap().unmeasured_entries, 1);
    assert_eq!(
        ohmydisk_core::space::metric_value(alias, ohmydisk_core::SizeMetric::Attributed),
        None
    );
    QueryIndex::new(&snapshot).unwrap();
}

#[tokio::test]
async fn queued_directory_is_rechecked_after_parent_finishes() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("queued", info(NodeKind::Directory, "queued", 4096));
    fake.foreign_after_parent = true;
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 4, 4, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(!snapshot.complete);
    assert_eq!(fake.entered.lock().unwrap().len(), 1);
    assert_eq!(snapshot.nodes[1].volume_id, "foreign");
}

#[tokio::test]
async fn partial_enumeration_retains_entries_and_propagates_incompleteness() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("file", info(NodeKind::File, "file", 19));
    fake.read_error = true;
    let snapshot = run(Arc::new(fake), 2, 2, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(!snapshot.complete);
    assert!(!snapshot.cancelled);
    assert_eq!(snapshot.nodes[0].logical_bytes, 19);
    assert!(snapshot.nodes[1].complete);
    assert_eq!(snapshot.issues[0].code, "read_directory");
}

#[tokio::test]
async fn cancellation_joins_started_workers_before_return() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    for i in 0..1000 {
        fake.entry(
            &format!("file-{i}"),
            info(NodeKind::File, &format!("file-{i}"), 1),
        );
    }
    fake.cancel = Some(cancel.clone());
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 4, 4, cancel).await.unwrap();
    assert!(snapshot.cancelled);
    assert!(!snapshot.complete);
    assert!(
        snapshot.nodes.iter().filter(|node| node.kind == NodeKind::Directory)
            .all(|node| !node.complete && node.space_usage.as_ref().unwrap().unmeasured_entries > 0)
    );
    assert_eq!(fake.active.load(Ordering::SeqCst), 0);
    let emitted = fake.emitted.load(Ordering::SeqCst);
    tokio::task::yield_now().await;
    assert_eq!(fake.emitted.load(Ordering::SeqCst), emitted);
    QueryIndex::new(&snapshot).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_panic_stops_and_joins_other_readers() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("panic", info(NodeKind::Directory, "panic", 4096));
    fake.entry("busy", info(NodeKind::Directory, "busy", 4096));
    fake.gate = Some(Gate::default());
    fake.panic_child = true;
    let fake = Arc::new(fake);
    let error = run(fake.clone(), 2, 2, Arc::new(false.into()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "worker_panic");
    assert_eq!(fake.active.load(Ordering::SeqCst), 0);
    let emitted = fake.emitted.load(Ordering::SeqCst);
    tokio::task::yield_now().await;
    assert_eq!(fake.emitted.load(Ordering::SeqCst), emitted);
}

#[tokio::test]
async fn serial_and_concurrent_scans_keep_logical_and_private_measurements_separate() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    let mut file = info(NodeKind::File, "file", 900);
    file.private_bytes = Some(200);
    fake.entry("file", file);
    fake.observations = true;
    let fake = Arc::new(fake);
    let serial =
        ohmydisk_core::scan::scan(&fake.root, fake.as_ref(), &false.into(), &mut |_| {}).unwrap();
    fake.observe_count.store(0, Ordering::SeqCst);
    let parallel = run(fake, 2, 2, Arc::new(false.into())).await.unwrap();
    for snapshot in [serial, parallel] {
        assert_eq!(snapshot.nodes[0].logical_bytes, 900);
        assert_eq!(
            snapshot.nodes[0]
                .space_usage
                .as_ref()
                .unwrap()
                .attributed_bytes,
            200
        );
        let accounting = snapshot.space_accounting.unwrap();
        assert_eq!(accounting.attributed_bytes, 200);
        assert_eq!(accounting.shared_unattributed_bytes, Some(400));
        assert_eq!(accounting.measurement_status, "complete");
    }
}

#[tokio::test]
async fn hardlinks_and_missing_private_sizes_are_unmeasured() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    let mut hardlink = info(NodeKind::File, "linked", 100);
    hardlink.link_count = 2;
    fake.entry("hardlink", hardlink);
    let mut unknown = info(NodeKind::File, "unknown", 200);
    unknown.private_bytes = None;
    fake.entry("unknown", unknown);
    fake.observations = true;
    let snapshot = run(Arc::new(fake), 2, 2, Arc::new(false.into()))
        .await
        .unwrap();
    let usage = snapshot.nodes[0].space_usage.as_ref().unwrap();
    assert_eq!(usage.attributed_bytes, 0);
    assert_eq!(usage.unmeasured_entries, 2);
    assert_eq!(snapshot.nodes[0].allocated_bytes, None);
    assert_eq!(
        snapshot.space_accounting.unwrap().measurement_status,
        "unavailable"
    );
}

#[tokio::test]
async fn links_foreign_volumes_and_ancestor_aliases_are_never_entered() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    let mut link = info(NodeKind::Directory, "link", 1);
    link.is_link_or_reparse = true;
    fake.entry("link", link);
    let mut foreign = info(NodeKind::Directory, "mount", 2);
    foreign.volume_id = "foreign".into();
    fake.entry("mount", foreign);
    fake.entry("back-to-root", info(NodeKind::Directory, "root", 3));
    let fake = Arc::new(fake);
    let snapshot = run(fake.clone(), 4, 4, Arc::new(false.into()))
        .await
        .unwrap();
    assert_eq!(fake.entered.lock().unwrap().len(), 1);
    assert!(!snapshot.complete);
    assert_eq!(snapshot.issues.len(), 3);
}

#[tokio::test]
async fn two_scans_share_one_filesystem_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    for i in 0..8 {
        fake.entry(
            &format!("dir-{i}"),
            info(NodeKind::Directory, &format!("dir-{i}"), 0),
        );
        fake.entry(
            &format!("dir-{i}/file"),
            info(NodeKind::File, &format!("file-{i}"), 1),
        );
    }
    let fake = Arc::new(fake);
    let budget = Arc::new(Semaphore::new(1));
    let (progress_a, _) = watch::channel(ScanProgress::default());
    let (progress_b, _) = watch::channel(ScanProgress::default());
    let (first, second) = tokio::join!(
        scan_async(
            fake.root.clone(),
            fake.clone(),
            Arc::new(false.into()),
            options(4),
            budget.clone(),
            progress_a
        ),
        scan_async(
            fake.root.clone(),
            fake.clone(),
            Arc::new(false.into()),
            options(4),
            budget,
            progress_b
        ),
    );
    assert!(first.unwrap().complete);
    assert!(second.unwrap().complete);
    assert_eq!(fake.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(fake.entered.lock().unwrap().len(), 18);
}

#[tokio::test]
async fn cancellation_before_dispatch_returns_an_empty_incomplete_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::new(root.path().canonicalize().unwrap()));
    let snapshot = run(fake.clone(), 4, 4, Arc::new(true.into()))
        .await
        .unwrap();
    assert!(snapshot.cancelled);
    assert!(!snapshot.complete);
    assert!(snapshot.nodes.is_empty());
    assert!(fake.entered.lock().unwrap().is_empty());
    QueryIndex::new(&snapshot).unwrap();
}

#[tokio::test]
async fn deep_tree_uses_an_iterative_frontier() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    let mut relative = PathBuf::new();
    for depth in 0..200 {
        relative.push("d");
        fake.entry(
            relative.to_str().unwrap(),
            info(NodeKind::Directory, &format!("depth-{depth}"), 4096),
        );
    }
    relative.push("leaf");
    fake.entry(relative.to_str().unwrap(), info(NodeKind::File, "leaf", 23));
    let snapshot = run(Arc::new(fake), 8, 2, Arc::new(false.into()))
        .await
        .unwrap();
    assert!(snapshot.complete);
    assert_eq!(snapshot.nodes.len(), 202);
    assert_eq!(snapshot.nodes[0].logical_bytes, 23);
    QueryIndex::new(&snapshot).unwrap();
}

#[tokio::test]
async fn invalid_options_fail_before_touching_the_platform() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::new(root.path().canonicalize().unwrap()));
    let (progress, _) = watch::channel(ScanProgress::default());
    let error = scan_async(
        fake.root.clone(),
        fake.clone(),
        Arc::new(false.into()),
        options(0),
        Arc::new(Semaphore::new(1)),
        progress,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert_eq!(fake.observe_count.load(Ordering::SeqCst), 0);
    assert!(fake.entered.lock().unwrap().is_empty());
}

struct NoFilesystem;
impl Platform for NoFilesystem {
    fn metadata(&self, _: &Path) -> io::Result<FileInfo> {
        panic!("cancelled preparation touched metadata");
    }
    fn read_directory(
        &self,
        _: &Path,
        _: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        panic!("cancelled preparation enumerated a directory");
    }
    fn space_observation(&self, _: &Path) -> io::Result<Option<SpaceObservation>> {
        panic!("cancelled preparation sampled the filesystem");
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        panic!("cancelled preparation listed volumes");
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        unreachable!();
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        unreachable!();
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

#[tokio::test]
async fn preparation_cancels_without_waiting_for_another_scans_permit_or_touching_filesystem() {
    for cancel_before_call in [false, true] {
        let budget = Arc::new(Semaphore::new(1));
        let (holding, held) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let holders_budget = budget.clone();
        let holder = tokio::spawn(async move {
            let permit = holders_budget.acquire_owned().await.unwrap();
            holding.send(()).unwrap();
            released.await.unwrap();
            drop(permit);
        });
        held.await.unwrap();
        let cancel = Arc::new(AtomicBool::new(cancel_before_call));
        let (progress, changes) = watch::channel(ScanProgress::default());
        // This path need not exist: even canonicalization would violate this test.
        let requested = if cfg!(windows) {
            PathBuf::from(r"C:\cancelled-without-io")
        } else {
            PathBuf::from("/cancelled-without-io")
        };
        let scan = scan_async(
            requested.clone(),
            Arc::new(NoFilesystem),
            cancel.clone(),
            options(4),
            budget.clone(),
            progress,
        );
        tokio::pin!(scan);
        if !cancel_before_call {
            // Poll the scan into its permit wait before requesting cancellation.
            tokio::select! {
                biased;
                result = &mut scan => panic!("preparation returned while its permit was held: {result:?}"),
                () = tokio::task::yield_now() => {}
            }
            cancel.store(true, Ordering::SeqCst);
        }
        let snapshot = tokio::time::timeout(Duration::from_secs(2), &mut scan)
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.cancelled);
        assert!(!snapshot.complete);
        assert!(snapshot.nodes.is_empty());
        assert!(snapshot.space_accounting.is_none());
        assert_eq!(snapshot.root_path.to_path().unwrap(), requested);
        assert_eq!(changes.borrow().phase, "cancelled");
        assert_eq!(
            budget.available_permits(),
            0,
            "the holder still owns the only permit"
        );
        release.send(()).unwrap();
        holder.await.unwrap();
        assert_eq!(budget.available_permits(), 1);
    }
}

#[tokio::test]
async fn unread_empty_directory_is_unknown_in_both_scanners() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.read_error = true;
    fake.observations = true;
    let fake = Arc::new(fake);
    let serial =
        ohmydisk_core::scan::scan(&fake.root, fake.as_ref(), &false.into(), &mut |_| {}).unwrap();
    fake.observe_count.store(0, Ordering::SeqCst);
    let parallel = run(fake, 2, 2, Arc::new(false.into())).await.unwrap();
    for snapshot in [serial, parallel] {
        assert!(!snapshot.complete);
        let root = &snapshot.nodes[0];
        let usage = root.space_usage.as_ref().unwrap();
        assert_eq!(usage.attributed_bytes, 0);
        assert_eq!(usage.measured_entries, 0);
        assert_eq!(usage.unmeasured_entries, 1);
        assert_eq!(
            ohmydisk_core::space::metric_value(root, ohmydisk_core::SizeMetric::Attributed),
            None
        );
        assert_eq!(
            snapshot.space_accounting.unwrap().measurement_status,
            "unavailable"
        );
    }
}

#[tokio::test]
async fn measured_file_and_unread_subtree_keep_the_known_contribution_and_unknown_marker() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("known", info(NodeKind::File, "known", 100));
    fake.entry("denied", info(NodeKind::Directory, "denied", 4096));
    fake.read_error_paths.push(fake.root.join("denied"));
    fake.observations = true;
    let fake = Arc::new(fake);
    let serial =
        ohmydisk_core::scan::scan(&fake.root, fake.as_ref(), &false.into(), &mut |_| {}).unwrap();
    fake.observe_count.store(0, Ordering::SeqCst);
    let parallel = run(fake, 2, 2, Arc::new(false.into())).await.unwrap();
    for snapshot in [serial, parallel] {
        assert!(!snapshot.complete);
        let root = &snapshot.nodes[0];
        let usage = root.space_usage.as_ref().unwrap();
        assert_eq!(root.logical_bytes, 100);
        assert_eq!(usage.attributed_bytes, 50);
        assert_eq!(usage.measured_entries, 1);
        assert_eq!(usage.unmeasured_entries, 1);
        let denied = snapshot
            .nodes
            .iter()
            .find(|node| node.name == "denied")
            .unwrap();
        assert_eq!(denied.space_usage.as_ref().unwrap().unmeasured_entries, 1);
        assert_eq!(
            ohmydisk_core::space::metric_value(denied, ohmydisk_core::SizeMetric::Attributed),
            None
        );
        assert_eq!(
            snapshot.space_accounting.unwrap().measurement_status,
            "partial"
        );
    }
}

#[tokio::test]
async fn metadata_failure_marks_the_parent_space_unknown_without_hiding_known_files() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path().canonicalize().unwrap());
    fake.entry("known", info(NodeKind::File, "known", 100));
    fake.entries
        .entry(fake.root.clone())
        .or_default()
        .push(fake.root.join("missing-metadata"));
    fake.observations = true;
    let fake = Arc::new(fake);
    let serial =
        ohmydisk_core::scan::scan(&fake.root, fake.as_ref(), &false.into(), &mut |_| {}).unwrap();
    fake.observe_count.store(0, Ordering::SeqCst);
    let parallel = run(fake, 2, 2, Arc::new(false.into())).await.unwrap();
    for snapshot in [serial, parallel] {
        assert!(!snapshot.complete);
        let usage = snapshot.nodes[0].space_usage.as_ref().unwrap();
        assert_eq!(usage.attributed_bytes, 50);
        assert_eq!(usage.measured_entries, 1);
        assert_eq!(usage.unmeasured_entries, 1);
        assert!(
            snapshot
                .issues
                .iter()
                .any(|issue| issue.code == "permission_denied")
        );
    }
}
