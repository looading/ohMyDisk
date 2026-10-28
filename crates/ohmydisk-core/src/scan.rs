use crate::*;
use std::{
    collections::HashMap,
    path::Path,
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
    let volume = original.volume_id;
    let mut snapshot = Snapshot {
        id: new_id(),
        scan_id: new_id(),
        root_id: "0".into(),
        root_path: NativePath::new(&root),
        created_at: now_ms(),
        complete: true,
        cancelled: false,
        nodes: vec![],
        issues: vec![],
    };
    let mut pending = vec![(root, None::<usize>)];
    let mut counts = ScanProgress {
        phase: "scanning".into(),
        ..Default::default()
    };
    let mut seen = 0usize;
    while let Some((path, parent)) = pending.pop() {
        if cancelled.load(Ordering::Relaxed) {
            snapshot.cancelled = true;
            snapshot.complete = false;
            break;
        }
        let info = match platform.metadata(&path) {
            Ok(i) => i,
            Err(e) => {
                if let Some(p) = parent {
                    snapshot.nodes[p].complete = false;
                }
                snapshot.issues.push(ScanIssue {
                    node_id: parent.map(|p| snapshot.nodes[p].id.clone()),
                    path: path.display().to_string(),
                    code: if e.kind() == std::io::ErrorKind::PermissionDenied {
                        "permission_denied"
                    } else {
                        "io_error"
                    }
                    .into(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        let idx = snapshot.nodes.len();
        let id = idx.to_string();
        let boundary =
            info.is_link_or_reparse || info.volume_id != volume || info.kind == NodeKind::Other;
        let directory = info.kind == NodeKind::Directory && !boundary;
        snapshot.nodes.push(Node {
            id: id.clone(),
            parent_id: parent.map(|p| snapshot.nodes[p].id.clone()),
            name: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            path: NativePath::new(&path),
            kind: info.kind,
            logical_bytes: if directory { 0 } else { info.logical_bytes },
            allocated_bytes: if directory {
                Some(0)
            } else {
                info.allocated_bytes
            },
            modified_ns: info.modified_ns,
            identity: info.identity,
            volume_id: info.volume_id,
            link_count: info.link_count,
            complete: !boundary,
            is_link_or_reparse: info.is_link_or_reparse,
        });
        if boundary {
            snapshot.issues.push(ScanIssue {
                node_id: Some(id),
                path: path.display().to_string(),
                code: "boundary_skipped".into(),
                message:
                    "Link, reparse point, special file or different filesystem was not traversed"
                        .into(),
            });
        }
        if directory {
            counts.directories += 1;
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    let mut children = vec![];
                    for entry in entries {
                        match entry {
                            Ok(entry) => children.push(entry.path()),
                            Err(e) => {
                                snapshot.nodes[idx].complete = false;
                                snapshot.issues.push(ScanIssue {
                                    node_id: Some(idx.to_string()),
                                    path: path.display().to_string(),
                                    code: "read_directory".into(),
                                    message: e.to_string(),
                                });
                            }
                        }
                    }
                    children.sort();
                    pending.extend(children.into_iter().rev().map(|p| (p, Some(idx))));
                }
                Err(e) => {
                    snapshot.nodes[idx].complete = false;
                    snapshot.issues.push(ScanIssue {
                        node_id: Some(idx.to_string()),
                        path: path.display().to_string(),
                        code: "read_directory".into(),
                        message: e.to_string(),
                    });
                }
            }
        } else {
            counts.files += 1;
            counts.bytes = counts.bytes.saturating_add(info.logical_bytes);
        }
        seen += 1;
        if seen % 128 == 0 {
            counts.current_path = path.display().to_string();
            progress(counts.clone());
        }
    }
    let positions = snapshot
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect::<HashMap<_, _>>();
    for i in (0..snapshot.nodes.len()).rev() {
        if let Some(p) = snapshot.nodes[i]
            .parent_id
            .as_ref()
            .and_then(|p| positions.get(p))
            .copied()
        {
            let child = snapshot.nodes[i].clone();
            let parent = &mut snapshot.nodes[p];
            parent.logical_bytes = parent.logical_bytes.saturating_add(child.logical_bytes);
            parent.allocated_bytes = if child.kind != NodeKind::Directory && child.link_count > 1 {
                None
            } else {
                parent
                    .allocated_bytes
                    .zip(child.allocated_bytes)
                    .and_then(|(a, b)| a.checked_add(b))
            };
            parent.complete &= child.complete;
        }
    }
    if snapshot.cancelled {
        for n in &mut snapshot.nodes {
            if n.kind == NodeKind::Directory {
                n.complete = false;
            }
        }
    }
    snapshot.complete &=
        snapshot.nodes.first().is_some_and(|n| n.complete) && snapshot.issues.is_empty();
    counts.phase = if snapshot.cancelled {
        "cancelled"
    } else {
        "completed"
    }
    .into();
    progress(counts);
    Ok(snapshot)
}
