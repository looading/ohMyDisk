//! UI interpolation of the service's measured scan time, independent of RPCs.
use super::locale::Language;
use std::time::Instant;

#[derive(Clone, Debug)]
pub(super) struct Clock {
    base_ms: u64,
    sampled_at: Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            base_ms: 0,
            sampled_at: Instant::now(),
        }
    }
}

impl Clock {
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_at(Instant::now())
    }

    fn elapsed_at(&self, now: Instant) -> u64 {
        self.base_ms.saturating_add(
            u64::try_from(now.saturating_duration_since(self.sampled_at).as_millis())
                .unwrap_or(u64::MAX),
        )
    }

    pub fn observe(&mut self, elapsed_ms: u64) {
        self.observe_at(elapsed_ms, Instant::now());
    }

    fn observe_at(&mut self, elapsed_ms: u64, now: Instant) {
        // A delayed service reply must not rewind the visible running clock.
        self.base_ms = self.elapsed_at(now).max(elapsed_ms);
        self.sampled_at = now;
    }
}

pub(super) fn format_duration(ms: u64, language: Language) -> String {
    if ms < 1_000 {
        return format!("{ms} {}", language.text("ms", "毫秒"));
    }
    let seconds = ms / 1_000;
    if seconds < 60 {
        return format!("{seconds} {}", language.text("s", "秒"));
    }
    if seconds < 3_600 {
        return format!(
            "{} {} {} {}",
            seconds / 60,
            language.text("m", "分"),
            seconds % 60,
            language.text("s", "秒")
        );
    }
    format!(
        "{} {} {} {} {} {}",
        seconds / 3_600,
        language.text("h", "小时"),
        (seconds % 3_600) / 60,
        language.text("m", "分"),
        seconds % 60,
        language.text("s", "秒")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clock_advances_without_progress_and_stale_samples_cannot_rewind_it() {
        let start = Instant::now();
        let mut clock = Clock {
            base_ms: 8_000,
            sampled_at: start,
        };
        let later = start + Duration::from_secs(12);
        assert_eq!(clock.elapsed_at(later), 20_000);
        clock.observe_at(9_000, later);
        assert_eq!(clock.elapsed_at(later), 20_000);
        clock.observe_at(24_000, later + Duration::from_secs(1));
        assert_eq!(clock.elapsed_at(later + Duration::from_secs(2)), 25_000);
    }

    #[test]
    fn duration_preserves_fast_scans_and_long_scan_units() {
        for (ms, english, chinese) in [
            (0, "0 ms", "0 毫秒"),
            (850, "850 ms", "850 毫秒"),
            (1_000, "1 s", "1 秒"),
            (59_999, "59 s", "59 秒"),
            (60_000, "1 m 0 s", "1 分 0 秒"),
            (3_723_456, "1 h 2 m 3 s", "1 小时 2 分 3 秒"),
        ] {
            assert_eq!(format_duration(ms, Language::English), english);
            assert_eq!(format_duration(ms, Language::Chinese), chinese);
        }
    }
}
