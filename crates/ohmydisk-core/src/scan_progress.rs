//! Constant-cost telemetry on the reducer's existing directory events.
//! The known-work denominator grows during discovery, so its high-water mark is
//! an estimate. Traversal owns at most 90%; durable service completion owns 100%.
use crate::ScanProgress;

impl ScanProgress {
    pub(crate) fn discover_directory_task(&mut self) {
        if let Some(completion) = &mut self.completion {
            completion.discovered_directory_tasks =
                completion.discovered_directory_tasks.saturating_add(1);
        }
    }

    pub(crate) fn complete_directory_task(&mut self) {
        if let Some(completion) = &mut self.completion {
            completion.completed_directory_tasks = completion
                .completed_directory_tasks
                .saturating_add(1)
                .min(completion.discovered_directory_tasks);
            // Widen before multiplication so a saturated u64 counter still
            // produces a correct bounded ratio rather than an overflow.
            let estimate = u128::from(completion.completed_directory_tasks) * 90
                / u128::from(completion.discovered_directory_tasks.max(1));
            completion.percentage = completion
                .percentage
                .max(u8::try_from(estimate).unwrap_or(90))
                .min(90);
        }
    }

    pub(crate) fn begin_reducing(&mut self) {
        self.phase = "reducing".into();
        if let Some(completion) = &mut self.completion {
            completion.percentage = 90;
            completion.estimated = true;
            completion.basis = "phase_checkpoint".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ScanCompletion, ScanProgress};

    fn progress() -> ScanProgress {
        ScanProgress {
            completion: Some(ScanCompletion::default()),
            ..Default::default()
        }
    }

    #[test]
    fn expanding_discovery_keeps_an_estimated_high_water_mark() {
        let mut progress = progress();
        progress.discover_directory_task();
        progress.complete_directory_task();
        assert_eq!(progress.completion.as_ref().unwrap().percentage, 45);
        for _ in 0..98 {
            progress.discover_directory_task();
        }
        let completion = progress.completion.as_ref().unwrap();
        assert_eq!(completion.discovered_directory_tasks, 100);
        assert_eq!(completion.completed_directory_tasks, 1);
        assert_eq!(completion.percentage, 45);
        progress.complete_directory_task();
        assert_eq!(progress.completion.as_ref().unwrap().percentage, 45);
        for _ in 2..100 {
            progress.complete_directory_task();
        }
        let completion = progress.completion.as_ref().unwrap();
        assert_eq!(completion.completed_directory_tasks, 100);
        assert_eq!(completion.percentage, 90);
        assert!(completion.estimated);
        progress.begin_reducing();
        assert_eq!(progress.phase, "reducing");
        assert_eq!(progress.completion.as_ref().unwrap().percentage, 90);
    }

    #[test]
    fn counters_saturate_without_overflowing_percentage() {
        let mut progress = progress();
        let completion = progress.completion.as_mut().unwrap();
        completion.discovered_directory_tasks = u64::MAX;
        completion.completed_directory_tasks = u64::MAX - 1;
        progress.discover_directory_task();
        progress.complete_directory_task();
        progress.complete_directory_task();
        let completion = progress.completion.unwrap();
        assert_eq!(completion.discovered_directory_tasks, u64::MAX);
        assert_eq!(completion.completed_directory_tasks, u64::MAX);
        assert_eq!(completion.percentage, 90);
    }

    #[test]
    fn legacy_progress_remains_unknown_and_new_telemetry_round_trips() {
        let legacy = serde_json::json!({
            "files":12,"directories":3,"bytes":1024,"current_path":"fixture","phase":"scanning"
        });
        let mut parsed: ScanProgress = serde_json::from_value(legacy.clone()).unwrap();
        assert!(parsed.completion.is_none());
        parsed.discover_directory_task();
        parsed.complete_directory_task();
        assert!(parsed.completion.is_none());
        assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
        let current = progress();
        let round_trip: ScanProgress =
            serde_json::from_value(serde_json::to_value(&current).unwrap()).unwrap();
        assert_eq!(round_trip.completion, current.completion);
    }
}
