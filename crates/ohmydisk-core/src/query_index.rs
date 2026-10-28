//! Position-only indexes for repeated queries against an immutable snapshot.

use crate::{AppError, Node, Query, Snapshot};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{cmp::Reverse, collections::HashMap, mem::size_of};

/// Derived data for one immutable snapshot. Node IDs and metadata remain owned
/// by the snapshot; the index keeps positions rather than copies of those values.
pub struct QueryIndex {
    snapshot_id: String,
    dense_ids: bool,
    by_id: Vec<usize>,
    ranked_all: Vec<usize>,
    rank: Vec<usize>,
    child_offsets: Vec<usize>,
    children: Vec<usize>,
    euler_order: Vec<usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
    root: Option<usize>,
}

fn invalid_snapshot() -> AppError {
    AppError::new(
        "invalid_snapshot",
        "Snapshot must contain one rooted tree with unique node IDs",
    )
}

fn canonical_position(id: &str) -> Option<usize> {
    let first = *id.as_bytes().first()?;
    if !first.is_ascii_digit() || (first == b'0' && id.len() != 1) {
        return None;
    }
    id.parse().ok()
}

fn decimal_lex_order(count: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(count);
    if count == 0 {
        return order;
    }
    order.push(0);
    let mut position = 1usize;
    for _ in 1..count {
        order.push(position);
        if let Some(child) = position.checked_mul(10).filter(|child| *child < count) {
            position = child;
        } else {
            while position % 10 == 9 || position.checked_add(1).is_none_or(|next| next >= count) {
                position /= 10;
            }
            position += 1;
        }
    }
    order
}

impl QueryIndex {
    /// Validate and index a snapshot once, without retaining its node ID strings.
    /// An empty partial snapshot is valid and has no queryable scope.
    ///
    /// # Errors
    /// Returns `invalid_snapshot` for duplicate IDs, missing parents, cycles or
    /// nodes disconnected from the declared root.
    pub fn new(snapshot: &Snapshot) -> Result<Self, AppError> {
        let count = snapshot.nodes.len();
        // Scanner IDs are canonical decimal positions. Validate that property
        // before bypassing hashing/sorting; imported or reordered trees retain
        // the general string-ID path and exactly the same query ordering.
        let dense_ids = snapshot
            .nodes
            .iter()
            .enumerate()
            .all(|(position, node)| canonical_position(&node.id) == Some(position));
        let mut positions = HashMap::new();
        if !dense_ids {
            positions.reserve(count);
            for (position, node) in snapshot.nodes.iter().enumerate() {
                if positions.insert(node.id.as_str(), position).is_some() {
                    return Err(invalid_snapshot());
                }
            }
        }
        let lookup = |id: &str| {
            if dense_ids {
                canonical_position(id).filter(|position| *position < count)
            } else {
                positions.get(id).copied()
            }
        };
        let root = if count == 0 {
            None
        } else {
            Some(lookup(snapshot.root_id.as_str()).ok_or_else(invalid_snapshot)?)
        };
        let parents = snapshot
            .nodes
            .iter()
            .enumerate()
            .map(|(position, node)| match &node.parent_id {
                None if Some(position) == root => Ok(None),
                Some(parent) if Some(position) != root => lookup(parent.as_str())
                    .map(Some)
                    .ok_or_else(invalid_snapshot),
                _ => Err(invalid_snapshot()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Only arbitrary IDs need a map while building and a sorted position
        // array for runtime lookup. Canonical IDs use checked direct positions.
        drop(positions);
        let mut by_id = if dense_ids {
            Vec::new()
        } else {
            (0..count).collect::<Vec<_>>()
        };
        by_id.sort_unstable_by(|a, b| snapshot.nodes[*a].id.cmp(&snapshot.nodes[*b].id));
        let ranked_all = if dense_ids {
            // Generate decimal string order in O(n), then stable-sort by size.
            // Equal sizes keep exactly the reference's lexicographic ID order,
            // without n*log(n) string comparisons for common equal-size files.
            let mut positions = decimal_lex_order(count);
            positions.sort_by_key(|position| Reverse(snapshot.nodes[*position].logical_bytes));
            positions
        } else {
            let mut positions = (0..count).collect::<Vec<_>>();
            positions.sort_unstable_by(|a, b| {
                snapshot.nodes[*b]
                    .logical_bytes
                    .cmp(&snapshot.nodes[*a].logical_bytes)
                    .then_with(|| snapshot.nodes[*a].id.cmp(&snapshot.nodes[*b].id))
            });
            positions
        };
        let mut rank = vec![0; count];
        for (order, position) in ranked_all.iter().copied().enumerate() {
            rank[position] = order;
        }
        let mut child_offsets = vec![0; count + 1];
        for parent in parents.iter().flatten() {
            child_offsets[*parent + 1] += 1;
        }
        for position in 1..child_offsets.len() {
            child_offsets[position] += child_offsets[position - 1];
        }
        let mut children = vec![0; count.saturating_sub(1)];
        let mut next_child = child_offsets[..count].to_vec();
        for position in ranked_all.iter().copied() {
            if let Some(parent) = parents[position] {
                children[next_child[parent]] = position;
                next_child[parent] += 1;
            }
        }
        drop(next_child);
        drop(parents);
        let mut enter = vec![usize::MAX; count];
        let mut exit = vec![0; count];
        let mut euler_order = Vec::with_capacity(count);
        let mut pending = root.map_or_else(Vec::new, |root| vec![(root, false)]);
        while let Some((position, leaving)) = pending.pop() {
            if leaving {
                exit[position] = euler_order.len();
            } else {
                if enter[position] != usize::MAX {
                    return Err(invalid_snapshot());
                }
                enter[position] = euler_order.len();
                euler_order.push(position);
                pending.push((position, true));
                for child in children[child_offsets[position]..child_offsets[position + 1]]
                    .iter()
                    .rev()
                {
                    pending.push((*child, false));
                }
            }
        }
        if euler_order.len() != count {
            return Err(invalid_snapshot());
        }
        Ok(Self {
            snapshot_id: snapshot.id.clone(),
            dense_ids,
            by_id,
            ranked_all,
            rank,
            child_offsets,
            children,
            euler_order,
            enter,
            exit,
            root,
        })
    }

    /// Allocated capacity owned by this index, excluding the snapshot and stack.
    #[must_use]
    pub fn estimated_heap_bytes(&self) -> usize {
        let positions = [
            &self.by_id,
            &self.ranked_all,
            &self.rank,
            &self.child_offsets,
            &self.children,
            &self.euler_order,
            &self.enter,
            &self.exit,
        ]
        .into_iter()
        .fold(0_usize, |sum, values| sum.saturating_add(values.capacity()));
        positions
            .saturating_mul(size_of::<usize>())
            .saturating_add(self.snapshot_id.capacity())
    }

    fn position(&self, snapshot: &Snapshot, id: &str) -> Option<usize> {
        if self.dense_ids {
            return canonical_position(id).filter(|position| *position < snapshot.nodes.len());
        }
        self.by_id
            .binary_search_by(|position| snapshot.nodes[*position].id.as_str().cmp(id))
            .ok()
            .map(|position| self.by_id[position])
    }

    fn direct_children(&self, scope: usize) -> &[usize] {
        &self.children[self.child_offsets[scope]..self.child_offsets[scope + 1]]
    }

    fn descendants(&self, scope: usize) -> &[usize] {
        // Euler intervals give subtree membership and its enumeration bounds
        // without following any ancestor chains during a query.
        &self.euler_order[self.enter[scope] + 1..self.exit[scope]]
    }
}

fn signature_and_offset(snapshot: &Snapshot, query: &Query) -> Result<(String, usize), AppError> {
    let mut criteria = query.clone();
    criteria.cursor = None;
    let signature = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(snapshot.id.as_str(), &criteria))
                .map_err(|error| AppError::new("serialization", error.to_string()))?
        )
    );
    let offset = match &query.cursor {
        None => 0,
        Some(cursor) => {
            let raw = URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| AppError::new("invalid_cursor", "Invalid cursor"))?;
            let (given_signature, offset): (String, usize) = serde_json::from_slice(&raw)
                .map_err(|_| AppError::new("invalid_cursor", "Invalid cursor"))?;
            if given_signature != signature {
                return Err(AppError::new(
                    "invalid_cursor",
                    "Cursor does not match snapshot and query",
                ));
            }
            offset
        }
    };
    Ok((signature, offset))
}

fn no_filters(query: &Query) -> bool {
    query.name.is_none()
        && query.kind.is_none()
        && query.min_bytes.is_none()
        && query.max_bytes.is_none()
        && query.modified_before.is_none()
}

fn name_matches(name: &str, lowercase_criterion: &str) -> bool {
    if lowercase_criterion.is_empty() {
        return true;
    }
    if name.is_ascii() && lowercase_criterion.is_ascii() {
        name.as_bytes()
            .windows(lowercase_criterion.len())
            .any(|part| part.eq_ignore_ascii_case(lowercase_criterion.as_bytes()))
    } else {
        name.to_lowercase().contains(lowercase_criterion)
    }
}

fn matches(node: &Node, query: &Query, name: Option<&str>) -> bool {
    query.kind.is_none_or(|kind| kind == node.kind)
        && name.is_none_or(|name| name_matches(&node.name, name))
        && query
            .min_bytes
            .is_none_or(|bytes| node.logical_bytes >= bytes)
        && query
            .max_bytes
            .is_none_or(|bytes| node.logical_bytes <= bytes)
        && query.modified_before.is_none_or(|before| {
            node.modified_ns
                .is_some_and(|time| time / 1_000_000 <= before)
        })
}

fn page_end(total: usize, offset: usize, query: &Query) -> Result<usize, AppError> {
    if offset > total {
        return Err(AppError::new("invalid_cursor", "Cursor outside result"));
    }
    Ok(offset
        .saturating_add(query.limit.unwrap_or(100).clamp(1, 500))
        .min(total))
}

fn response(
    snapshot: &Snapshot,
    query: &Query,
    signature: String,
    positions: &[usize],
    total: usize,
    end: usize,
) -> Result<serde_json::Value, AppError> {
    let next = if end < total {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&(signature, end))
                    .map_err(|error| AppError::new("serialization", error.to_string()))?,
            ),
        )
    } else {
        None
    };
    let items = positions
        .iter()
        .map(|position| &snapshot.nodes[*position])
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "scan_id":snapshot.scan_id,
        "snapshot_id":snapshot.id,
        "items":items,
        "next_cursor":next,
        "total_matches":total,
        "includes_ancestor_descendant_overlap":query.descendants,
    }))
}

/// Query an immutable snapshot using an index built from that same snapshot.
/// Results and cursor semantics match `query::query`.
///
/// # Errors
/// Returns `invalid_query_index` if the snapshot ID or node count differs,
/// `not_found` for a missing scope, or the reference query's `invalid_cursor`
/// and `serialization` errors.
pub fn query_indexed(
    snapshot: &Snapshot,
    index: &QueryIndex,
    query: &Query,
) -> Result<serde_json::Value, AppError> {
    if index.snapshot_id != snapshot.id || index.rank.len() != snapshot.nodes.len() {
        return Err(AppError::new(
            "invalid_query_index",
            "Index belongs to a different snapshot",
        ));
    }
    let scope = query.scope_node_id.as_deref().unwrap_or(&snapshot.root_id);
    let scope = index
        .position(snapshot, scope)
        .ok_or_else(|| AppError::new("not_found", "Scope node not found"))?;
    let (signature, offset) = signature_and_offset(snapshot, query)?;
    let unfiltered = no_filters(query);
    if unfiltered && !query.descendants {
        let children = index.direct_children(scope);
        let end = page_end(children.len(), offset, query)?;
        return response(
            snapshot,
            query,
            signature,
            &children[offset..end],
            children.len(),
            end,
        );
    }
    if unfiltered && Some(scope) == index.root {
        let total = snapshot.nodes.len() - 1;
        let end = page_end(total, offset, query)?;
        let scope_rank = index.rank[scope];
        // Translate the page's rank directly around the removed scope. Using
        // filter(...).skip(offset) would rescan the prefix on every later page.
        let page = (offset..end)
            .map(|rank| index.ranked_all[rank + usize::from(rank >= scope_rank)])
            .collect::<Vec<_>>();
        return response(snapshot, query, signature, &page, total, end);
    }
    let candidates = if query.descendants {
        index.descendants(scope)
    } else {
        index.direct_children(scope)
    };
    let lowercase_name = query.name.as_ref().map(|name| name.to_lowercase());
    let mut selected = candidates
        .iter()
        .copied()
        .filter(|position| matches(&snapshot.nodes[*position], query, lowercase_name.as_deref()))
        .collect::<Vec<_>>();
    let total = selected.len();
    let end = page_end(total, offset, query)?;
    if query.descendants && offset < end {
        // All matches still contribute to total_matches. Only the first end
        // global ranks need selection, and only the final page needs sorting.
        if end < total {
            selected.select_nth_unstable_by_key(end, |position| index.rank[*position]);
            selected.truncate(end);
        }
        if offset > 0 {
            selected.select_nth_unstable_by_key(offset, |position| index.rank[*position]);
        }
        selected[offset..end].sort_unstable_by_key(|position| index.rank[*position]);
    }
    response(
        snapshot,
        query,
        signature,
        &selected[offset..end],
        total,
        end,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativePath, NodeKind, ScanIssue, query::query as reference};
    use serde_json::{Value, json};

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            usize::try_from(self.0 >> 32).unwrap() % bound
        }
    }

    fn snapshot(count: usize, shape: usize, seed: u64, shuffle: bool) -> Snapshot {
        let mut random = Lcg(seed);
        let ids = (0..count)
            .map(|position| match position {
                0 => "root/🤖".into(),
                1 => String::new(),
                _ => format!("node-{:x}-é", count - position),
            })
            .collect::<Vec<String>>();
        let mut directories = vec![0];
        let names = [
            "Alpha.TXT",
            "ÄPFEL",
            "Straße",
            "İSTANBUL",
            "ΟΣ",
            "\u{212A}ELVIN",
            "éclair",
            "CAFÉ",
            "😀 Mix",
            "e\u{301}",
            ".hidden",
            "ascii-needle-NEEDLE",
            "Kelvin",
        ];
        let mut nodes = Vec::new();
        for position in 0..count {
            let parent = if position == 0 {
                None
            } else {
                Some(match shape {
                    0 => 0,
                    1 => position - 1,
                    _ => directories[random.next(directories.len())],
                })
            };
            let kind = if position == 0 || shape == 1 || position % 4 == 0 {
                NodeKind::Directory
            } else {
                [NodeKind::File, NodeKind::Symlink, NodeKind::Other][position % 3]
            };
            if position != 0 && kind == NodeKind::Directory {
                directories.push(position);
            }
            nodes.push(Node {
                id: ids[position].clone(),
                parent_id: parent.map(|parent| ids[parent].clone()),
                name: format!("{}-{position}", names[position % names.len()]),
                path: NativePath {
                    display: format!("/fixture/entry-{position}"),
                    encoded: format!("fixture-native-{position}"),
                },
                kind,
                logical_bytes: random.next(17) as u64 * 100,
                allocated_bytes: (position % 2 == 0).then_some(position as u64 * 512),
                modified_ns: (position % 3 != 0)
                    .then_some(position as u64 * 1_000_000 + position as u64),
                identity: format!("identity:{position}"),
                volume_id: "volume:fixture".into(),
                link_count: position as u64 % 5 + 1,
                complete: position % 3 == 0,
                is_link_or_reparse: kind == NodeKind::Symlink,
            });
        }
        if shuffle {
            for position in (1..nodes.len()).rev() {
                let other = random.next(position + 1);
                nodes.swap(position, other);
            }
        }
        Snapshot {
            id: "snapshot-fixture".into(),
            scan_id: "scan-fixture".into(),
            root_id: "root/🤖".into(),
            root_path: NativePath {
                display: "/fixture".into(),
                encoded: "L2ZpeHR1cmU=".into(),
            },
            created_at: 123,
            complete: false,
            cancelled: true,
            nodes,
            issues: vec![ScanIssue {
                node_id: None,
                path: "/fixture".into(),
                code: "fixture".into(),
                message: "partial fixture".into(),
            }],
        }
    }

    fn same(snapshot: &Snapshot, index: &QueryIndex, query: &Query) -> Option<Value> {
        match (
            reference(snapshot, query),
            query_indexed(snapshot, index, query),
        ) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(actual, expected);
                Some(actual)
            }
            (Err(expected), Err(actual)) => {
                assert_eq!(actual.code, expected.code);
                assert_eq!(actual.message, expected.message);
                None
            }
            _ => panic!("Reference and indexed query disagreed about success"),
        }
    }

    fn criteria() -> Vec<Query> {
        let mut queries = vec![Query::default()];
        for name in [
            "ALPHA", "ä", "STRASSE", "straße", "i\u{307}", "istanbul", "σ", "ς", "ΟΣ", "k",
            "\u{212A}", "İ", "é", "E\u{301}", "😀", "needle", "", "missing",
        ] {
            queries.push(Query {
                name: Some(name.into()),
                ..Query::default()
            });
        }
        for kind in [
            NodeKind::Directory,
            NodeKind::File,
            NodeKind::Symlink,
            NodeKind::Other,
        ] {
            queries.push(Query {
                kind: Some(kind),
                ..Query::default()
            });
        }
        for (min_bytes, max_bytes) in [
            (None, Some(0)),
            (Some(0), None),
            (Some(300), Some(900)),
            (Some(900), Some(300)),
            (Some(u64::MAX), None),
        ] {
            queries.push(Query {
                min_bytes,
                max_bytes,
                ..Query::default()
            });
        }
        for before in [0, 1, 19, u64::MAX] {
            queries.push(Query {
                modified_before: Some(before),
                ..Query::default()
            });
        }
        queries.push(Query {
            name: Some("a".into()),
            kind: Some(NodeKind::File),
            min_bytes: Some(100),
            max_bytes: Some(1_500),
            modified_before: Some(35),
            ..Query::default()
        });
        queries
    }

    #[test]
    fn differential_filters_all_scopes_shapes_and_shuffled_string_ids() {
        for shape in 0..3 {
            for seed in [7, 101] {
                let snapshot = snapshot(37, shape, seed, true);
                let index = QueryIndex::new(&snapshot).unwrap();
                for scope in std::iter::once(None)
                    .chain(snapshot.nodes.iter().map(|node| Some(node.id.clone())))
                {
                    for descendants in [false, true] {
                        for mut query in criteria() {
                            query.scope_node_id.clone_from(&scope);
                            query.descendants = descendants;
                            query.limit = Some(3);
                            let mut collected = Vec::new();
                            loop {
                                let page = same(&snapshot, &index, &query).unwrap();
                                collected.extend(page["items"].as_array().unwrap().iter().cloned());
                                query.cursor = page["next_cursor"].as_str().map(str::to_owned);
                                if query.cursor.is_none() {
                                    break;
                                }
                            }
                            query.limit = Some(500);
                            let all = same(&snapshot, &index, &query).unwrap();
                            assert_eq!(collected, *all["items"].as_array().unwrap());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn differential_limits_and_outside_or_mismatched_cursors() {
        let snapshot = snapshot(19, 2, 42, true);
        let index = QueryIndex::new(&snapshot).unwrap();
        for descendants in [false, true] {
            for scope in [
                None,
                Some(snapshot.root_id.clone()),
                Some(String::new()),
                Some("missing".into()),
            ] {
                for limit in [
                    None,
                    Some(0),
                    Some(1),
                    Some(2),
                    Some(100),
                    Some(500),
                    Some(501),
                    Some(usize::MAX),
                ] {
                    let mut query = Query {
                        scope_node_id: scope.clone(),
                        descendants,
                        limit,
                        ..Query::default()
                    };
                    same(&snapshot, &index, &query);
                    for cursor in [
                        "%not-base64".to_owned(),
                        URL_SAFE_NO_PAD.encode(b"not JSON"),
                        URL_SAFE_NO_PAD.encode(br#"["wrong",0]"#),
                    ] {
                        query.cursor = Some(cursor);
                        same(&snapshot, &index, &query);
                    }
                    query.cursor = None;
                    let (signature, _) = signature_and_offset(&snapshot, &query).unwrap();
                    for offset in [0, 1, 19, 20, usize::MAX] {
                        query.cursor = Some(
                            URL_SAFE_NO_PAD
                                .encode(serde_json::to_vec(&(signature.as_str(), offset)).unwrap()),
                        );
                        same(&snapshot, &index, &query);
                    }
                }
            }
        }
        let original = Query {
            descendants: true,
            limit: Some(1),
            ..Query::default()
        };
        let page = same(&snapshot, &index, &original).unwrap();
        let cursor = page["next_cursor"].as_str().unwrap().to_owned();
        for mut changed in [
            Query {
                limit: Some(2),
                ..original.clone()
            },
            Query {
                name: Some(String::new()),
                ..original.clone()
            },
            Query {
                scope_node_id: Some(snapshot.root_id.clone()),
                ..original.clone()
            },
            Query {
                descendants: false,
                ..original.clone()
            },
        ] {
            changed.cursor = Some(cursor.clone());
            same(&snapshot, &index, &changed);
        }
    }

    #[test]
    fn root_fast_page_excludes_scope_at_every_possible_global_rank() {
        let mut snapshot = snapshot(21, 0, 21, true);
        for bytes in [0, 100, 700, 1_600, u64::MAX] {
            snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == snapshot.root_id)
                .unwrap()
                .logical_bytes = bytes;
            let index = QueryIndex::new(&snapshot).unwrap();
            let mut query = Query {
                descendants: true,
                limit: Some(2),
                ..Query::default()
            };
            loop {
                let page = same(&snapshot, &index, &query).unwrap();
                query.cursor = page["next_cursor"].as_str().map(str::to_owned);
                if query.cursor.is_none() {
                    break;
                }
            }
        }
    }

    #[test]
    fn empty_partial_snapshot_and_single_node_match_reference() {
        for count in [0, 1] {
            let snapshot = snapshot(count, 0, 2, false);
            let index = QueryIndex::new(&snapshot).unwrap();
            for mut query in criteria() {
                for descendants in [false, true] {
                    query.descendants = descendants;
                    same(&snapshot, &index, &query);
                }
            }
        }
    }

    #[test]
    fn canonical_position_ids_keep_string_order_and_reject_numeric_aliases() {
        let mut snapshot = snapshot(121, 2, 19, false);
        let original_ids = snapshot
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        for node in &mut snapshot.nodes {
            if let Some(parent) = &mut node.parent_id {
                *parent = original_ids
                    .iter()
                    .position(|id| id == parent)
                    .unwrap()
                    .to_string();
            }
        }
        for (position, node) in snapshot.nodes.iter_mut().enumerate() {
            node.id = position.to_string();
        }
        snapshot.root_id = "0".into();
        let index = QueryIndex::new(&snapshot).unwrap();
        assert!(index.dense_ids);
        assert!(index.by_id.is_empty());
        for query in criteria() {
            for scope in [
                "0",
                "1",
                "10",
                "120",
                "121",
                "01",
                "+1",
                "-0",
                "",
                "999999999999999999999999999",
            ] {
                for descendants in [false, true] {
                    same(
                        &snapshot,
                        &index,
                        &Query {
                            scope_node_id: Some(scope.into()),
                            descendants,
                            ..query.clone()
                        },
                    );
                }
            }
        }
        // Numeric-looking arbitrary IDs remain valid via the general path.
        for alias in ["01", "+1", "-0"] {
            let mut imported = snapshot.clone();
            imported.nodes[120].id = alias.into();
            let index = QueryIndex::new(&imported).unwrap();
            assert!(!index.dense_ids);
            same(
                &imported,
                &index,
                &Query {
                    scope_node_id: Some(alias.into()),
                    ..Query::default()
                },
            );
            same(
                &imported,
                &index,
                &Query {
                    descendants: true,
                    ..Query::default()
                },
            );
        }
    }

    #[test]
    fn decimal_order_matches_reference_string_sort_at_digit_boundaries() {
        for count in [0, 1, 2, 9, 10, 11, 19, 20, 99, 100, 101, 999, 1000, 100_100] {
            let mut strings = (0..count)
                .map(|position| position.to_string())
                .collect::<Vec<_>>();
            strings.sort_unstable();
            let expected = strings
                .iter()
                .map(|id| id.parse::<usize>().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(decimal_lex_order(count), expected, "node count {count}");
        }
    }

    #[test]
    fn malformed_graphs_are_rejected_before_queries_can_loop() {
        let valid = snapshot(6, 0, 3, false);
        let mut duplicate = valid.clone();
        duplicate.nodes[2].id = duplicate.nodes[1].id.clone();
        let mut missing_root = valid.clone();
        missing_root.root_id = "absent-root".into();
        let mut missing_parent = valid.clone();
        missing_parent.nodes[2].parent_id = Some("absent-parent".into());
        let mut second_root = valid.clone();
        second_root.nodes[2].parent_id = None;
        let mut root_cycle = valid.clone();
        root_cycle.nodes[0].parent_id = Some(root_cycle.root_id.clone());
        let mut self_cycle = valid.clone();
        self_cycle.nodes[2].parent_id = Some(self_cycle.nodes[2].id.clone());
        let mut separate_cycle = valid;
        separate_cycle.nodes[2].parent_id = Some(separate_cycle.nodes[3].id.clone());
        separate_cycle.nodes[3].parent_id = Some(separate_cycle.nodes[2].id.clone());
        for snapshot in [
            duplicate,
            missing_root,
            missing_parent,
            second_root,
            root_cycle,
            self_cycle,
            separate_cycle,
        ] {
            match QueryIndex::new(&snapshot) {
                Err(error) => assert_eq!(error.code, "invalid_snapshot"),
                Ok(_) => panic!("Malformed graph unexpectedly accepted"),
            }
        }
    }

    #[test]
    fn deep_tree_build_is_iterative_and_heap_estimate_counts_position_arrays() {
        let snapshot = snapshot(10_000, 1, 3, true);
        let index = QueryIndex::new(&snapshot).unwrap();
        let query = Query {
            descendants: true,
            ..Query::default()
        };
        let response = query_indexed(&snapshot, &index, &query).unwrap();
        assert_eq!(response["total_matches"], json!(9_999));
        assert_eq!(response["items"].as_array().unwrap().len(), 100);
        assert_eq!(index.euler_order.len(), snapshot.nodes.len());
        assert_eq!(
            index.estimated_heap_bytes(),
            8 * snapshot.nodes.len() * size_of::<usize>() + snapshot.id.len()
        );
        let mut different = snapshot.clone();
        different.id.push('x');
        assert_eq!(
            query_indexed(&different, &index, &query).unwrap_err().code,
            "invalid_query_index"
        );
        different.id.clone_from(&snapshot.id);
        different.nodes.pop();
        assert_eq!(
            query_indexed(&different, &index, &query).unwrap_err().code,
            "invalid_query_index"
        );
    }
}
