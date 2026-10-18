//! OSC 7501 程序状态 → 终端通知。
//!
//! 只把需要人看的边沿（等你处理、完成、出错）变成一条信息性通知，走跟 OSC 9 同一条
//! `terminal.notification`。不改会话相位：OSC 是 PTY 里任何程序都能写的低可信来源，
//! 只能给人看（见 docs/notification-architecture.md）。

use smelt_core::osc::{ProgramBlockedKind, ProgramState, ProgramStatus};

/// 一个会话上最近一次 7501 根状态，用来去掉重复报告：程序重画时可能把同一状态再报一遍。
#[derive(Default)]
pub(crate) struct ProgramStatusNotices {
    last: Option<ProgramStatus>,
}

impl ProgramStatusNotices {
    /// 收到一条报告。跟上一条不同、而且需要人看时返回通知正文。
    pub(crate) fn observe(&mut self, status: ProgramStatus) -> Option<String> {
        if self.last.as_ref() == Some(&status) {
            return None;
        }
        let notice = notice_text(&status);
        self.last = (status.state != ProgramState::Clear).then_some(status);
        notice
    }
}

fn notice_text(status: &ProgramStatus) -> Option<String> {
    let label = match (status.state, status.kind) {
        (ProgramState::Blocked, Some(ProgramBlockedKind::Permission)) => "等你批准",
        (ProgramState::Blocked, Some(ProgramBlockedKind::Question)) => "等你回答",
        (ProgramState::Blocked, Some(ProgramBlockedKind::Auth)) => "等你登录",
        (ProgramState::Blocked, None) => "等你处理",
        (ProgramState::Done, _) => "已完成",
        (ProgramState::Error, _) => "出错了",
        (ProgramState::Idle | ProgramState::Working | ProgramState::Clear, _) => return None,
    };
    let body = match &status.message {
        Some(message) => format!("{label}：{message}"),
        None => label.to_string(),
    };
    Some(match &status.app {
        Some(app) => format!("{app} {body}"),
        None => body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(state: ProgramState) -> ProgramStatus {
        ProgramStatus {
            state,
            kind: None,
            app: Some("pi".to_string()),
            message: None,
        }
    }

    #[test]
    fn only_states_that_need_a_person_become_notices() {
        let mut notices = ProgramStatusNotices::default();
        assert_eq!(notices.observe(report(ProgramState::Working)), None);
        assert_eq!(
            notices.observe(ProgramStatus {
                kind: Some(ProgramBlockedKind::Permission),
                message: Some("运行 rm -rf build".to_string()),
                ..report(ProgramState::Blocked)
            }),
            Some("pi 等你批准：运行 rm -rf build".to_string())
        );
        assert_eq!(notices.observe(report(ProgramState::Working)), None);
        assert_eq!(
            notices.observe(report(ProgramState::Done)),
            Some("pi 已完成".to_string())
        );
        assert_eq!(notices.observe(report(ProgramState::Idle)), None);
        assert_eq!(
            notices.observe(ProgramStatus {
                app: None,
                message: Some("网络断了".to_string()),
                ..report(ProgramState::Error)
            }),
            Some("出错了：网络断了".to_string())
        );
    }

    #[test]
    fn repeated_reports_notify_once_but_a_new_run_notifies_again() {
        let mut notices = ProgramStatusNotices::default();
        assert!(notices.observe(report(ProgramState::Done)).is_some());
        assert_eq!(
            notices.observe(report(ProgramState::Done)),
            None,
            "重画时再报一遍同样的状态不算新事件"
        );
        assert_eq!(notices.observe(report(ProgramState::Working)), None);
        assert!(notices.observe(report(ProgramState::Done)).is_some());

        assert_eq!(notices.observe(report(ProgramState::Clear)), None);
        assert!(
            notices.observe(report(ProgramState::Done)).is_some(),
            "clear 之后同样的状态是新的一条"
        );
    }
}
