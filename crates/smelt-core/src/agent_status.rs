//! 会话里 agent 的状态：总览页状态徽章、侧栏会话行状态点共用同一份枚举。
//! 借鉴 codex 的 ThreadStatus 细分：「需要处理」不再一锅烩，等审批和一般等待
//! 是不同等级的行动召唤。排列顺序即优先级（值越小越靠前 / 越紧急）。

use crate::daemon_state::DaemonPhase;

/// 值 GPUI 无关，纯状态判断——UI 层（`ui_theme`/侧栏）按它上色，不掺渲染逻辑。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    /// Claude 等你批准操作（通知文本含 permission/权限等）→ 最高优先，红色。
    WaitingApproval,
    /// 其他需要处理：等输入或失败 → 橙色。普通 BEL/OSC 通知不进入 Agent 状态。
    NeedsAttention,
    /// 标题以 Braille spinner 开头 → 运行中，蓝色。
    Running,
    /// 任务刚完成、你还没回应过 → 「有结果可看」，绿色。
    Done,
    /// 其余 → 空闲，灰色。
    Idle,
}

impl AgentStatus {
    /// 将守护状态压缩成 UI 五态。`Idle`/`Dead` 没有需要展示的 agent 活动，
    /// 因而返回 `None`，调用方可继续使用未读完成或标题 spinner fallback。
    pub fn from_daemon_phase(phase: DaemonPhase) -> Option<Self> {
        match phase {
            DaemonPhase::AwaitingApproval => Some(Self::WaitingApproval),
            DaemonPhase::WaitingForUser | DaemonPhase::Failed => Some(Self::NeedsAttention),
            DaemonPhase::Thinking | DaemonPhase::ExecutingTool => Some(Self::Running),
            DaemonPhase::Succeeded => Some(Self::Done),
            DaemonPhase::Idle | DaemonPhase::Dead => None,
        }
    }

    /// 优先级序（越小越紧急），与声明序一致：排序、聚合（项目 rail 的组内
    /// 最高优先级状态点）共用。
    pub fn rank(self) -> u8 {
        match self {
            AgentStatus::WaitingApproval => 0,
            AgentStatus::NeedsAttention => 1,
            AgentStatus::Running => 2,
            AgentStatus::Done => 3,
            AgentStatus::Idle => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentStatus;
    use crate::daemon_state::DaemonPhase;

    #[test]
    fn daemon_phase_mapping_covers_every_phase() {
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::AwaitingApproval),
            Some(AgentStatus::WaitingApproval)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::WaitingForUser),
            Some(AgentStatus::NeedsAttention)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Failed),
            Some(AgentStatus::NeedsAttention)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Thinking),
            Some(AgentStatus::Running)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::ExecutingTool),
            Some(AgentStatus::Running)
        );
        assert_eq!(
            AgentStatus::from_daemon_phase(DaemonPhase::Succeeded),
            Some(AgentStatus::Done)
        );
        assert_eq!(AgentStatus::from_daemon_phase(DaemonPhase::Idle), None);
        assert_eq!(AgentStatus::from_daemon_phase(DaemonPhase::Dead), None);
    }
}
