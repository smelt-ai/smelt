//! OSC 9;4 与 OSC 7501 进度 → `terminal.progress`。
//!
//! 进度只给人看，不进会话投影、不落盘。OSC 9;4 程序该每秒刷新一次（Ghostty 的建议），所以
//! 15 秒没新报告就当它走了、清掉，跟 Ghostty 一样；OSC 7501 由协议生命周期管理，不走 15 秒
//! 超时，一旦收到 7501 记录就抑制同期的 OSC 9;4，直到 `done` / `clear` / `RIS` 或新提示符。
//! 只在变化时发；后连上的客户端（重开的桌面、断线重连）由守护在订阅建立时补发当前进度
//! （`DaemonEventHub::replay_terminal_progress`）。

use std::time::{Duration, Instant};

use smelt_core::osc::{ProgramState, ProgramStatus, ProgressReport};
use smelt_event_protocol::{TerminalProgress, TerminalProgressState};

const STALE_AFTER: Duration = Duration::from_secs(15);

#[derive(Default)]
pub(crate) struct ProgressTracker {
    shown: Option<TerminalProgress>,
    /// 上一个报过的数值：出错、暂停不带数值时沿用它。
    value: u8,
    /// OSC 9;4 进度条显示着时才有；OSC 7501 进度不走超时。
    reported_at: Option<Instant>,
    from_program_status: bool,
    published: Option<TerminalProgress>,
}

impl ProgressTracker {
    pub(crate) fn observe(&mut self, report: ProgressReport, now: Instant) {
        if self.from_program_status {
            return;
        }
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

    pub(crate) fn observe_program_status(&mut self, status: &ProgramStatus) {
        match (status.state, status.progress) {
            (ProgramState::Working, Some(percent)) => {
                self.from_program_status = true;
                self.reported_at = None;
                self.shown = Some(TerminalProgress {
                    state: TerminalProgressState::Normal,
                    percent: Some(percent),
                });
            }
            (ProgramState::Blocked, Some(percent)) => {
                self.from_program_status = true;
                self.reported_at = None;
                self.shown = Some(TerminalProgress {
                    state: TerminalProgressState::Paused,
                    percent: Some(percent),
                });
            }
            _ => self.clear_program_status(),
        }
    }

    pub(crate) fn clear_program_status(&mut self) {
        if self.from_program_status {
            self.from_program_status = false;
            self.shown = None;
            self.reported_at = None;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.shown = None;
        self.value = 0;
        self.reported_at = None;
        self.from_program_status = false;
    }

    /// 泵最晚该在什么时候醒来调 [`Self::take_update`]：进度条还在就是它过期的那一刻，
    /// 没有进度条就不用为它醒。
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.reported_at.map(|at| at + STALE_AFTER)
    }

    /// 这一刻要发出去的进度（`Some(None)` = 清掉）。变了才发。
    pub(crate) fn take_update(&mut self, now: Instant) -> Option<Option<TerminalProgress>> {
        if self
            .reported_at
            .is_some_and(|at| now.duration_since(at) >= STALE_AFTER)
        {
            self.shown = None;
            self.value = 0;
            self.reported_at = None;
        }
        if self.shown == self.published {
            return None;
        }
        self.published = self.shown;
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
    fn clears_when_the_program_goes_quiet_without_republishing() {
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        tracker.observe(ProgressReport::Set(30), start);
        assert_eq!(tracker.take_update(start), Some(normal(30)));
        assert_eq!(
            tracker.take_update(start + Duration::from_secs(5)),
            None,
            "没变就不重发，后连上的客户端由订阅时补发"
        );
        assert_eq!(
            tracker.take_update(start + STALE_AFTER),
            Some(None),
            "程序 15 秒没再报，当它走了"
        );
        assert_eq!(tracker.take_update(start + STALE_AFTER * 2), None);
    }

    #[test]
    fn the_pump_only_wakes_for_a_shown_progress_bar() {
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        assert_eq!(tracker.next_deadline(), None, "没有进度条时一直等输出");

        tracker.observe(ProgressReport::Set(30), start);
        assert_eq!(tracker.next_deadline(), Some(start + STALE_AFTER));
        let later = start + Duration::from_secs(3);
        tracker.observe(ProgressReport::Set(40), later);
        assert_eq!(
            tracker.next_deadline(),
            Some(later + STALE_AFTER),
            "新的报告把过期往后推"
        );

        tracker.observe(ProgressReport::Remove, later);
        assert_eq!(tracker.next_deadline(), None, "清掉之后不再为它醒");

        tracker.observe(ProgressReport::Indeterminate, later);
        let _ = tracker.take_update(later + STALE_AFTER);
        assert_eq!(tracker.next_deadline(), None, "过期清掉之后不再为它醒");
    }

    #[test]
    fn program_status_progress_suppresses_conemu_and_skips_stale_timeout() {
        let start = Instant::now();
        let mut tracker = ProgressTracker::default();
        let working_40 = ProgramStatus {
            state: ProgramState::Working,
            kind: None,
            progress: Some(40),
            message: None,
        };
        tracker.observe_program_status(&working_40);
        assert_eq!(tracker.next_deadline(), None, "7501 进度不设 15s 超时");
        assert_eq!(tracker.take_update(start), Some(normal(40)));

        // 同期的 OSC 9;4 被 7501 抑制，也不产生过期计时器。
        tracker.observe(ProgressReport::Set(99), start + Duration::from_secs(1));
        assert_eq!(tracker.take_update(start + Duration::from_secs(20)), None);
        assert_eq!(tracker.next_deadline(), None);

        // blocked 切为 Paused；无 progress 时清掉并恢复放行 OSC 9;4。
        tracker.observe_program_status(&ProgramStatus {
            state: ProgramState::Blocked,
            kind: None,
            progress: Some(40),
            message: None,
        });
        assert_eq!(
            tracker.take_update(start),
            Some(Some(TerminalProgress {
                state: TerminalProgressState::Paused,
                percent: Some(40),
            }))
        );
        tracker.observe_program_status(&ProgramStatus {
            state: ProgramState::Done,
            kind: None,
            progress: None,
            message: None,
        });
        assert_eq!(tracker.take_update(start), Some(None));

        tracker.observe(ProgressReport::Set(15), start);
        assert_eq!(tracker.take_update(start), Some(normal(15)));

        // RIS 复位立即清掉正在显示的进度。
        tracker.reset();
        assert_eq!(tracker.take_update(start), Some(None));
    }
}
