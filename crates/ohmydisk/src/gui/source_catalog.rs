//! Persistent overview roots. Scan snapshots remain owned by the local service.
use super::{ScanSummary, preferences::FolderSettings};
use ohmydisk_core::{NativePath, Volume};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::{Component, PathBuf};

fn normalized(path: &NativePath) -> Option<PathBuf> {
    let path = path.to_path().ok()?;
    if !path.is_absolute() {
        return None;
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        result.push(format!("{}:", char::from(drive.to_ascii_uppercase())))
                    }
                    _ => result.push(prefix.as_os_str()),
                }
            }
            _ => result.push(component.as_os_str()),
        }
    }
    // Do not collapse parent components across possible symlinks. Picker and
    // completed scan paths are canonicalized off the GUI thread before use.
    Some(result)
}

pub(super) fn path_equal(left: &NativePath, right: &NativePath) -> bool {
    match (normalized(left), normalized(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn clean(paths: Vec<NativePath>) -> Vec<NativePath> {
    let mut output = Vec::new();
    for path in paths {
        if normalized(&path).is_some() {
            // Keep the original native spelling for I/O (notably Windows
            // verbatim long paths); normalization is a comparison key only.
            let path = NativePath::new(&path.to_path().expect("validated native encoding"));
            if !output.iter().any(|other| path_equal(other, &path)) {
                output.push(path);
            }
        }
    }
    output
}

#[derive(Clone, Debug)]
pub(super) struct Catalog {
    settings: FolderSettings,
}

impl Catalog {
    pub fn new(settings: FolderSettings) -> Self {
        let removed = clean(settings.removed);
        let roots = clean(settings.roots)
            .into_iter()
            .filter(|root| !removed.iter().any(|path| path_equal(path, root)))
            .collect();
        Self {
            settings: FolderSettings { roots, removed },
        }
    }

    pub fn folders(&self) -> &[NativePath] {
        &self.settings.roots
    }
    pub fn settings(&self) -> FolderSettings {
        self.settings.clone()
    }

    pub fn register(&mut self, root: NativePath) -> bool {
        if normalized(&root).is_none() {
            return false;
        }
        let root = NativePath::new(&root.to_path().expect("validated native encoding"));
        let previous = self.settings.removed.len();
        self.settings
            .removed
            .retain(|path| !path_equal(path, &root));
        let mut changed = previous != self.settings.removed.len();
        if !self
            .settings
            .roots
            .iter()
            .any(|path| path_equal(path, &root))
        {
            self.settings.roots.push(root);
            changed = true;
        }
        changed
    }

    pub fn remove(&mut self, root: &NativePath) -> bool {
        let previous = self.settings.roots.len();
        self.settings.roots.retain(|path| !path_equal(path, root));
        if previous == self.settings.roots.len() {
            return false;
        }
        if !self
            .settings
            .removed
            .iter()
            .any(|path| path_equal(path, root))
        {
            self.settings.removed.push(root.clone());
        }
        true
    }

    pub fn reconcile(&mut self, scans: &[ScanSummary], sources: &[Volume]) -> bool {
        let mut changed = false;
        for scan in scans {
            if sources
                .iter()
                .any(|volume| path_equal(&volume.mount_path, &scan.root_path))
                || implicit_volume_root(&scan.root_path)
                || self
                    .settings
                    .removed
                    .iter()
                    .any(|path| path_equal(path, &scan.root_path))
            {
                continue;
            }
            changed |= self.register(scan.root_path.clone());
        }
        changed
    }
}

fn implicit_volume_root(root: &NativePath) -> bool {
    let Some(root) = normalized(root) else {
        return true;
    };
    if root.parent().is_none() {
        return true;
    }
    #[cfg(target_os = "macos")]
    if root == Path::new("/System/Volumes/Data") || root.parent() == Some(Path::new("/Volumes")) {
        return true;
    }
    false
}

fn compare(left: &ScanSummary, right: &ScanSummary) -> std::cmp::Ordering {
    left.started_at
        .unwrap_or(left.created_at)
        .cmp(&right.started_at.unwrap_or(right.created_at))
        .then(left.created_at.cmp(&right.created_at))
        .then(left.scan_id.cmp(&right.scan_id))
}

pub(super) fn latest<'a>(scans: &'a [ScanSummary], path: &NativePath) -> Option<&'a ScanSummary> {
    scans
        .iter()
        .filter(|scan| path_equal(&scan.root_path, path))
        .max_by(|left, right| compare(left, right))
}

pub(super) fn latest_viewable<'a>(
    scans: &'a [ScanSummary],
    path: &NativePath,
) -> Option<&'a ScanSummary> {
    scans
        .iter()
        .filter(|scan| scan.root_id.is_some() && path_equal(&scan.root_path, path))
        .max_by(|left, right| compare(left, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root(name: &str) -> NativePath {
        NativePath::new(&std::env::temp_dir().join("omd-catalog-fixture").join(name))
    }
    fn scan(
        id: &str,
        root: &NativePath,
        started: u64,
        finished: u64,
        viewable: bool,
    ) -> ScanSummary {
        serde_json::from_value(serde_json::json!({"scan_id":id,"root_path":root,"root_id":viewable.then_some("root"),"started_at":started,"created_at":finished,"state":if viewable {"completed"} else {"failed"}})).unwrap()
    }

    #[test]
    fn duplicate_paths_and_removed_roots_survive_reconciliation_and_readdition() {
        let folder = root("folder");
        let mut catalog = Catalog::new(FolderSettings::default());
        assert!(catalog.register(folder.clone()));
        let alias = NativePath::new(&folder.to_path().unwrap().join("."));
        assert!(!catalog.register(alias));
        assert_eq!(catalog.folders().len(), 1);
        assert!(catalog.remove(&folder));
        let mut reopened = Catalog::new(catalog.settings());
        assert!(!reopened.reconcile(&[scan("old", &folder, 1, 2, true)], &[]));
        assert!(reopened.folders().is_empty());
        assert!(reopened.register(folder));
        assert_eq!(reopened.folders().len(), 1);
        assert!(reopened.settings().removed.is_empty());
    }

    #[test]
    fn sources_and_repeated_scan_roots_produce_one_custom_entry() {
        let folder = root("folder");
        let disk = root("disk");
        let sources = vec![Volume {
            id: "disk".into(),
            name: "disk".into(),
            mount_path: disk.clone(),
            total_bytes: 10,
            available_bytes: 5,
            space_observation: None,
        }];
        let scans = [
            scan("1", &folder, 1, 2, true),
            scan("2", &folder, 3, 4, true),
            scan("disk", &disk, 2, 3, true),
        ];
        let mut catalog = Catalog::new(FolderSettings::default());
        assert!(catalog.reconcile(&scans, &sources));
        assert_eq!(catalog.folders(), &[folder]);
        assert!(!catalog.reconcile(&scans, &sources));
    }

    #[test]
    fn latest_attempt_and_last_viewable_snapshot_are_independent() {
        let folder = root("folder");
        let scans = [
            scan("old-slow", &folder, 1, 100, true),
            scan("new-completed", &folder, 2, 10, true),
            scan("retry-failed", &folder, 3, 3, false),
        ];
        assert_eq!(latest(&scans, &folder).unwrap().scan_id, "retry-failed");
        assert_eq!(
            latest_viewable(&scans, &folder).unwrap().scan_id,
            "new-completed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_path_identity_is_not_lossy_display_text() {
        use std::os::unix::ffi::OsStringExt;
        let base = std::env::temp_dir().join("omd-native-root");
        let a = NativePath::new(&base.join(std::ffi::OsString::from_vec(vec![0xff])));
        let b = NativePath::new(&base.join(std::ffi::OsString::from_vec(vec![0xfe])));
        assert_eq!(a.display, b.display);
        assert!(!path_equal(&a, &b));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_drive_prefix_matches_without_changing_the_path_used_for_io() {
        let original = NativePath::new(std::path::Path::new(r"\\?\C:\folder"));
        let plain = NativePath::new(std::path::Path::new(r"c:\folder"));
        assert!(path_equal(&original, &plain));
        let mut catalog = Catalog::new(FolderSettings::default());
        assert!(catalog.register(original.clone()));
        assert!(!catalog.register(plain));
        assert_eq!(catalog.folders(), &[original]);
    }
}
