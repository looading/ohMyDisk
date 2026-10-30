//! Explicit occupancy measurements; raw logical lengths are never capped.
use crate::{AppError, FileInfo, Node, NodeKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeMetric {
    #[default]
    Logical,
    Allocated,
    Attributed,
}
impl SizeMetric {
    pub fn is_logical(&self) -> bool {
        *self == Self::Logical
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionProvider {
    MacPrivateSize,
    NtfsUnsharedAllocation,
    #[default]
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSpace {
    pub attributed_bytes: u64,
    pub measured_entries: u64,
    pub unmeasured_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceObservation {
    pub volume_id: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub captured_at: u64,
    pub source: String,
    pub capacity_scope: String,
    pub provider: AttributionProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceAccounting {
    pub before: SpaceObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<SpaceObservation>,
    pub attributed_bytes: u64,
    pub shared_unattributed_bytes: Option<u64>,
    pub reconciliation_status: String,
    pub measurement_status: String,
}

pub fn file_space(info: &FileInfo, provider: AttributionProvider) -> NodeSpace {
    if info.kind != NodeKind::File
        || info.is_link_or_reparse
        || info.link_count != 1
        || info.identity.is_empty()
    {
        return NodeSpace {
            unmeasured_entries: 1,
            ..NodeSpace::default()
        };
    }
    let bytes = match provider {
        AttributionProvider::MacPrivateSize => info.private_bytes,
        AttributionProvider::NtfsUnsharedAllocation => info.allocated_bytes,
        AttributionProvider::Unsupported => None,
    };
    match bytes {
        Some(attributed_bytes) => NodeSpace {
            attributed_bytes,
            measured_entries: 1,
            unmeasured_entries: 0,
        },
        None => NodeSpace {
            unmeasured_entries: 1,
            ..NodeSpace::default()
        },
    }
}

pub fn metric_value(node: &Node, metric: SizeMetric) -> Option<u64> {
    match metric {
        SizeMetric::Logical => Some(node.logical_bytes),
        SizeMetric::Allocated => node.allocated_bytes,
        SizeMetric::Attributed => node.space_usage.as_ref().and_then(|space| {
            (space.measured_entries > 0 || space.unmeasured_entries == 0)
                .then_some(space.attributed_bytes)
        }),
    }
}

pub fn metric_is_complete(node: &Node, metric: SizeMetric) -> bool {
    metric_value(node, metric).is_some()
        && (metric == SizeMetric::Logical || node.complete)
        && (metric != SizeMetric::Attributed
            || node
                .space_usage
                .as_ref()
                .is_some_and(|space| space.unmeasured_entries == 0))
}

pub fn reconcile_space(
    before: SpaceObservation,
    after: Option<SpaceObservation>,
    usage: &NodeSpace,
    complete: bool,
) -> SpaceAccounting {
    let observation = after.as_ref().unwrap_or(&before);
    let coherent = after.is_some()
        && before.available_bytes <= before.total_bytes
        && observation.volume_id == before.volume_id
        && observation.capacity_scope == before.capacity_scope
        && observation.total_bytes == before.total_bytes
        && observation.available_bytes <= observation.total_bytes
        && observation.capacity_scope != "caller_view";
    let remainder = coherent
        .then(|| {
            observation
                .total_bytes
                .checked_sub(observation.available_bytes)
                .and_then(|used| used.checked_sub(usage.attributed_bytes))
        })
        .flatten();
    let measurement_status = if usage.unmeasured_entries == 0 && complete {
        "complete"
    } else if usage.measured_entries > 0 {
        "partial"
    } else {
        "unavailable"
    };
    let reconciliation_status = if after.is_none() {
        "unavailable"
    } else if remainder.is_some() {
        "balanced"
    } else {
        "inconsistent"
    };
    SpaceAccounting {
        before,
        after,
        attributed_bytes: usage.attributed_bytes,
        shared_unattributed_bytes: remainder,
        reconciliation_status: reconciliation_status.into(),
        measurement_status: measurement_status.into(),
    }
}

pub fn require_measurement(nodes: &[Node], metric: SizeMetric) -> Result<(), AppError> {
    if metric == SizeMetric::Attributed && nodes.iter().all(|node| node.space_usage.is_none()) {
        return Err(AppError::new(
            "measurement_required",
            "This snapshot has no occupancy measurement; rescan to measure it",
        ));
    }
    Ok(())
}

/// Keep query measurements self-describing for MCP clients that query directly.
pub fn annotate_query(
    value: &mut serde_json::Value,
    snapshot: &crate::Snapshot,
    metric: SizeMetric,
) {
    let semantics = match metric {
        SizeMetric::Logical => "file_entry_logical_lengths",
        SizeMetric::Allocated => "reported_allocated_bytes_may_overlap",
        SizeMetric::Attributed => "confirmed_file_attribution_not_cleanup_yield",
    };
    value["size_semantics"] = serde_json::json!(semantics);
    if metric == SizeMetric::Attributed {
        let accounting = snapshot.space_accounting.as_ref();
        value["measurement_status"] = serde_json::json!(
            accounting.map_or("unavailable", |space| space.measurement_status.as_str())
        );
        value["reconciliation_status"] = serde_json::json!(
            accounting.map_or("unavailable", |space| space.reconciliation_status.as_str())
        );
        value["attribution_provider"] =
            serde_json::to_value(accounting.map(|space| space.before.provider))
                .unwrap_or(serde_json::Value::Null);
    } else {
        value["measurement_status"] = serde_json::json!("reported");
        value["reconciliation_status"] = serde_json::json!("not_applicable");
        value["attribution_provider"] = serde_json::Value::Null;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation(available_bytes: u64) -> SpaceObservation {
        SpaceObservation {
            volume_id: "volume".into(),
            total_bytes: 995,
            available_bytes,
            captured_at: 1,
            source: "fixture".into(),
            capacity_scope: "filesystem_visible".into(),
            provider: AttributionProvider::MacPrivateSize,
            used_bytes: None,
            caller_available_bytes: None,
        }
    }
    fn file() -> FileInfo {
        FileInfo {
            private_bytes: Some(3),
            kind: NodeKind::File,
            logical_bytes: 1100,
            allocated_bytes: Some(4),
            modified_ns: None,
            identity: "inode".into(),
            volume_id: "volume".into(),
            link_count: 1,
            is_link_or_reparse: false,
        }
    }
    #[test]
    fn large_logical_length_does_not_enter_occupancy_equation() {
        let mut usage = file_space(&file(), AttributionProvider::MacPrivateSize);
        assert_eq!(usage.attributed_bytes, 3);
        usage.attributed_bytes = 500;
        let ledger = reconcile_space(observation(245), Some(observation(245)), &usage, true);
        assert_eq!(ledger.shared_unattributed_bytes, Some(250));
        assert_eq!(ledger.reconciliation_status, "balanced");
        assert_eq!(
            245 + ledger.attributed_bytes + ledger.shared_unattributed_bytes.unwrap(),
            995
        );
    }
    #[test]
    fn known_zero_is_different_from_missing_and_hardlinked_metadata() {
        let mut info = file();
        info.private_bytes = Some(0);
        let zero = file_space(&info, AttributionProvider::MacPrivateSize);
        assert_eq!((zero.measured_entries, zero.unmeasured_entries), (1, 0));
        info.private_bytes = None;
        assert_eq!(
            file_space(&info, AttributionProvider::MacPrivateSize).unmeasured_entries,
            1
        );
        info.private_bytes = Some(3);
        info.link_count = 2;
        assert_eq!(
            file_space(&info, AttributionProvider::MacPrivateSize).attributed_bytes,
            0
        );
        assert_eq!(
            file_space(&info, AttributionProvider::MacPrivateSize).unmeasured_entries,
            1
        );
        info.link_count = 1;
        assert_eq!(
            file_space(&info, AttributionProvider::NtfsUnsharedAllocation).attributed_bytes,
            4
        );
        assert_eq!(
            file_space(&info, AttributionProvider::Unsupported).unmeasured_entries,
            1
        );
    }
    #[test]
    fn oversized_or_unmatched_observations_are_never_forced_to_balance() {
        let usage = NodeSpace {
            attributed_bytes: 900,
            measured_entries: 1,
            unmeasured_entries: 0,
        };
        let ledger = reconcile_space(observation(245), Some(observation(245)), &usage, true);
        assert_eq!(ledger.attributed_bytes, 900);
        assert_eq!(ledger.shared_unattributed_bytes, None);
        assert_eq!(ledger.reconciliation_status, "inconsistent");
        let missing = reconcile_space(observation(245), None, &usage, true);
        assert_eq!(missing.reconciliation_status, "unavailable");
        let mut changed = observation(245);
        changed.volume_id = "other".into();
        assert_eq!(
            reconcile_space(observation(245), Some(changed), &usage, true)
                .shared_unattributed_bytes,
            None
        );
        let mut quota = observation(245);
        quota.capacity_scope = "caller_view".into();
        assert_eq!(
            reconcile_space(quota.clone(), Some(quota), &NodeSpace::default(), true)
                .shared_unattributed_bytes,
            None
        );
    }
    #[test]
    fn unknown_and_partial_entries_remain_in_measurement_status() {
        let mut usage = NodeSpace {
            attributed_bytes: 20,
            measured_entries: 1,
            unmeasured_entries: 1,
        };
        assert_eq!(
            reconcile_space(observation(245), Some(observation(245)), &usage, false)
                .measurement_status,
            "partial"
        );
        usage.measured_entries = 0;
        usage.attributed_bytes = 0;
        assert_eq!(
            reconcile_space(observation(245), Some(observation(245)), &usage, false)
                .measurement_status,
            "unavailable"
        );
    }
}
