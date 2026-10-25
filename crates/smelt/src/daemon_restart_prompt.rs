//! 会停掉本机守护进程的确认文案。
//!
//! 没有正在进行的会话时不弹窗。有的话列出名字，确认按钮用警示样式。

use smelt_model::daemon_state::{DaemonPhase, DaemonSessionState};

pub(crate) const NAME_LIMIT: usize = 8;

pub(crate) const UPDATE_SENTENCE: &str =
    "更新将重启后台守护进程，正在进行的对话本轮回复会被中断（历史记录已保存，重启后可继续对话）";

pub(crate) const RESTART_SENTENCE: &str = "这次重启会关掉后台守护进程，正在进行的对话本轮回复会被中断（历史记录已保存，重启后可继续对话）";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonStopReason {
    UpdateInstall,
    QuitToInstall,
    ExplicitRestart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaemonStopPrompt {
    pub title: &'static str,
    pub body: String,
    pub confirm: &'static str,
    pub danger: bool,
}

/// 本机正在流式输出或执行的会话。空闲 shell、等用户输入的会话不算。
pub(crate) fn busy_work_names(states: &[DaemonSessionState]) -> Vec<String> {
    states
        .iter()
        .filter(|state| {
            state.has_runtime()
                && matches!(
                    state.effective_phase(),
                    DaemonPhase::Thinking | DaemonPhase::ExecutingTool | DaemonPhase::Connecting
                )
        })
        .map(|state| {
            state
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or(state.id.as_str())
                .to_string()
        })
        .collect()
}

pub(crate) fn should_confirm(names: &[String]) -> bool {
    !names.is_empty()
}

pub(crate) fn daemon_stop_prompt(reason: DaemonStopReason, names: &[String]) -> DaemonStopPrompt {
    let (title, sentence, confirm) = match reason {
        DaemonStopReason::UpdateInstall => ("重启以完成更新？", UPDATE_SENTENCE, "重启并更新"),
        DaemonStopReason::QuitToInstall => ("退出并安装更新？", UPDATE_SENTENCE, "退出并更新"),
        DaemonStopReason::ExplicitRestart => ("确定重启守护进程吗？", RESTART_SENTENCE, "确定重启"),
    };
    let mut body = sentence.to_string();
    if !names.is_empty() {
        let shown = names.len().min(NAME_LIMIT);
        let mut listed = names[..shown].join("、");
        if names.len() > shown {
            listed.push_str(&format!(" 等 {} 个", names.len()));
        }
        body.push_str(&format!(
            "\n当前有 {} 个会话正在进行：{listed}。",
            names.len()
        ));
    }
    DaemonStopPrompt {
        title,
        body,
        confirm,
        danger: !names.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(
        id: &str,
        title: Option<&str>,
        phase: DaemonPhase,
        runtime: bool,
    ) -> DaemonSessionState {
        DaemonSessionState {
            id: id.to_string(),
            title: title.map(str::to_string),
            phase,
            runtime,
            turn_events: true,
            ..DaemonSessionState::default()
        }
    }

    #[test]
    fn idle_sessions_do_not_ask() {
        let states = vec![
            state("idle", Some("空闲"), DaemonPhase::Idle, true),
            state("ghost", Some("幽灵"), DaemonPhase::Thinking, false),
            state("wait", Some("等输入"), DaemonPhase::WaitingForUser, true),
        ];
        let names = busy_work_names(&states);
        assert!(names.is_empty());
        assert!(!should_confirm(&names));
        let prompt = daemon_stop_prompt(DaemonStopReason::QuitToInstall, &names);
        assert!(!prompt.danger);
        assert_eq!(prompt.confirm, "退出并更新");
        assert!(prompt.body.contains(UPDATE_SENTENCE));
    }

    #[test]
    fn busy_sessions_are_listed_and_the_button_is_danger() {
        let mut states = vec![
            state("a", Some("写文档"), DaemonPhase::Thinking, true),
            state("b", None, DaemonPhase::ExecutingTool, true),
            state("c", Some("  "), DaemonPhase::Connecting, true),
        ];
        for index in 0..8 {
            states.push(state(
                &format!("extra-{index}"),
                Some(&format!("会话{index}")),
                DaemonPhase::Thinking,
                true,
            ));
        }
        let names = busy_work_names(&states);
        assert!(should_confirm(&names));
        assert_eq!(names[0], "写文档");
        assert_eq!(names[1], "b");
        assert_eq!(names[2], "c");
        let prompt = daemon_stop_prompt(DaemonStopReason::ExplicitRestart, &names);
        assert!(prompt.danger);
        assert_eq!(prompt.confirm, "确定重启");
        assert!(prompt.body.contains(RESTART_SENTENCE));
        assert!(prompt.body.contains("写文档"));
        assert!(prompt.body.contains(&format!("等 {} 个", names.len())));
        assert!(!prompt.body.contains("extra-6"));
    }
}
