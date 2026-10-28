use crate::*;
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub fn scan(
    root: &Path,
    platform: &dyn Platform,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<Snapshot, AppError> {
    if !root.is_absolute() {
        return Err(AppError::new(
            "path_not_allowed",
            "Scan root must be absolute",
        ));
    }
    let original = platform.metadata(root)?;
    if original.kind != NodeKind::Directory || original.is_link_or_reparse {
        return Err(AppError::new(
            "path_not_allowed",
            "Choose a real local directory",
        ));
    }
    let root = std::fs::canonicalize(root)?;
    let snapshot = Snapshot {
        id: new_id(),
        scan_id: new_id(),
        root_id: "0".into(),
        root_path: NativePath::new(&root),
        created_at: now_ms(),
        complete: true,
        cancelled: false,
        nodes: Vec::new(),
        issues: Vec::new(),
    };
    let mut builder = ScanBuilder {
        snapshot,
        platform,
        cancelled,
        progress,
        volume: original.volume_id,
        visited_directories: HashSet::new(),
        parents: Vec::new(),
        pending: vec![Pending::Metadata(root, None)],
        counts: ScanProgress {
            phase: "scanning".into(),
            ..Default::default()
        },
    };
    while let Some(task) = builder.pending.pop() {
        if builder.stop_requested() {
            break;
        }
        let (path, parent) = match task {
            Pending::Metadata(path, parent) => {
                builder.add(path, parent, None);
                continue;
            }
            Pending::Directory(path, parent) => (path, parent),
        };
        let result = platform.read_directory(&path, &mut |event| {
            if builder.stop_requested() {
                return false;
            }
            match event {
                DirectoryEvent::Entry(entry) => {
                    // Directory metadata must be obtained when it is about to
                    // be traversed, not while it waits behind sibling entries.
                    if entry.info.as_ref().is_none_or(|info| {
                        info.as_ref().is_ok_and(|i| i.kind == NodeKind::Directory)
                    }) {
                        builder
                            .pending
                            .push(Pending::Metadata(entry.path, Some(parent)));
                    } else {
                        builder.add(entry.path, Some(parent), entry.info);
                    }
                }
                DirectoryEvent::Error(error) => builder.directory_error(parent, error),
            }
            !builder.stop_requested()
        });
        if let Err(error) = result {
            builder.directory_error(parent, error);
        }
    }
    Ok(builder.finish())
}

enum Pending {
    Metadata(PathBuf, Option<usize>),
    Directory(PathBuf, usize),
}

struct ScanBuilder<'a, 'p> {
    snapshot: Snapshot,
    platform: &'a dyn Platform,
    cancelled: &'a AtomicBool,
    progress: &'p mut dyn FnMut(ScanProgress),
    volume: String,
    // Traversed directories all belong to `volume`, so identity keys need no
    // second copy of that same volume ID. Empty identities cannot prove aliasing.
    visited_directories: HashSet<String>,
    // Parent indices precede their children; usize::MAX is the root sentinel.
    parents: Vec<usize>,
    // Native regular-file metadata is consumed immediately. Unresolved paths
    // retain only their PathBuf/parent until a just-in-time metadata check.
    pending: Vec<Pending>,
    counts: ScanProgress,
}

impl ScanBuilder<'_, '_> {
    fn stop_requested(&mut self) -> bool {
        if self.cancelled.load(Ordering::Relaxed) {
            self.snapshot.cancelled = true;
            self.snapshot.complete = false;
            true
        } else {
            false
        }
    }

    fn add(&mut self, path: PathBuf, parent: Option<usize>, info: Option<io::Result<FileInfo>>) {
        let info = match info.unwrap_or_else(|| self.platform.metadata(&path)) {
            Ok(info) => info,
            Err(error) => {
                if let Some(parent) = parent {
                    self.snapshot.nodes[parent].complete = false;
                }
                self.snapshot.issues.push(ScanIssue {
                    node_id: parent.map(|p| self.snapshot.nodes[p].id.clone()),
                    path: path.display().to_string(),
                    code: if error.kind() == io::ErrorKind::PermissionDenied {
                        "permission_denied"
                    } else {
                        "io_error"
                    }
                    .into(),
                    message: error.to_string(),
                });
                return;
            }
        };
        let index = self.snapshot.nodes.len();
        let boundary = info.is_link_or_reparse
            || info.volume_id != self.volume
            || info.kind == NodeKind::Other;
        let duplicate_directory = !boundary
            && info.kind == NodeKind::Directory
            && !info.identity.is_empty()
            && !self.visited_directories.insert(info.identity.clone());
        let directory = info.kind == NodeKind::Directory && !boundary && !duplicate_directory;
        let excluded_directory_bytes = directory || duplicate_directory;
        self.parents.push(parent.unwrap_or(usize::MAX));
        self.snapshot.nodes.push(Node {
            id: index.to_string(),
            parent_id: parent.map(|p| self.snapshot.nodes[p].id.clone()),
            name: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            path: NativePath::new(&path),
            kind: info.kind,
            logical_bytes: if excluded_directory_bytes {
                0
            } else {
                info.logical_bytes
            },
            allocated_bytes: if excluded_directory_bytes {
                Some(0)
            } else {
                info.allocated_bytes
            },
            modified_ns: info.modified_ns,
            identity: info.identity,
            volume_id: info.volume_id,
            link_count: info.link_count,
            complete: !boundary && !duplicate_directory,
            is_link_or_reparse: info.is_link_or_reparse,
        });
        if boundary || duplicate_directory {
            self.snapshot.issues.push(ScanIssue {
                node_id: Some(index.to_string()),
                path: self.snapshot.nodes[index].path.display.clone(),
                code: "boundary_skipped".into(),
                message: if duplicate_directory {
                    "The same directory identity was already visited; this alias subtree was not traversed or counted"
                } else {
                    "Link, reparse point, special file or different filesystem was not traversed"
                }.into(),
            });
        }
        if directory {
            self.counts.directories += 1;
        } else if !duplicate_directory {
            self.counts.files += 1;
            self.counts.bytes = self.counts.bytes.saturating_add(info.logical_bytes);
        }
        if self.snapshot.nodes.len().is_multiple_of(128) {
            self.counts
                .current_path
                .clone_from(&self.snapshot.nodes[index].path.display);
            (self.progress)(self.counts.clone());
        }
        if directory {
            self.pending.push(Pending::Directory(path, index));
        }
    }

    fn directory_error(&mut self, parent: usize, error: io::Error) {
        let node = &mut self.snapshot.nodes[parent];
        node.complete = false;
        self.snapshot.issues.push(ScanIssue {
            node_id: Some(node.id.clone()),
            path: node.path.display.clone(),
            code: "read_directory".into(),
            message: error.to_string(),
        });
    }

    fn finish(mut self) -> Snapshot {
        self.stop_requested();
        for index in (0..self.snapshot.nodes.len()).rev() {
            let parent = self.parents[index];
            if parent == usize::MAX {
                continue;
            }
            let child = &self.snapshot.nodes[index];
            let (logical, allocated, kind, links, complete) = (
                child.logical_bytes,
                child.allocated_bytes,
                child.kind,
                child.link_count,
                child.complete,
            );
            let parent = &mut self.snapshot.nodes[parent];
            parent.logical_bytes = parent.logical_bytes.saturating_add(logical);
            parent.allocated_bytes = if kind != NodeKind::Directory && links > 1 {
                None
            } else {
                parent
                    .allocated_bytes
                    .zip(allocated)
                    .and_then(|(a, b)| a.checked_add(b))
            };
            parent.complete &= complete;
        }
        self.stop_requested();
        if self.snapshot.cancelled {
            for node in &mut self.snapshot.nodes {
                if node.kind == NodeKind::Directory {
                    node.complete = false;
                }
            }
        }
        self.snapshot.complete &= self.snapshot.nodes.first().is_some_and(|n| n.complete)
            && self.snapshot.issues.is_empty();
        self.counts.phase = if self.snapshot.cancelled {
            "cancelled"
        } else {
            "completed"
        }
        .into();
        (self.progress)(self.counts);
        self.snapshot
    }
}
