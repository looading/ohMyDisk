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
        let (identity, volume, allocated, nlink) = (
            format!("{}:{:?}", p.display(), m.created()),
            "test".into(),
            None,
            1,
        );
        Ok(FileInfo {
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
