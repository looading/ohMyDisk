//! Presentation invariants for the capacity view. Never repair raw values by capping them.
use ohmydisk_core::{Node, SpaceAccounting, SpaceObservation, Volume};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootMode {
    Volume,
    #[default]
    Directory,
}

/// This comparison chooses a view only. It is never used for filesystem authorization.
pub fn default_root_mode(root: &str, sources: &[Volume], windows: bool) -> RootMode {
    let root = normalized_display_path(root, windows);
    if !root.is_empty()
        && sources
            .iter()
            .any(|source| normalized_display_path(&source.mount_path.display, windows) == root)
    {
        RootMode::Volume
    } else {
        RootMode::Directory
    }
}
fn normalized_display_path(value: &str, windows: bool) -> String {
    let mut path = if windows {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    if windows {
        if let Some(rest) = path.strip_prefix("//?/") {
            path = if rest
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
            {
                format!("//{}", &rest[4..])
            } else {
                rest.to_owned()
            };
        }
        path = path.to_lowercase();
    }
    if path != "/" {
        path.truncate(path.trim_end_matches('/').len());
    }
    path
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub total: u64,
    pub available: u64,
    pub attributed: u64,
    pub remainder: u64,
}
impl Capacity {
    pub fn reconciled(accounting: &SpaceAccounting) -> Option<Self> {
        let current = accounting.after.as_ref()?;
        let before = &accounting.before;
        if accounting.reconciliation_status != "balanced"
            || before.volume_id != current.volume_id
            || before.total_bytes != current.total_bytes
            || before.capacity_scope != current.capacity_scope
            || current.capacity_scope == "caller_view"
        {
            return None;
        }
        let used = current.total_bytes.checked_sub(current.available_bytes)?;
        let remainder = used.checked_sub(accounting.attributed_bytes)?;
        if accounting.shared_unattributed_bytes != Some(remainder) {
            return None;
        }
        Some(Self {
            total: current.total_bytes,
            available: current.available_bytes,
            attributed: accounting.attributed_bytes,
            remainder,
        })
    }
}

pub fn observation(accounting: &SpaceAccounting) -> &SpaceObservation {
    accounting.after.as_ref().unwrap_or(&accounting.before)
}

/// None and a fully measured zero are different states, including empty directories.
pub fn node_bytes(node: &Node) -> Option<u64> {
    node.space_usage.as_ref().and_then(|space| {
        (space.measured_entries > 0 || space.unmeasured_entries == 0)
            .then_some(space.attributed_bytes)
    })
}
pub fn partly_measured(node: &Node) -> bool {
    node.space_usage
        .as_ref()
        .is_some_and(|space| space.unmeasured_entries > 0)
}
pub fn shared_zero(node: &Node) -> bool {
    node_bytes(node) == Some(0) && node.allocated_bytes.is_some_and(|bytes| bytes > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmydisk_core::AttributionProvider;
    fn accounting(attributed: u64) -> SpaceAccounting {
        let before = SpaceObservation {
            volume_id: "disk".into(),
            total_bytes: 995,
            available_bytes: 245,
            captured_at: 0,
            source: "fixture".into(),
            capacity_scope: "volume".into(),
            provider: AttributionProvider::MacPrivateSize,
            used_bytes: None,
            caller_available_bytes: None,
        };
        SpaceAccounting {
            after: Some(before.clone()),
            before,
            attributed_bytes: attributed,
            shared_unattributed_bytes: 750_u64.checked_sub(attributed),
            reconciliation_status: "balanced".into(),
            measurement_status: "complete".into(),
        }
    }
    fn source(path: &str) -> Volume {
        Volume {
            id: "source".into(),
            name: "source".into(),
            mount_path: ohmydisk_core::NativePath::new(std::path::Path::new(path)),
            total_bytes: 995,
            available_bytes: 245,
            space_observation: None,
        }
    }
    #[test]
    fn only_known_mount_roots_default_to_volume_context() {
        let sources = [source("/"), source("/Volumes/External/")];
        assert_eq!(default_root_mode("/", &sources, false), RootMode::Volume);
        assert_eq!(
            default_root_mode("/Volumes/External", &sources, false),
            RootMode::Volume
        );
        assert_eq!(
            default_root_mode("/tmp/folder", &sources, false),
            RootMode::Directory
        );
        assert_eq!(default_root_mode("/", &[], false), RootMode::Directory);
    }
    #[test]
    fn windows_mount_comparison_normalizes_display_prefix_case_and_separators() {
        let sources = [source(r"C:\"), source(r"\\Server\Share\")];
        assert_eq!(
            default_root_mode(r"\\?\c:\", &sources, true),
            RootMode::Volume
        );
        assert_eq!(default_root_mode("c:/", &sources, true), RootMode::Volume);
        assert_eq!(
            default_root_mode(r"\\?\UNC\server\SHARE\", &sources, true),
            RootMode::Volume
        );
        assert_eq!(
            default_root_mode(r"C:\folder", &sources, true),
            RootMode::Directory
        );
    }
    #[test]
    fn reconciled_ring_preserves_capacity_and_rejects_overflow() {
        let model = Capacity::reconciled(&accounting(500)).unwrap();
        assert_eq!(model.available + model.attributed + model.remainder, 995);
        assert_eq!(model.remainder, 250);
        assert!(Capacity::reconciled(&accounting(1100)).is_none());
        let mut changed = accounting(500);
        let mut after = changed.before.clone();
        after.volume_id = "different".into();
        changed.after = Some(after);
        assert!(Capacity::reconciled(&changed).is_none());
        let mut no_end_observation = accounting(500);
        no_end_observation.after = None;
        assert!(Capacity::reconciled(&no_end_observation).is_none());
        let mut wrong_remainder = accounting(500);
        wrong_remainder.shared_unattributed_bytes = Some(0);
        assert!(Capacity::reconciled(&wrong_remainder).is_none());
    }
}
