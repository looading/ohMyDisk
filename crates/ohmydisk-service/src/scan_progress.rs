//! Overall scan checkpoints. Traversal estimates come from the core reducer;
//! publication owns the final 100 percent, after storage and indexing succeed.
use ohmydisk_core::{Job, ScanCompletion, ScanProgress};

pub(crate) fn queued() -> ScanProgress {
    let mut progress = ScanProgress::default();
    checkpoint(&mut progress, "queued", Some(0));
    progress
}

pub(crate) fn checkpoint(progress: &mut ScanProgress, phase: &str, percentage: Option<u8>) {
    progress.phase = phase.into();
    if let Some(percentage) = percentage {
        let completion = progress.completion.get_or_insert_with(|| ScanCompletion {
            percentage: 0,
            estimated: true,
            basis: "phase_checkpoint".into(),
            discovered_directory_tasks: 0,
            completed_directory_tasks: 0,
        });
        completion.percentage = completion.percentage.max(percentage).min(99);
        completion.estimated = true;
        completion.basis = "phase_checkpoint".into();
    }
}

pub(crate) fn complete(job: &mut Job, state: &str) {
    if job.kind != "scan" || state != "completed" {
        return;
    }
    checkpoint(&mut job.progress, "finished", Some(99));
    if let Some(completion) = &mut job.progress.completion {
        completion.percentage = 100;
        completion.estimated = false;
        completion.basis = "finished".into();
    }
}

pub(crate) fn for_summary(job: &Job, complete: bool, published: bool) -> ScanProgress {
    let mut progress = job.progress.clone();
    // ScanRecord and Job are read separately. A reader can hold an older record
    // while observing the just-finished job; do not attach its 100% to that record.
    if progress
        .completion
        .as_ref()
        .is_some_and(|value| value.percentage == 100)
        && !(complete && published && job.state == "completed")
    {
        progress.completion = None;
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(kind: &str) -> Job {
        Job {
            id: "job".into(),
            kind: kind.into(),
            state: "running".into(),
            created_at: 0,
            elapsed_ms: None,
            finished_at: None,
            progress: queued(),
            result: json!({}),
            error: None,
        }
    }

    #[test]
    fn publication_checkpoints_preserve_counts_and_never_go_backwards() {
        let mut progress = queued();
        let completion = progress.completion.as_mut().unwrap();
        completion.discovered_directory_tasks = 20;
        completion.completed_directory_tasks = 20;
        checkpoint(&mut progress, "saving", Some(95));
        checkpoint(&mut progress, "indexing", Some(98));
        checkpoint(&mut progress, "saving", Some(95));
        let completion = progress.completion.as_ref().unwrap();
        assert_eq!(completion.percentage, 98);
        assert!(completion.estimated);
        assert_eq!(completion.discovered_directory_tasks, 20);
        assert_eq!(completion.completed_directory_tasks, 20);
        checkpoint(&mut progress, "saving", Some(100));
        assert_eq!(progress.completion.unwrap().percentage, 99);
    }

    #[test]
    fn only_a_successful_scan_can_reach_one_hundred_percent() {
        for state in [
            "completed",
            "completed_with_errors",
            "cancelled",
            "failed",
            "interrupted",
        ] {
            let mut scan = job("scan");
            checkpoint(&mut scan.progress, "indexing", Some(98));
            complete(&mut scan, state);
            let completion = scan.progress.completion.unwrap();
            assert_eq!(
                completion.percentage,
                if state == "completed" { 100 } else { 98 }
            );
            assert_eq!(completion.estimated, state != "completed");
        }
        let mut cleanup = job("cleanup");
        cleanup.progress.completion = None;
        complete(&mut cleanup, "completed");
        assert!(cleanup.progress.completion.is_none());
    }

    #[test]
    fn saving_cancelled_results_does_not_advance_the_last_estimate() {
        let mut progress = queued();
        checkpoint(&mut progress, "scanning", Some(37));
        checkpoint(&mut progress, "saving", None);
        checkpoint(&mut progress, "indexing", None);
        assert_eq!(progress.phase, "indexing");
        assert_eq!(progress.completion.unwrap().percentage, 37);
    }

    #[test]
    fn an_older_summary_record_cannot_borrow_a_new_jobs_full_percentage() {
        let mut scan = job("scan");
        complete(&mut scan, "completed");
        scan.state = "completed".into();
        assert!(for_summary(&scan, false, true).completion.is_none());
        assert!(for_summary(&scan, true, false).completion.is_none());
        assert_eq!(
            for_summary(&scan, true, true)
                .completion
                .unwrap()
                .percentage,
            100
        );
        scan.state = "running".into();
        assert!(for_summary(&scan, true, true).completion.is_none());
        assert_eq!(scan.progress.completion.unwrap().percentage, 100);
    }
}
