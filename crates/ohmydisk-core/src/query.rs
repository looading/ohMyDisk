use crate::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub fn query(snapshot: &Snapshot, query: &Query) -> Result<serde_json::Value, AppError> {
    let scope = query.scope_node_id.as_deref().unwrap_or(&snapshot.root_id);
    if !snapshot.nodes.iter().any(|n| n.id == scope) {
        return Err(AppError::new("not_found", "Scope node not found"));
    }
    let by_id = snapshot
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<HashMap<_, _>>();
    let mut criteria = query.clone();
    criteria.cursor = None;
    let signature = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(snapshot.id.as_str(), &criteria))
                .map_err(|e| AppError::new("serialization", e.to_string()))?
        )
    );
    let offset = match &query.cursor {
        None => 0,
        Some(c) => {
            let raw = URL_SAFE_NO_PAD
                .decode(c)
                .map_err(|_| AppError::new("invalid_cursor", "Invalid cursor"))?;
            let (sig, offset): (String, usize) = serde_json::from_slice(&raw)
                .map_err(|_| AppError::new("invalid_cursor", "Invalid cursor"))?;
            if sig != signature {
                return Err(AppError::new(
                    "invalid_cursor",
                    "Cursor does not match snapshot and query",
                ));
            }
            offset
        }
    };
    let mut nodes = snapshot
        .nodes
        .iter()
        .filter(|n| {
            let inside = if query.descendants {
                let mut parent = n.parent_id.as_deref();
                let mut found = false;
                while let Some(p) = parent {
                    if p == scope {
                        found = true;
                        break;
                    }
                    parent = by_id.get(p).and_then(|p| p.parent_id.as_deref());
                }
                found
            } else {
                n.parent_id.as_deref() == Some(scope)
            };
            inside
                && query.kind.is_none_or(|k| k == n.kind)
                && query
                    .name
                    .as_ref()
                    .is_none_or(|q| n.name.to_lowercase().contains(&q.to_lowercase()))
                && query.min_bytes.is_none_or(|x| n.logical_bytes >= x)
                && query.max_bytes.is_none_or(|x| n.logical_bytes <= x)
                && query
                    .modified_before
                    .is_none_or(|x| n.modified_ns.is_some_and(|m| m / 1_000_000 <= x))
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|a, b| {
        b.logical_bytes
            .cmp(&a.logical_bytes)
            .then_with(|| a.id.cmp(&b.id))
    });
    if offset > nodes.len() {
        return Err(AppError::new("invalid_cursor", "Cursor outside result"));
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let end = offset.saturating_add(limit).min(nodes.len());
    let next = if end < nodes.len() {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&(signature, end))
                    .map_err(|e| AppError::new("serialization", e.to_string()))?,
            ),
        )
    } else {
        None
    };
    Ok(
        serde_json::json!({"scan_id":snapshot.scan_id,"snapshot_id":snapshot.id,"items":nodes[offset..end],"next_cursor":next,"total_matches":nodes.len(),"includes_ancestor_descendant_overlap":query.descendants}),
    )
}
