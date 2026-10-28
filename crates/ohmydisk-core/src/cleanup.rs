use crate::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub fn prepare_plan(
    snapshot: &Snapshot,
    node_ids: &[String],
    mode: CleanupMode,
    protected: &[PathBuf],
) -> Result<CleanupPlan, AppError> {
    if snapshot.cancelled {
        return Err(AppError::new(
            "scan_not_ready",
            "Cancelled scan cannot authorize deletion",
        ));
    }
    if node_ids.is_empty() || node_ids.len() > 500 {
        return Err(AppError::new(
            "invalid_selection",
            "Select between 1 and 500 nodes",
        ));
    }
    let by_id = snapshot
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<HashMap<_, _>>();
    let requested = node_ids.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut selected = Vec::new();
    for id in &requested {
        let n = *by_id
            .get(id)
            .ok_or_else(|| AppError::new("not_found", "Selected node no longer exists"))?;
        let path = n.path.to_path()?;
        if n.id == snapshot.root_id
            || protected.iter().any(|p| same_path(p, &path))
            || path.parent().is_none()
        {
            return Err(AppError::new(
                "path_not_allowed",
                format!("Protected target: {}", path.display()),
            ));
        }
        if !n.complete
            || n.is_link_or_reparse
            || !matches!(n.kind, NodeKind::File | NodeKind::Directory)
        {
            return Err(AppError::new(
                "incomplete_subtree",
                format!(
                    "Cannot clean incomplete or linked target: {}",
                    path.display()
                ),
            ));
        }
        if protected.iter().any(|p| p.starts_with(&path)) {
            return Err(AppError::new(
                "path_not_allowed",
                "Target contains a protected path",
            ));
        }
        let mut parent = n.parent_id.as_deref();
        let mut covered = false;
        while let Some(p) = parent {
            if requested.contains(p) {
                covered = true;
                break;
            }
            parent = by_id.get(p).and_then(|n| n.parent_id.as_deref());
        }
        if !covered {
            selected.push(n);
        }
    }
    selected.sort_by(|a, b| a.path.encoded.cmp(&b.path.encoded));
    let mut targets = vec![];
    for node in selected {
        let mut members = snapshot
            .nodes
            .iter()
            .filter(|n| {
                n.path
                    .to_path()
                    .is_ok_and(|p| p.starts_with(node.path.to_path().unwrap_or_default()))
            })
            .cloned()
            .collect::<Vec<_>>();
        if members.iter().any(|n| !n.complete || n.is_link_or_reparse) {
            return Err(AppError::new(
                "incomplete_subtree",
                "A descendant is incomplete or linked",
            ));
        }
        members.sort_by(|a, b| a.path.encoded.cmp(&b.path.encoded));
        targets.push(PlanTarget {
            node: node.clone(),
            members,
        });
    }
    let created_at = now_ms();
    let mut p = CleanupPlan {
        id: new_id(),
        scan_id: snapshot.scan_id.clone(),
        snapshot_id: snapshot.id.clone(),
        hash: String::new(),
        mode,
        created_at,
        expires_at: created_at + 600_000,
        logical_bytes: targets.iter().map(|t| t.node.logical_bytes).sum(),
        targets,
    };
    rehash_plan(&mut p)?;
    Ok(p)
}

pub fn rehash_plan(plan: &mut CleanupPlan) -> Result<(), AppError> {
    plan.hash.clear();
    plan.hash = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&plan).map_err(|e| AppError::new("serialization", e.to_string()))?
        )
    );
    Ok(())
}

/// Validate both targets within protected trees and targets containing them.
pub fn validate_plan_scope(
    plan: &CleanupPlan,
    protected_subtrees: &[PathBuf],
    platform: &dyn Platform,
) -> Result<(), AppError> {
    // Compare native file identities, not path spellings. APFS/NTFS can resolve
    // case aliases and normalization variants that lexical comparison misses.
    let identity = |path: &Path| {
        platform
            .metadata(path)
            .ok()
            .filter(|m| !m.identity.is_empty())
            .map(|m| (m.volume_id, m.identity))
    };
    let mut tree_roots = HashSet::new();
    let mut forbidden_target_ids = HashSet::new();
    for protected in protected_subtrees {
        if let Some(id) = identity(protected) {
            tree_roots.insert(id);
        }
        for ancestor in protected.ancestors() {
            if let Some(id) = identity(ancestor) {
                forbidden_target_ids.insert(id);
            }
        }
    }
    for protected in platform.protected_paths() {
        for ancestor in protected.ancestors() {
            if let Some(id) = identity(ancestor) {
                forbidden_target_ids.insert(id);
            }
        }
    }
    for target in &plan.targets {
        let path = target.node.path.to_path()?;
        let target_id = identity(&path);
        let intersects = target_id
            .as_ref()
            .is_some_and(|id| forbidden_target_ids.contains(id))
            || path
                .ancestors()
                .filter_map(identity)
                .any(|id| tree_roots.contains(&id));
        if intersects
            || protected_subtrees
                .iter()
                .any(|p| path_is_within(&path, p) || path_is_within(p, &path))
        {
            return Err(AppError::new(
                "path_not_allowed",
                format!("Target intersects a protected tree: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn path_is_within(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path
            .components()
            .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>();
        let base = base
            .components()
            .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>();
        path.starts_with(&base)
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn unchanged(
    n: &Node,
    platform: &dyn Platform,
    check_directory_time: bool,
) -> Result<(), AppError> {
    let p = n.path.to_path()?;
    let m = platform
        .metadata(&p)
        .map_err(|e| AppError::new("stale_plan", e.to_string()))?;
    if m.identity.is_empty()
        || m.identity != n.identity
        || m.kind != n.kind
        || m.is_link_or_reparse
        || m.volume_id != n.volume_id
        || (n.kind == NodeKind::File
            && (m.logical_bytes != n.logical_bytes || m.modified_ns != n.modified_ns))
        || (check_directory_time && n.kind == NodeKind::Directory && m.modified_ns != n.modified_ns)
    {
        return Err(AppError::new(
            "stale_plan",
            format!("Target changed: {}", p.display()),
        ));
    }
    // Check every ancestor against unexpected link traversal. Stable identity of the
    // selected member is also required; the filesystem remains concurrently mutable.
    let mut ancestor = p.parent();
    while let Some(a) = ancestor {
        let m = platform
            .metadata(a)
            .map_err(|e| AppError::new("stale_plan", e.to_string()))?;
        if m.is_link_or_reparse {
            return Err(AppError::new("stale_plan", "Ancestor became a link"));
        }
        ancestor = a.parent();
    }
    Ok(())
}
fn validate_target(target: &PlanTarget, platform: &dyn Platform) -> Result<(), AppError> {
    let expected = target
        .members
        .iter()
        .map(|n| n.path.encoded.as_str())
        .collect::<HashSet<_>>();
    for n in &target.members {
        unchanged(n, platform, true)?;
        if n.kind == NodeKind::Directory {
            for child in std::fs::read_dir(n.path.to_path()?)
                .map_err(|e| AppError::new("stale_plan", e.to_string()))?
            {
                let p = child
                    .map_err(|e| AppError::new("stale_plan", e.to_string()))?
                    .path();
                if !expected.contains(NativePath::new(&p).encoded.as_str()) {
                    return Err(AppError::new("stale_plan", "Directory has new members"));
                }
            }
        }
    }
    Ok(())
}
fn item(n: &Node, status: ItemStatus, error: Option<AppError>) -> ItemResult {
    ItemResult {
        node_id: n.id.clone(),
        path: n.path.display.clone(),
        status,
        code: error.as_ref().map(|e| e.code.clone()),
        message: error.map(|e| e.message),
    }
}

pub fn execute_plan(
    plan: &CleanupPlan,
    platform: &dyn Platform,
    cancelled: &AtomicBool,
    record: &mut dyn FnMut(ItemResult) -> Result<(), AppError>,
) -> Result<Vec<ItemResult>, AppError> {
    if now_ms() > plan.expires_at {
        return Err(AppError::new("plan_expired", "Prepare a new cleanup plan"));
    }
    let mut result = vec![];
    for target in &plan.targets {
        if cancelled.load(Ordering::Relaxed) {
            let r = item(&target.node, ItemStatus::NotStarted, None);
            record(r.clone())?;
            result.push(r);
            continue;
        }
        if let Err(e) = validate_target(target, platform) {
            let r = item(&target.node, ItemStatus::Skipped, Some(e));
            record(r.clone())?;
            result.push(r);
            continue;
        }
        if cancelled.load(Ordering::Relaxed) {
            let r = item(&target.node, ItemStatus::NotStarted, None);
            record(r.clone())?;
            result.push(r);
            continue;
        }
        match plan.mode {
            CleanupMode::Trash => {
                let n = &target.node;
                record(item(n, ItemStatus::Started, None))?;
                if cancelled.load(Ordering::Relaxed) {
                    let r = item(n, ItemStatus::NotStarted, None);
                    record(r.clone())?;
                    result.push(r);
                    continue;
                }
                let r = match platform.trash(&n.path.to_path()?) {
                    Ok(()) => item(n, ItemStatus::Succeeded, None),
                    Err(e) => item(n, ItemStatus::Failed, Some(e.into())),
                };
                record(r.clone())?;
                result.push(r);
            }
            CleanupMode::Permanent => {
                let mut members = target.members.iter().collect::<Vec<_>>();
                members.sort_by_key(|n| {
                    std::cmp::Reverse(
                        n.path
                            .to_path()
                            .map(|p| p.components().count())
                            .unwrap_or(0),
                    )
                });
                for n in members {
                    if cancelled.load(Ordering::Relaxed) {
                        let r = item(n, ItemStatus::NotStarted, None);
                        record(r.clone())?;
                        result.push(r);
                        continue;
                    }
                    if let Err(e) = unchanged(n, platform, false) {
                        let r = item(n, ItemStatus::Skipped, Some(e));
                        record(r.clone())?;
                        result.push(r);
                        continue;
                    }
                    record(item(n, ItemStatus::Started, None))?;
                    if cancelled.load(Ordering::Relaxed) {
                        let r = item(n, ItemStatus::NotStarted, None);
                        record(r.clone())?;
                        result.push(r);
                        continue;
                    }
                    let p = n.path.to_path()?;
                    let deletion = if n.kind == NodeKind::Directory {
                        std::fs::remove_dir(p)
                    } else {
                        std::fs::remove_file(p)
                    };
                    let r = match deletion {
                        Ok(()) => item(n, ItemStatus::Succeeded, None),
                        Err(e) => item(n, ItemStatus::Failed, Some(e.into())),
                    };
                    record(r.clone())?;
                    result.push(r);
                }
            }
        }
    }
    Ok(result)
}
