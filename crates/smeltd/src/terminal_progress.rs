//! OSC 9;4 进度 → `terminal.progress`。
//!
//! 进度只给人看，不进会话投影、不落盘。程序该每秒刷新一次（Ghostty 的建议），所以
//! 15 秒没新报告就当它走了、清掉，跟 Ghostty 一样；进度条还在时每 5 秒重发一次，后连上
//! 的客户端（重开的桌面、断线重连）不用等进度变化就能看到。

use std::time::{Duration, Instant};

use smelt_core::osc::ProgressReport;
use smelt_event_protocol::{TerminalProgress, TerminalProgressState};

const STALE_AFTER: Duration = Duration::from_secs(15);
const REPUBLISH_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct ProgressTracker {
    shown: Option<TerminalProgress>,
    /// 上一个报过的数值：出错、暂停不带数值时沿用它。
    value: u8,
    reported_at: Option<Instant>,
    published: Option<TerminalProgress>,
    published_at: Option<Instant>,
}

impl ProgressTracker {
    pub(crate) fn observe(&mut self, report: ProgressReport, now: Instant) {
        let progress = |state, percent| TerminalProgress { state, percent };
        self.shown = match report {
            ProgressReport::Remove => {
                self.value = 0;
                None
            }
            ProgressReport::Set(value) => {
                self.value = value;
                Some(progress(TerminalProgressState::Normal, Some(value)))
            }
            ProgressReport::Error(value) => {
                self.value = value.unwrap_or(self.value);
                Some(progress(TerminalProgressState::Error, Some(self.value)))
            }
            ProgressReport::Indeterminate => {
                Some(progress(TerminalProgressState::Indeterminate, None))
            }
            ProgressReport::Paused(value) => {
                self.value = value.unwrap_or(self.value);
                Some(progress(TerminalProgressState::Paused, Some(self.value)))
            }
        };
        self.reported_at = self.shown.is_some().then_some(now);
    }

    /// 这一刻要发出去的进度（`Some(None)` = 清掉）。变了就发；没变但进度条还在、离上次
    /// 发已经 5 秒，也重发一次。
    pub(crate) fn take_update(&mut self, now: Instant) -> Option<Option<TerminalProgress>> {
        if self
            .reported_at
            .is_some_and(|at| now.duration_since(at) >= STALE_AFTER)
        {
            self.shown = None;
            self.value = 0;
            self.reported_at = None;
        }
        let due = self.shown != self.published
            || (self.shown.is_some()
                && self
                    .published_at
                    .is_none_or(|at| now.duration_since(at) >= REPUBLISH_EVERY));
        if !due {
            return None;
        }
        self.published = self.shown;
        self.published_at = Some(now);
        Some(self.shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(percent: u8) -> Option<TerminalProgress> {
        Some(TerminalProgress {
            state: TerminalProgressState::Normal,
            percent: Some(percent),
        })
    }

    #[test]
    fn publishes_changes_and_nothing_while_idle() {
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        assert_eq!(tracker.take_update(start), None, "从没报过进度就什么都不发");

        tracker.observe(ProgressReport::Set(10), start);
        assert_eq!(tracker.take_update(start), Some(normal(10)));
        tracker.observe(ProgressReport::Set(10), start);
        assert_eq!(tracker.take_update(start), None, "同一个进度重复报不重发");

        tracker.observe(ProgressReport::Remove, start);
        assert_eq!(tracker.take_update(start), Some(None));
        assert_eq!(tracker.take_update(start + STALE_AFTER * 2), None);
    }

    #[test]
    fn error_and_pause_keep_the_last_value_when_none_is_given() {
        let now = Instant::now();
        let mut tracker = ProgressTracker::default();
        tracker.observe(ProgressReport::Set(60), now);
        tracker.observe(ProgressReport::Error(None), now);
        assert_eq!(
            tracker.take_update(now),
            Some(Some(TerminalProgress {
                state: TerminalProgressState::Error,
                percent: Some(60),
            }))
        );
        tracker.observe(ProgressReport::Indeterminate, now);
        tracker.observe(ProgressReport::Paused(None), now);
        assert_eq!(
            tracker.take_update(now),
            Some(Some(TerminalProgress {
                state: TerminalProgressState::Paused,
                percent: Some(60),
            }))
        );
    }

    #[test]
    fn republishes_while_shown_and_clears_when_the_program_goes_quiet() {
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        tracker.observe(ProgressReport::Set(30), start);
        assert_eq!(tracker.take_update(start), Some(normal(30)));
        assert_eq!(tracker.take_update(start + Duration::from_secs(1)), None);
        assert_eq!(
            tracker.take_update(start + REPUBLISH_EVERY),
            Some(normal(30)),
            "进度条还在时定期重发，后连上的客户端才看得到"
        );
        assert_eq!(
            tracker.take_update(start + STALE_AFTER),
            Some(None),
            "程序 15 秒没再报，当它走了"
        );
        assert_eq!(tracker.take_update(start + STALE_AFTER * 2), None);
    }
}
