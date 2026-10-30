use crate::space::{file_space, reconcile_space};
use crate::{
    AppError, AttributionProvider, DirectoryEvent, FileInfo, NativePath, Node, NodeKind, NodeSpace,
    Platform, ScanIssue, ScanProgress, Snapshot, SpaceObservation, new_id, now_ms,
};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// The serial scanner remains available as a reference and for synchronous callers.
///
/// # Errors
/// Returns an error for an invalid root or a root that cannot be inspected.
/// Errors in discovered entries are retained as incomplete snapshot issues.
pub fn scan(
    root: &Path,
    platform: &dyn Platform,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<Snapshot, AppError> {
    let (root, volume, before) = prepare_root(root, platform)?;
    let mut builder = ScanBuilder::new(&root, volume, before);
    let mut pending = vec![(root.clone(), None)];
    while let Some((path, parent)) = pending.pop() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let old_len = builder.snapshot.nodes.len();
        let admitted = builder.add(&path, parent, platform.scan_metadata(&path));
        if builder.snapshot.nodes.len() != old_len
            && builder.snapshot.nodes.len().is_multiple_of(128)
        {
            progress(builder.counts.clone());
        }
        let Some(index) = admitted else {
            continue;
        };
        let result = platform.read_directory(&path, &mut |event| {
            if cancelled.load(Ordering::Relaxed) {
                return false;
            }
            match event {
                DirectoryEvent::Entry(entry) => {
                    // Only ordinary-file metadata is reused. Directories are checked
                    // again immediately before traversal, including native bulk entries.
                    let old_len = builder.snapshot.nodes.len();
                    match entry.info {
                        Some(Ok(info)) if info.kind != NodeKind::Directory => {
                            builder.add(&entry.path, Some(index), Ok(info));
                        }
                        Some(Err(error)) => {
                            builder.add(&entry.path, Some(index), Err(error));
                        }
                        _ => pending.push((entry.path, Some(index))),
                    }
                    if builder.snapshot.nodes.len() != old_len
                        && builder.snapshot.nodes.len().is_multiple_of(128)
                    {
                        progress(builder.counts.clone());
                    }
                }
                DirectoryEvent::Error(error) => builder.directory_error(index, &error),
            }
            !cancelled.load(Ordering::Relaxed)
        });
        if let Err(error) = result {
            builder.directory_error(index, &error);
        }
    }
    let after = matching_observation(platform, &root, &builder.volume);
    let (snapshot, counts) = builder.finish(cancelled, after);
    progress(counts);
    Ok(snapshot)
}

pub(super) fn matching_observation(
    platform: &dyn Platform,
    root: &Path,
    volume: &str,
) -> Option<SpaceObservation> {
    platform
        .space_observation(root)
        .ok()
        .flatten()
        .filter(|observation| observation.volume_id == volume)
}

pub(super) fn prepare_root(
    root: &Path,
    platform: &dyn Platform,
) -> Result<(PathBuf, String, Option<SpaceObservation>), AppError> {
    if !root.is_absolute() {
        return Err(AppError::new(
            "path_not_allowed",
            "Scan root must be absolute",
        ));
    }
    let original = platform.scan_metadata(root)?;
    if original.kind != NodeKind::Directory || original.is_link_or_reparse {
        return Err(AppError::new(
            "path_not_allowed",
            "Choose a real local directory",
        ));
    }
    let root = std::fs::canonicalize(root)?;
    let before = matching_observation(platform, &root, &original.volume_id);
    Ok((root, original.volume_id, before))
}

/// One owner applies all discovery events. Nodes are appended after their parents,
/// even when independent directory workers finish in a different order.
pub(super) struct ScanBuilder {
    pub(super) snapshot: Snapshot,
    pub(super) volume: String,
    pub(super) counts: ScanProgress,
    before: Option<SpaceObservation>,
    provider: AttributionProvider,
    visited_directories: HashSet<String>,
    parents: Vec<usize>,
}

impl ScanBuilder {
    pub(super) fn new(root: &Path, volume: String, before: Option<SpaceObservation>) -> Self {
        let provider = before
            .as_ref()
            .map_or(AttributionProvider::Unsupported, |observation| {
                observation.provider
            });
        Self {
            snapshot: Snapshot {
                space_accounting: None,
                id: new_id(),
                scan_id: new_id(),
                root_id: "0".into(),
                root_path: NativePath::new(root),
                created_at: now_ms(),
                complete: true,
                cancelled: false,
                nodes: Vec::new(),
                issues: Vec::new(),
            },
            volume,
            before,
            provider,
            visited_directories: HashSet::new(),
            parents: Vec::new(),
            counts: ScanProgress {
                phase: "scanning".into(),
                ..Default::default()
            },
        }
    }

    /// Admit a fresh metadata result. Only an admitted directory may be read.
    pub(super) fn add(
        &mut self,
        path: &Path,
        parent: Option<usize>,
        info: io::Result<FileInfo>,
    ) -> Option<usize> {
        let info = match info {
            Ok(info) => info,
            Err(error) => {
                if let Some(parent) = parent {
                    let node = &mut self.snapshot.nodes[parent];
                    node.complete = false;
                    if let Some(space) = &mut node.space_usage {
                        space.unmeasured_entries = space.unmeasured_entries.saturating_add(1);
                    }
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
                return None;
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
        let space_usage = if directory {
            NodeSpace::default()
        } else if boundary || duplicate_directory {
            NodeSpace {
                unmeasured_entries: 1,
                ..Default::default()
            }
        } else {
            file_space(&info, self.provider)
        };
        self.parents.push(parent.unwrap_or(usize::MAX));
        self.snapshot.nodes.push(Node {
            space_usage: Some(space_usage),
            id: index.to_string(),
            parent_id: parent.map(|p| self.snapshot.nodes[p].id.clone()),
            name: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            path: NativePath::new(path),
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
                node_id: Some(index.to_string()), path: self.snapshot.nodes[index].path.display.clone(),
                code: "boundary_skipped".into(),
                message: if duplicate_directory {
                    "The same directory identity was already visited; this alias subtree was not traversed or counted"
                } else { "Link, reparse point, special file or different filesystem was not traversed" }.into(),
            });
        }
        if directory {
            self.counts.directories += 1;
        } else if !duplicate_directory {
            self.counts.files += 1;
            self.counts.bytes = self.counts.bytes.saturating_add(info.logical_bytes);
        }
        self.counts
            .current_path
            .clone_from(&self.snapshot.nodes[index].path.display);
        directory.then_some(index)
    }

    pub(super) fn directory_error(&mut self, parent: usize, error: &io::Error) {
        let node = &mut self.snapshot.nodes[parent];
        node.complete = false;
        if let Some(space) = &mut node.space_usage {
            space.unmeasured_entries = space.unmeasured_entries.saturating_add(1);
        }
        self.snapshot.issues.push(ScanIssue {
            node_id: Some(node.id.clone()),
            path: node.path.display.clone(),
            code: "read_directory".into(),
            message: error.to_string(),
        });
    }

    pub(super) fn finish(
        mut self,
        cancelled: &AtomicBool,
        after: Option<SpaceObservation>,
    ) -> (Snapshot, ScanProgress) {
        self.snapshot.cancelled = cancelled.load(Ordering::Relaxed);
        self.snapshot.complete &= !self.snapshot.cancelled;
        if self.snapshot.cancelled {
            mark_cancelled_directories(&mut self.snapshot.nodes);
        }
        for index in (0..self.snapshot.nodes.len()).rev() {
            let parent = self.parents[index];
            if parent == usize::MAX {
                continue;
            }
            let child = &self.snapshot.nodes[index];
            let (logical, allocated, kind, links, complete, space) = (
                child.logical_bytes,
                child.allocated_bytes,
                child.kind,
                child.link_count,
                child.complete,
                child.space_usage.clone().unwrap_or_default(),
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
            if let Some(usage) = &mut parent.space_usage {
                usage.attributed_bytes = usage
                    .attributed_bytes
                    .saturating_add(space.attributed_bytes);
                usage.measured_entries = usage
                    .measured_entries
                    .saturating_add(space.measured_entries);
                usage.unmeasured_entries = usage
                    .unmeasured_entries
                    .saturating_add(space.unmeasured_entries);
            }
        }
        self.snapshot.cancelled |= cancelled.load(Ordering::Relaxed);
        self.snapshot.complete &= !self.snapshot.cancelled;
        if self.snapshot.cancelled {
            // A cancellation can arrive during aggregation. In that case every
            // directory, including ancestors already reduced, still carries at
            // least one unknown-subtree marker instead of a measured zero.
            mark_cancelled_directories(&mut self.snapshot.nodes);
        }
        self.snapshot.complete &= self.snapshot.nodes.first().is_some_and(|n| n.complete)
            && self.snapshot.issues.is_empty();
        if let Some(before) = self.before {
            let usage = self
                .snapshot
                .nodes
                .first()
                .and_then(|node| node.space_usage.as_ref())
                .cloned()
                .unwrap_or_default();
            self.snapshot.space_accounting = Some(reconcile_space(
                before,
                after,
                &usage,
                self.snapshot.complete,
            ));
        }
        self.counts.phase = if self.snapshot.cancelled {
            "cancelled"
        } else {
            "completed"
        }
        .into();
        (self.snapshot, self.counts)
    }
}

fn mark_cancelled_directories(nodes: &mut [Node]) {
    for node in nodes {
        if node.kind == NodeKind::Directory {
            node.complete = false;
            if let Some(space) = &mut node.space_usage {
                space.unmeasured_entries = space.unmeasured_entries.max(1);
            }
        }
    }
}
