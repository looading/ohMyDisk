//! Shared scan limits. Auto favors desktop responsiveness; explicit workers can
//! use more of the process-wide filesystem budget without multiplying that budget.

/// Maximum supported directory-worker setting for one scan.
pub const MAX_SCAN_WORKERS: usize = 64;

/// Warm-cache benchmarks favor four workers for small-file and mixed workloads.
pub const AUTO_SCAN_WORKER_CAP: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanConcurrency {
    /// OS-reported available parallelism, with an unknown or zero value set to one.
    pub available_parallelism: usize,
    /// Per-scan directory-worker limit when the caller omits an explicit value.
    pub auto_workers: usize,
    /// One shared filesystem-worker limit for the entire service.
    pub filesystem_workers: usize,
}

impl ScanConcurrency {
    /// Query available CPU parallelism once when initializing a scan policy.
    #[must_use]
    pub fn detect() -> Self {
        Self::for_parallelism(
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        )
    }

    /// Resolve the policy independently of hardware queries and filesystem work.
    #[must_use]
    pub fn for_parallelism(available_parallelism: usize) -> Self {
        let available_parallelism = available_parallelism.max(1);
        Self {
            available_parallelism,
            auto_workers: available_parallelism.min(AUTO_SCAN_WORKER_CAP),
            filesystem_workers: available_parallelism.min(MAX_SCAN_WORKERS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_shared_budget_resolve_low_and_high_cpu_counts() {
        for (reported, available, auto, filesystem) in [
            (0, 1, 1, 1),
            (1, 1, 1, 1),
            (2, 2, 2, 2),
            (4, 4, 4, 4),
            (8, 8, 4, 8),
            (18, 18, 4, 18),
            (64, 64, 4, 64),
            (128, 128, 4, 64),
            (usize::MAX, usize::MAX, 4, 64),
        ] {
            let policy = ScanConcurrency::for_parallelism(reported);
            assert_eq!(policy.available_parallelism, available);
            assert_eq!(policy.auto_workers, auto);
            assert_eq!(policy.filesystem_workers, filesystem);
            assert!((1..=MAX_SCAN_WORKERS).contains(&policy.auto_workers));
            assert!((policy.auto_workers..=MAX_SCAN_WORKERS).contains(&policy.filesystem_workers));
        }
    }

    #[test]
    fn detected_policy_keeps_a_nonzero_bounded_shared_budget() {
        let policy = ScanConcurrency::detect();
        assert_eq!(
            policy,
            ScanConcurrency::for_parallelism(policy.available_parallelism)
        );
        assert!(policy.auto_workers > 0);
        assert!(policy.auto_workers <= policy.filesystem_workers);
        assert!(policy.filesystem_workers <= MAX_SCAN_WORKERS);
    }
}
