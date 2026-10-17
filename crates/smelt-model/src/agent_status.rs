//! 会话里 agent 的状态：侧栏、菜单栏、移动端指挥台共用。
//!
//! 只有三态。彩色只留给「要你」；运行中靠蓝；其余全是空闲灰。
//! 刚完成、断连、无活动都算空闲——未读用 `unread` / attention，不占一态。

use crate::acp_session::AcpTurnOutcome;
use crate::daemon_state::{DaemonPhase, DaemonSessionState};

/// 值 GPUI 无关，纯状态判断——UI 层按它上色，不掺渲染逻辑。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    /// 要你：等批准、等输入、失败。
    NeedsYou,
    /// 正在跑。
    Running,
    /// 空闲：无活动、刚完成、运行时已丢。
    Idle,
}

/// ACP 本地协议快照里与三态提示有关的事实。
///
/// view 只负责采集这些事实；运行窗口、失败结果和相位优先级都在 core 里解释，
/// 避免侧栏、对话页或后续消费者各写一套边界判断。
#[derive(Clone, Copy, Debug)]
pub struct AcpStatusEvidence {
    pub phase: DaemonPhase,
    pub turn_outcome: Option<AcpTurnOutcome>,
    pub prompt_dispatch_pending: bool,
    pub immediate_cancel_pending: bool,
    pub fresh_start_pending: bool,
    /// 消息流里是否还有 Pending/InProgress 工具。只还原现场；回合结束后
    /// 不能单独把会话画成运行中。
    pub has_unfinished_tool: bool,
}

impl AcpStatusEvidence {
    /// 对话里用户能看见的活动：当前执行、派发窗口或新会话首轮。
    ///
    /// 回合已经结束后，adapter 欠下的工具终态只留在工具卡上，不能把侧栏/
    /// 菜单栏钉在运行蓝。Connecting 中的未结束工具来自历史回放，同样不是当前回合。
    pub fn is_visibly_running(self) -> bool {
        if matches!(self.phase, DaemonPhase::Dead) {
            return false;
        }
        matches!(
            self.phase,
            DaemonPhase::Thinking | DaemonPhase::ExecutingTool
        ) || self.fresh_start_pending
            || self.prompt_dispatch_pending
            || self.immediate_cancel_pending
    }

    /// 将 ACP 本地协议快照压成共享三态，供守护镜像尚未到达时兜底。
    pub fn status(self) -> AgentStatus {
        if matches!(self.phase, DaemonPhase::Dead) {
            return AgentStatus::Idle;
        }
        let phase = AgentStatus::from_daemon_phase(self.phase).unwrap_or(AgentStatus::Idle);
        let outcome = if matches!(self.phase, DaemonPhase::Idle)
            && self
                .turn_outcome
                .is_some_and(|outcome| outcome.failure_message().is_some())
        {
            AgentStatus::NeedsYou
        } else {
            AgentStatus::Idle
        };
        let visible = if self.is_visibly_running() {
            AgentStatus::Running
        } else {
            AgentStatus::Idle
        };
        AgentStatus::highest([phase, outcome, visible])
    }
}

/// 将单个 Terminal pane 的守护结构化回合状态压成共享三态。
/// 终端标题只是展示元数据，不能参与状态推断。
pub fn terminal_agent_status(daemon: Option<&DaemonSessionState>) -> AgentStatus {
    daemon
        .and_then(AgentStatus::from_daemon_state)
        .unwrap_or(AgentStatus::Idle)
}

impl AgentStatus {
    /// 将守护相位压成三态。`Idle`/`Dead`/`Succeeded` 没有需要展示的活动，
    /// 返回 `None`。
    pub fn from_daemon_phase(phase: DaemonPhase) -> Option<Self> {
        match phase {
            DaemonPhase::AwaitingApproval | DaemonPhase::WaitingForUser | DaemonPhase::Failed => {
                Some(Self::NeedsYou)
            }
            DaemonPhase::Thinking | DaemonPhase::ExecutingTool => Some(Self::Running),
            DaemonPhase::Succeeded
            | DaemonPhase::Idle
            | DaemonPhase::Dead
            | DaemonPhase::Connecting => None,
        }
    }

    /// 运行时已丢或没有回合级结构化来源时，相位不能当成当前状态。
    pub fn from_daemon_state(state: &DaemonSessionState) -> Option<Self> {
        if !state.has_runtime() || !state.phase_is_authoritative() {
            return None;
        }
        Self::from_daemon_phase(state.effective_phase())
    }

    /// 优先级序（越小越紧急）。
    pub fn rank(self) -> u8 {
        match self {
            AgentStatus::NeedsYou => 0,
            AgentStatus::Running => 1,
            AgentStatus::Idle => 2,
        }
    }

    /// 聚合任意状态来源或子 pane，取最高优先级：要你 > 运行中 > 空闲。
    /// 空迭代器按空闲处理，方便会话恢复期间布局树暂时没有叶子时直接使用。
    pub fn highest(statuses: impl IntoIterator<Item = Self>) -> Self {
        statuses
            .into_iter()
            .min_by_key(|status| status.rank())
            .unwrap_or(Self::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::{AcpStatusEvidence, AgentStatus, terminal_agent_status};
    use crate::acp_session::AcpTurnOutcome;
    use crate::daemon_state::{DaemonPhase, DaemonSessionState};

    fn acp_evidence(phase: DaemonPhase) -> AcpStatusEvidence {
        AcpStatusEvidence {
            phase,
            turn_outcome: None,
            prompt_dispatch_pending: false,
            immediate_cancel_pending: false,
            fresh_start_pending: false,
            has_unfinished_tool: false,
        }
    }

    #[test]
    fn daemon_phase_mapping_covers_every_phase() {
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::AwaitingApproval),
            Some(AgentStatus::NeedsYou)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::WaitingForUser),
            Some(AgentStatus::NeedsYou)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Failed),
            Some(AgentStatus::NeedsYou)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Thinking),
            Some(AgentStatus::Running)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::ExecutingTool),
            Some(AgentStatus::Running)
        );
        assert_eq!(AgentStatus::from_daemon_phase(DaemonPhase::Succeeded), None);
        assert_eq!(AgentStatus::from_daemon_phase(DaemonPhase::Idle), None);
        assert_eq!(AgentStatus::from_daemon_phase(DaemonPhase::Dead), None);
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Connecting),
            None
        );
    }

    #[test]
    fn missing_runtime_is_idle_even_if_last_phase_was_waiting() {
        let mut state = crate::daemon_state::DaemonSessionState {
            phase: DaemonPhase::AwaitingApproval,
            runtime: false,
            turn_events: true,
            ..Default::default()
        };
        assert_eq!(AgentStatus::from_daemon_state(&state), None);
        state.runtime = true;
        assert_eq!(
            AgentStatus::from_daemon_state(&state),
            Some(AgentStatus::NeedsYou)
        );
    }

    #[test]
    fn terminal_status_requires_a_verified_turn_event() {
        assert_eq!(terminal_agent_status(None), AgentStatus::Idle);

        let title_only = DaemonSessionState {
            phase: DaemonPhase::Thinking,
            runtime: true,
            title: Some("Thinking - TSLA 趋势且入场与 put 判断 - grok".into()),
            structured_events: true,
            ..Default::default()
        };
        assert_eq!(
            terminal_agent_status(Some(&title_only)),
            AgentStatus::Idle,
            "没有回合级结构化来源时，标题和旧 daemon 的 phase 都不能染色"
        );

        let authoritative_thinking = DaemonSessionState {
            phase: DaemonPhase::Thinking,
            runtime: true,
            turn_events: true,
            ..Default::default()
        };
        assert_eq!(
            terminal_agent_status(Some(&authoritative_thinking)),
            AgentStatus::Running
        );

        let awaiting_user = DaemonSessionState {
            phase: DaemonPhase::WaitingForUser,
            runtime: true,
            turn_events: true,
            ..Default::default()
        };
        assert_eq!(
            terminal_agent_status(Some(&awaiting_user)),
            AgentStatus::NeedsYou
        );

        // 侧栏蓝点只看权威 phase。标题写着 Thinking 不算；回合还在 Thinking 就必须是蓝。
        let mid_turn_thinking = DaemonSessionState {
            phase: DaemonPhase::Thinking,
            runtime: true,
            turn_events: true,
            title: Some("Thinking - Smelt 中途 - grok".into()),
            ..Default::default()
        };
        assert_eq!(
            terminal_agent_status(Some(&mid_turn_thinking)),
            AgentStatus::Running,
            "回合还在进行时，侧栏必须是运行蓝，不能因为又来了一帧 SessionStart 或标题变化而变灰"
        );
    }

    #[test]
    fn acp_status_covers_dispatch_tool_cleanup_failure_and_dead_runtime() {
        let mut evidence = acp_evidence(DaemonPhase::Idle);
        evidence.prompt_dispatch_pending = true;
        assert!(evidence.is_visibly_running());
        assert_eq!(evidence.status(), AgentStatus::Running);

        evidence.prompt_dispatch_pending = false;
        evidence.has_unfinished_tool = true;
        assert!(!evidence.is_visibly_running());
        assert_eq!(
            evidence.status(),
            AgentStatus::Idle,
            "回合已结束后，未收尾工具不能把会话钉在运行蓝"
        );

        evidence.has_unfinished_tool = false;
        evidence.turn_outcome = Some(AcpTurnOutcome::MaxTokens);
        assert_eq!(evidence.status(), AgentStatus::NeedsYou);

        evidence.phase = DaemonPhase::Dead;
        evidence.has_unfinished_tool = true;
        assert!(!evidence.is_visibly_running());
        assert_eq!(
            evidence.status(),
            AgentStatus::Idle,
            "运行时已结束后，历史失败和未收尾工具都不能继续染色"
        );
    }

    #[test]
    fn ended_turn_with_dangling_tools_is_not_running() {
        let mut evidence = acp_evidence(DaemonPhase::Idle);
        evidence.has_unfinished_tool = true;
        evidence.turn_outcome = Some(AcpTurnOutcome::Succeeded);
        assert!(!evidence.is_visibly_running());
        assert_eq!(evidence.status(), AgentStatus::Idle);

        evidence.turn_outcome = Some(AcpTurnOutcome::Cancelled);
        assert_eq!(evidence.status(), AgentStatus::Idle);

        evidence.turn_outcome = Some(AcpTurnOutcome::Failed);
        assert_eq!(
            evidence.status(),
            AgentStatus::NeedsYou,
            "失败结果仍要压过悬空工具"
        );

        evidence = acp_evidence(DaemonPhase::Succeeded);
        evidence.has_unfinished_tool = true;
        assert!(!evidence.is_visibly_running());
        assert_eq!(evidence.status(), AgentStatus::Idle);

        evidence = acp_evidence(DaemonPhase::ExecutingTool);
        evidence.has_unfinished_tool = true;
        assert!(evidence.is_visibly_running());
        assert_eq!(evidence.status(), AgentStatus::Running);

        let daemon = DaemonSessionState {
            phase: DaemonPhase::Succeeded,
            runtime: true,
            turn_events: true,
            ..Default::default()
        };
        let daemon_status = AgentStatus::from_daemon_state(&daemon).unwrap_or(AgentStatus::Idle);
        let mut view = acp_evidence(DaemonPhase::Idle);
        view.has_unfinished_tool = true;
        view.turn_outcome = Some(AcpTurnOutcome::Succeeded);
        assert_eq!(
            AgentStatus::highest([daemon_status, view.status()]),
            AgentStatus::Idle,
            "守护完成边沿和本地悬空工具聚合后也不能再显示运行中"
        );
    }

    #[test]
    fn acp_status_uses_shared_priority_and_ignores_history_during_connecting() {
        let mut evidence = acp_evidence(DaemonPhase::WaitingForUser);
        evidence.has_unfinished_tool = true;
        assert_eq!(evidence.status(), AgentStatus::NeedsYou);

        evidence = acp_evidence(DaemonPhase::Succeeded);
        evidence.has_unfinished_tool = true;
        assert_eq!(evidence.status(), AgentStatus::Idle);

        evidence = acp_evidence(DaemonPhase::Connecting);
        evidence.has_unfinished_tool = true;
        assert!(!evidence.is_visibly_running());
        assert_eq!(evidence.status(), AgentStatus::Idle);
    }

    #[test]
    fn highest_status_considers_every_terminal_pane() {
        assert_eq!(AgentStatus::highest([]), AgentStatus::Idle);
        assert_eq!(
            AgentStatus::highest([AgentStatus::Idle, AgentStatus::Running, AgentStatus::Idle,]),
            AgentStatus::Running,
            "后台 pane 正在运行时，父会话不能仍显示空闲"
        );
        assert_eq!(
            AgentStatus::highest([
                AgentStatus::Running,
                AgentStatus::NeedsYou,
                AgentStatus::Idle,
            ]),
            AgentStatus::NeedsYou,
            "任一 pane 需要用户处理时必须覆盖运行态"
        );
    }
}
