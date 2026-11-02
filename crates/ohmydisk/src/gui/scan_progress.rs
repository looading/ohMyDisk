//! Presentation rules for optional service estimates; elapsed time never drives fill.
use super::locale::Language;
use ohmydisk_core::ScanCompletion;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Presentation {
    pub percentage: Option<u8>,
    pub estimated: bool,
}

impl Presentation {
    pub fn active(completion: Option<&ScanCompletion>) -> Self {
        Self {
            percentage: completion.map(|value| value.percentage.min(99)),
            estimated: true,
        }
    }

    pub fn terminal(completion: Option<&ScanCompletion>, successful: bool) -> Option<Self> {
        completion.map(|value| {
            if successful && value.percentage == 100 && !value.estimated {
                Self {
                    percentage: Some(100),
                    estimated: false,
                }
            } else {
                Self::active(Some(value))
            }
        })
    }

    pub fn label(self, language: Language) -> String {
        match self.percentage {
            Some(percentage) if self.estimated => {
                format!("{} {percentage}%", language.text("Estimated", "估算"))
            }
            Some(percentage) => format!("{percentage}%"),
            None => language.text("Estimating…", "正在估算…").into(),
        }
    }

    pub fn fill(self) -> f32 {
        f32::from(self.percentage.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(percentage: u8, estimated: bool) -> ScanCompletion {
        ScanCompletion {
            percentage,
            estimated,
            basis: "directory_work".into(),
            discovered_directory_tasks: 12,
            completed_directory_tasks: 5,
        }
    }

    #[test]
    fn missing_estimates_keep_an_empty_track_and_switch_language_at_render_time() {
        let value = Presentation::active(None);
        assert_eq!(value.fill(), 0.0);
        assert_eq!(value.label(Language::English), "Estimating…");
        assert_eq!(value.label(Language::Chinese), "正在估算…");
        assert!(Presentation::terminal(None, true).is_none());
    }

    #[test]
    fn only_explicit_success_can_show_one_hundred_percent() {
        let complete = completion(100, false);
        assert_eq!(Presentation::active(Some(&complete)).percentage, Some(99));
        for successful in [false, true] {
            let value = Presentation::terminal(Some(&complete), successful).unwrap();
            assert_eq!(value.percentage, Some(if successful { 100 } else { 99 }));
            assert_eq!(value.estimated, !successful);
        }
        let estimated = completion(100, true);
        assert_eq!(
            Presentation::terminal(Some(&estimated), true)
                .unwrap()
                .percentage,
            Some(99)
        );
        let malformed = completion(u8::MAX, false);
        assert_eq!(Presentation::active(Some(&malformed)).fill(), 99.0);
        assert_eq!(
            Presentation::terminal(Some(&malformed), true)
                .unwrap()
                .fill(),
            99.0
        );
    }

    #[test]
    fn estimated_percentages_remain_distinct_from_exact_completion() {
        let value = Presentation::active(Some(&completion(42, true)));
        assert_eq!(value.label(Language::English), "Estimated 42%");
        assert_eq!(value.label(Language::Chinese), "估算 42%");
        let done = Presentation::terminal(Some(&completion(100, false)), true).unwrap();
        assert_eq!(done.label(Language::English), "100%");
        assert_eq!(done.label(Language::Chinese), "100%");
    }
}
