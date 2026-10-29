use ohmydisk_core::*;
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
struct TestPlatform;
impl Platform for TestPlatform {
    fn metadata(&self, p: &Path) -> io::Result<FileInfo> {
        let m = std::fs::symlink_metadata(p)?;
        let kind = if m.is_symlink() {
            NodeKind::Symlink
        } else if m.is_dir() {
            NodeKind::Directory
        } else if m.is_file() {
            NodeKind::File
        } else {
            NodeKind::Other
        };
        #[cfg(unix)]
        let (identity, volume, allocated, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (
                format!("{}:{}", m.dev(), m.ino()),
                m.dev().to_string(),
                Some(m.blocks() * 512),
                m.nlink(),
            )
        };
        #[cfg(windows)]
        let (identity, volume, allocated, nlink) = {
            // Model one identity for ordinary, extended-length and case aliases
            // on the fixture's default case-insensitive Windows filesystem.
            let canonical = std::fs::canonicalize(p)?;
            (
                format!(
                    "{}:{:?}",
                    canonical.to_string_lossy().to_lowercase(),
                    m.created()
                ),
                "test".into(),
                None,
                1,
            )
        };
        Ok(FileInfo {
            private_bytes: None,
            kind,
            logical_bytes: m.len(),
            allocated_bytes: allocated,
            modified_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos().try_into().unwrap_or(u64::MAX)),
            identity,
            volume_id: volume,
            link_count: nlink,
            is_link_or_reparse: m.is_symlink(),
        })
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(vec![])
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test trash disabled",
        ))
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        vec![]
    }
}
fn scanned(p: &Path) -> Snapshot {
    ohmydisk_core::scan::scan(p, &TestPlatform, &AtomicBool::new(false), &mut |_| {}).unwrap()
}
#[test]
fn hidden_files_and_stable_pagination() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join(".hidden")).unwrap();
    std::fs::write(t.path().join(".hidden/a"), b"12345").unwrap();
    std::fs::write(t.path().join("b"), b"123").unwrap();
    let s = scanned(t.path());
    assert_eq!(s.nodes[0].logical_bytes, 8);
    assert!(s.complete);
    let q = Query {
        scope_node_id: Some(s.root_id.clone()),
        limit: Some(1),
        ..Default::default()
    };
    let first = ohmydisk_core::query::query(&s, &q).unwrap();
    assert_eq!(first["items"][0]["name"], ".hidden");
    let mut q2 = q.clone();
    q2.cursor = first["next_cursor"].as_str().map(str::to_owned);
    assert_eq!(
        ohmydisk_core::query::query(&s, &q2).unwrap()["items"][0]["name"],
        "b"
    );
    q2.min_bytes = Some(1);
    assert_eq!(
        ohmydisk_core::query::query(&s, &q2).unwrap_err().code,
        "invalid_cursor"
    );
}
#[test]
fn plans_deduplicate_and_do_not_delete_new_members() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("selected");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("a"), b"123").unwrap();
    let s = scanned(t.path());
    let parent = s.nodes.iter().find(|n| n.name == "selected").unwrap();
    let child = s.nodes.iter().find(|n| n.name == "a").unwrap();
    let p = ohmydisk_core::cleanup::prepare_plan(
        &s,
        &[parent.id.clone(), child.id.clone()],
        CleanupMode::Permanent,
        &[],
    )
    .unwrap();
    assert_eq!(p.targets.len(), 1);
    assert_eq!(p.logical_bytes, 3);
    std::fs::write(d.join("new"), b"new").unwrap();
    let r = ohmydisk_core::cleanup::execute_plan(&p, &TestPlatform, &false.into(), &mut |_| Ok(()))
        .unwrap();
    assert!(r.iter().any(|r| r.code.as_deref() == Some("stale_plan")));
    assert!(d.join("new").exists());
    assert!(d.join("a").exists());
}
#[test]
fn permanent_delete_is_explicit_and_trash_never_falls_back() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("a"), b"abc").unwrap();
    let s = scanned(t.path());
    let n = s.nodes.iter().find(|n| n.name == "a").unwrap();
    let trash = ohmydisk_core::cleanup::prepare_plan(
        &s,
        std::slice::from_ref(&n.id),
        CleanupMode::Trash,
        &[],
    )
    .unwrap();
    let r =
        ohmydisk_core::cleanup::execute_plan(&trash, &TestPlatform, &false.into(), &mut |_| Ok(()))
            .unwrap();
    assert_eq!(r[0].status, ItemStatus::Failed);
    assert!(t.path().join("a").exists());
    let p = ohmydisk_core::cleanup::prepare_plan(
        &s,
        std::slice::from_ref(&n.id),
        CleanupMode::Permanent,
        &[],
    )
    .unwrap();
    let r = ohmydisk_core::cleanup::execute_plan(&p, &TestPlatform, &false.into(), &mut |_| Ok(()))
        .unwrap();
    assert_eq!(r[0].status, ItemStatus::Succeeded);
    assert!(!t.path().join("a").exists());
}
#[test]
fn root_protection_and_write_ahead_failure() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("a"), b"x").unwrap();
    let s = scanned(t.path());
    assert!(
        ohmydisk_core::cleanup::prepare_plan(
            &s,
            std::slice::from_ref(&s.root_id),
            CleanupMode::Permanent,
            &[]
        )
        .is_err()
    );
    let n = s.nodes.iter().find(|n| n.name == "a").unwrap();
    let p = ohmydisk_core::cleanup::prepare_plan(
        &s,
        std::slice::from_ref(&n.id),
        CleanupMode::Permanent,
        &[],
    )
    .unwrap();
    assert!(
        ohmydisk_core::cleanup::execute_plan(&p, &TestPlatform, &false.into(), &mut |_| Err(
            AppError::new("store_error", "no write ahead log")
        ))
        .is_err()
    );
    assert!(t.path().join("a").exists());
}
#[test]
fn cancelled_scan_is_incomplete() {
    let t = tempfile::tempdir().unwrap();
    let s = ohmydisk_core::scan::scan(t.path(), &TestPlatform, &true.into(), &mut |_| {}).unwrap();
    assert!(s.cancelled);
    assert!(!s.complete);
}

#[test]
fn protected_tree_alias_and_cancel_before_trash() {
    let t = tempfile::tempdir().unwrap();
    let secret = t.path().join("ProtectedTree");
    std::fs::create_dir(&secret).unwrap();
    std::fs::write(secret.join("a"), b"data").unwrap();
    let s = scanned(t.path());
    let child = s.nodes.iter().find(|n| n.name == "a").unwrap();
    let p = ohmydisk_core::cleanup::prepare_plan(
        &s,
        std::slice::from_ref(&child.id),
        CleanupMode::Trash,
        &[],
    )
    .unwrap();
    assert_eq!(
        ohmydisk_core::cleanup::validate_plan_scope(
            &p,
            std::slice::from_ref(&secret),
            &TestPlatform
        )
        .unwrap_err()
        .code,
        "path_not_allowed"
    );
    let alias = t.path().join("protectedtree");
    if alias.exists() {
        assert!(ohmydisk_core::cleanup::validate_plan_scope(&p, &[alias], &TestPlatform).is_err());
    }
    let cancel = AtomicBool::new(false);
    let results = ohmydisk_core::cleanup::execute_plan(&p, &TestPlatform, &cancel, &mut |event| {
        if event.status == ItemStatus::Started {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(results[0].status, ItemStatus::NotStarted);
    assert!(secret.join("a").exists());
    let permanent = ohmydisk_core::cleanup::prepare_plan(
        &s,
        std::slice::from_ref(&child.id),
        CleanupMode::Permanent,
        &[],
    )
    .unwrap();
    cancel.store(false, std::sync::atomic::Ordering::SeqCst);
    let results =
        ohmydisk_core::cleanup::execute_plan(&permanent, &TestPlatform, &cancel, &mut |event| {
            if event.status == ItemStatus::Started {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(results[0].status, ItemStatus::NotStarted);
    assert!(secret.join("a").exists());
}
#[cfg(unix)]
#[test]
fn native_path_roundtrip_and_link_boundary() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let t = tempfile::tempdir().unwrap();
    let raw = t.path().join(std::ffi::OsString::from_vec(vec![b'a', 255]));
    let np = NativePath::new(&raw);
    assert_eq!(np.to_path().unwrap(), raw);
    std::fs::write(t.path().join("valid"), b"abc").unwrap();
    symlink(t.path(), t.path().join("loop")).unwrap();
    let s = scanned(t.path());
    assert!(s.nodes.len() < 10);
    assert!(!s.complete);
}

struct StreamingPlatform {
    produced: std::sync::atomic::AtomicUsize,
    metadata_calls: std::sync::atomic::AtomicUsize,
    errors: bool,
}
impl Platform for StreamingPlatform {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        self.metadata_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TestPlatform.metadata(path)
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(Vec::new())
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("not used"))
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("not used"))
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn read_directory(
        &self,
        path: &Path,
        visit: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        let parent = TestPlatform.metadata(path)?;
        if self.errors {
            if !visit(DirectoryEvent::Error(io::Error::other(
                "partial directory error",
            ))) {
                return Ok(());
            }
            if !visit(DirectoryEvent::Entry(ScanEntry {
                kind_hint: None,
                path: path.join("denied"),
                info: Some(Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "denied",
                ))),
            })) {
                return Ok(());
            }
        }
        for i in 0..if self.errors { 1 } else { 100_000 } {
            self.produced
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let entry = ScanEntry {
                kind_hint: None,
                path: path.join(format!("file-{i}")),
                info: Some(Ok(FileInfo {
                    private_bytes: None,
                    kind: NodeKind::File,
                    logical_bytes: 1,
                    allocated_bytes: Some(0),
                    modified_ns: Some(0),
                    identity: format!("virtual-{i}"),
                    volume_id: parent.volume_id.clone(),
                    link_count: 1,
                    is_link_or_reparse: false,
                })),
            };
            if !visit(DirectoryEvent::Entry(entry)) {
                break;
            }
        }
        Ok(())
    }
}

#[test]
fn streaming_metadata_is_reused_and_cancel_stops_enumeration() {
    let root = tempfile::tempdir().unwrap();
    let platform = StreamingPlatform {
        produced: 0.into(),
        metadata_calls: 0.into(),
        errors: false,
    };
    let cancelled = AtomicBool::new(false);
    let snapshot = ohmydisk_core::scan::scan(root.path(), &platform, &cancelled, &mut |p| {
        if p.files >= 127 {
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .unwrap();
    assert!(snapshot.cancelled);
    assert!(!snapshot.complete);
    assert_eq!(snapshot.nodes.len(), 128);
    assert_eq!(snapshot.nodes[0].logical_bytes, 127);
    assert_eq!(
        platform
            .metadata_calls
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert_eq!(
        platform.produced.load(std::sync::atomic::Ordering::Relaxed),
        127
    );
}

#[test]
fn streaming_errors_do_not_hide_later_entries() {
    let root = tempfile::tempdir().unwrap();
    let platform = StreamingPlatform {
        produced: 0.into(),
        metadata_calls: 0.into(),
        errors: true,
    };
    let snapshot =
        ohmydisk_core::scan::scan(root.path(), &platform, &false.into(), &mut |_| {}).unwrap();
    assert!(!snapshot.complete);
    assert!(!snapshot.cancelled);
    assert_eq!(snapshot.nodes.len(), 2);
    assert!(snapshot.nodes[1].complete);
    assert_eq!(snapshot.nodes[0].logical_bytes, 1);
    assert!(snapshot.issues.iter().any(|i| i.code == "read_directory"));
    assert!(
        snapshot
            .issues
            .iter()
            .any(|i| i.code == "permission_denied")
    );
}

#[test]
fn queued_directory_boundary_is_checked_when_it_is_visited() {
    struct ChangedVolume {
        root: PathBuf,
        changed: AtomicBool,
        entered: std::sync::atomic::AtomicUsize,
    }
    impl Platform for ChangedVolume {
        fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
            let mut info = TestPlatform.metadata(path)?;
            if path == self.root.join("queued")
                && self.changed.load(std::sync::atomic::Ordering::Relaxed)
            {
                info.volume_id = "different-volume".into();
                info.identity = "different-directory".into();
            }
            Ok(info)
        }
        fn read_directory(
            &self,
            path: &Path,
            visit: &mut dyn FnMut(DirectoryEvent) -> bool,
        ) -> io::Result<()> {
            if path == self.root {
                visit(DirectoryEvent::Entry(ScanEntry {
                    kind_hint: None,
                    path: path.join("queued"),
                    info: None,
                }));
                self.changed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            } else {
                self.entered
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(io::Error::other("Foreign directory must not be enumerated"))
            }
        }
        fn volumes(&self) -> io::Result<Vec<Volume>> {
            Ok(Vec::new())
        }
        fn trash(&self, _: &Path) -> io::Result<()> {
            Err(io::Error::other("not used"))
        }
        fn reveal(&self, _: &Path) -> io::Result<()> {
            Err(io::Error::other("not used"))
        }
        fn protected_paths(&self) -> Vec<PathBuf> {
            Vec::new()
        }
    }
    let fixture = tempfile::tempdir().unwrap();
    std::fs::create_dir(fixture.path().join("queued")).unwrap();
    let platform = ChangedVolume {
        root: std::fs::canonicalize(fixture.path()).unwrap(),
        changed: false.into(),
        entered: 0.into(),
    };
    let snapshot =
        ohmydisk_core::scan::scan(fixture.path(), &platform, &false.into(), &mut |_| {}).unwrap();
    assert_eq!(
        platform.entered.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(!snapshot.complete);
    assert!(
        snapshot
            .issues
            .iter()
            .any(|issue| issue.code == "boundary_skipped")
    );
}

/// Models two paths to one physical directory, as macOS firmlinks can expose.
/// Only the empty test root exists on disk; all entries below it are virtual.
struct DirectoryAliasPlatform {
    root: PathBuf,
    identity: Option<&'static str>,
    reverse: bool,
    entered: std::sync::Mutex<Vec<PathBuf>>,
}
impl DirectoryAliasPlatform {
    fn info(&self, path: &Path) -> FileInfo {
        let root = path == self.root;
        let directory = root
            || self.identity == Some("root-directory")
            || path.parent() == Some(self.root.as_path());
        FileInfo {
            private_bytes: None,
            kind: if directory {
                NodeKind::Directory
            } else {
                NodeKind::File
            },
            logical_bytes: if directory { 4096 } else { 7 },
            allocated_bytes: Some(if directory { 4096 } else { 512 }),
            modified_ns: Some(0),
            identity: if root {
                "root-directory".into()
            } else if directory {
                self.identity
                    .map_or_else(|| format!("directory:{}", path.display()), str::to_owned)
            } else if self.identity.is_none_or(str::is_empty) {
                format!("file:{}", path.display())
            } else {
                "one-physical-file".into()
            },
            volume_id: "same-volume".into(),
            link_count: 1,
            is_link_or_reparse: false,
        }
    }
}
impl Platform for DirectoryAliasPlatform {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        Ok(self.info(path))
    }
    fn read_directory(
        &self,
        path: &Path,
        visitor: &mut dyn FnMut(DirectoryEvent) -> bool,
    ) -> io::Result<()> {
        self.entered.lock().unwrap().push(path.to_path_buf());
        let root_alias = self.identity == Some("root-directory");
        if root_alias && self.entered.lock().unwrap().len() > 2 {
            // Keep a broken traversal bounded if ancestor-cycle detection regresses.
            return Err(io::Error::other("Ancestor alias was traversed repeatedly"));
        }
        if path == self.root || root_alias {
            let aliases = if self.reverse {
                ["data-path", "firmlink-path"]
            } else {
                ["firmlink-path", "data-path"]
            };
            for alias in aliases {
                if !visitor(DirectoryEvent::Entry(ScanEntry {
                    kind_hint: None,
                    path: path.join(alias),
                    info: None,
                })) {
                    break;
                }
            }
        } else {
            let child = path.join("same-file");
            visitor(DirectoryEvent::Entry(ScanEntry {
                kind_hint: None,
                info: Some(Ok(self.info(&child))),
                path: child,
            }));
        }
        Ok(())
    }
    fn volumes(&self) -> io::Result<Vec<Volume>> {
        Ok(Vec::new())
    }
    fn trash(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("not used"))
    }
    fn reveal(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("not used"))
    }
    fn protected_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

#[test]
fn repeated_directory_identity_is_a_boundary_and_does_not_duplicate_bytes() {
    for reverse in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        let platform = DirectoryAliasPlatform {
            root: std::fs::canonicalize(fixture.path()).unwrap(),
            identity: Some("same-physical-directory"),
            reverse,
            entered: std::sync::Mutex::new(Vec::new()),
        };
        let mut last_progress = ScanProgress::default();
        let snapshot =
            ohmydisk_core::scan::scan(&platform.root, &platform, &false.into(), &mut |progress| {
                last_progress = progress;
            })
            .unwrap();
        assert_eq!(
            snapshot.nodes[0].logical_bytes, 7,
            "a directory alias must not duplicate its subtree"
        );
        assert_eq!(
            platform.entered.lock().unwrap().len(),
            2,
            "only root and one physical child directory are enumerated"
        );
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::File)
                .count(),
            1
        );
        let aliases = snapshot
            .nodes
            .iter()
            .filter(|node| node.identity == "same-physical-directory")
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 2);
        let duplicate = aliases.iter().find(|node| !node.complete).unwrap();
        assert_eq!(duplicate.logical_bytes, 0);
        assert_eq!(duplicate.allocated_bytes, Some(0));
        assert!(!snapshot.complete);
        assert_eq!(last_progress.files, 1);
        assert_eq!(last_progress.directories, 2);
        assert_eq!(last_progress.bytes, 7);
        assert!(snapshot.issues.iter().any(|issue| issue.node_id.as_deref()
            == Some(duplicate.id.as_str())
            && issue.code == "boundary_skipped"
            && issue.message.contains("directory identity")));
        assert_eq!(
            ohmydisk_core::cleanup::prepare_plan(
                &snapshot,
                std::slice::from_ref(&duplicate.id),
                CleanupMode::Trash,
                &[]
            )
            .unwrap_err()
            .code,
            "incomplete_subtree"
        );
    }
}

#[test]
fn distinct_or_unknown_directory_identities_are_not_collapsed() {
    for identity in [None, Some("")] {
        let fixture = tempfile::tempdir().unwrap();
        let platform = DirectoryAliasPlatform {
            root: std::fs::canonicalize(fixture.path()).unwrap(),
            identity,
            reverse: false,
            entered: std::sync::Mutex::new(Vec::new()),
        };
        let snapshot =
            ohmydisk_core::scan::scan(&platform.root, &platform, &false.into(), &mut |_| {})
                .unwrap();
        assert_eq!(snapshot.nodes[0].logical_bytes, 14);
        assert_eq!(platform.entered.lock().unwrap().len(), 3);
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::File)
                .count(),
            2
        );
        assert!(snapshot.complete);
        assert!(snapshot.issues.is_empty());
    }
}

#[test]
fn directory_alias_to_the_root_cannot_reenter_the_ancestor() {
    let fixture = tempfile::tempdir().unwrap();
    let platform = DirectoryAliasPlatform {
        root: std::fs::canonicalize(fixture.path()).unwrap(),
        identity: Some("root-directory"),
        reverse: false,
        entered: std::sync::Mutex::new(Vec::new()),
    };
    let snapshot =
        ohmydisk_core::scan::scan(&platform.root, &platform, &false.into(), &mut |_| {}).unwrap();
    assert_eq!(platform.entered.lock().unwrap().len(), 1);
    assert_eq!(snapshot.nodes.len(), 3);
    assert_eq!(snapshot.nodes[0].logical_bytes, 0);
    assert!(!snapshot.complete);
    assert_eq!(snapshot.issues.len(), 2);
    assert!(
        snapshot
            .issues
            .iter()
            .all(|issue| issue.code == "boundary_skipped")
    );
}

#[cfg(unix)]
#[test]
fn ordinary_hardlinks_keep_logical_bytes_per_entry() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("first"), b"seven!!").unwrap();
    std::fs::hard_link(fixture.path().join("first"), fixture.path().join("second")).unwrap();
    let snapshot = scanned(fixture.path());
    let files = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::File)
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].identity, files[1].identity);
    assert_eq!(files[0].link_count, 2);
    assert_eq!(snapshot.nodes[0].logical_bytes, 14);
    assert_eq!(snapshot.nodes[0].allocated_bytes, None);
    assert!(snapshot.complete);
}
