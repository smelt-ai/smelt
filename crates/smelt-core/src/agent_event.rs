//! CLI agent hooks 归一化后的稳定事件协议。
//!
//! provider 适配器只负责把各家的 hook 名称/字段翻译成这里的语义事件；smeltd
//! 再据此归约会话状态。协议带版本号，避免 helper 与守护升级不同步时静默误判。

/// daemon 当前理解的最高事件版本。既有生命周期事件继续按 v1 发送，保证新版
/// helper 遇到尚未升级的 daemon 时仍能被归约；v2 只用于新增的纯标题事件。
pub const AGENT_EVENT_VERSION: u32 = 2;
pub const AGENT_EVENT_MIN_VERSION: u32 = 1;

/// Agent hook/extension helper 的稳定路径。GUI 会把 App 内同版本二进制同步到这里，
/// daemon 再把绝对路径传给进程级集成，避免依赖 Dock 启动时不完整的 PATH。
pub fn notify_executable_path() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| "/tmp/.smelt".into())
        .join("bin")
        .join("smelt-notify")
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentEvent {
    pub version: u32,
    pub provider: String,
    pub kind: AgentEventKind,
    /// 会话标题。PromptSubmitted 携带从首条用户请求生成的本地兜底；
    /// SessionTitleChanged 携带 provider 发布的权威标题。它与 `message` 分开：
    /// 后者描述工具/审批状态，不能把标题错投影成 pending question。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_title: Option<String>,
    /// provider 自己的对话 id（Copilot `sessionId`、Claude `session_id` …）。
    ///
    /// 它标识「这个终端此刻在跟哪一段对话说话」，与 smelt 的终端会话 id 正交：
    /// 用户在 TUI 里退出、`/resume` 到别的对话，同一个 PTY 会换 id。每条 hook
    /// 都带它，因此绑定是自愈的，不依赖某一次握手。
    ///
    /// 纯附加元数据，不参与相位归约，所以**不抬事件版本**——新 helper 配旧
    /// daemon 时，旧 daemon 忽略该字段即可，其余事件不受影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

impl AgentEvent {
    pub fn new(provider: impl Into<String>, kind: AgentEventKind) -> Self {
        Self {
            version: if kind == AgentEventKind::SessionTitleChanged {
                AGENT_EVENT_VERSION
            } else {
                AGENT_EVENT_MIN_VERSION
            },
            provider: provider.into(),
            kind,
            conversation_title: None,
            conversation_id: None,
            message: None,
            tool_name: None,
            tool_use_id: None,
            agent_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted,
    /// 只更新会话元数据，不是回合边沿，也不得改变 phase。
    SessionTitleChanged,
    PromptSubmitted,
    ToolStarted,
    ToolFinished,
    ToolFailed,
    ApprovalRequested,
    InputRequested,
    SubagentStarted,
    SubagentStopped,
    TurnSucceeded,
    TurnFailed,
    SessionEnded,
}

/// 回合占用意图。线协议是 [`AgentEventKind`]；占用是归约唯一入口。
///
/// 适配器把各家 hook 翻成 kind，reducer 只按占用改相位。加 kind 必须先归到
/// 这里，否则编不过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    /// SessionStart / 改名：不改相位。
    Metadata,
    /// 用户 prompt：任何相位都能打开回合，包括 Dead。
    OpenTurn,
    /// 工具或子任务开始。Dead 不能复活。
    /// `follow_matched_wait`：等待中仅当 blocker 匹配才放行（工具）；子任务一律挡住。
    ResumeWork {
        follow_matched_wait: bool,
    },
    /// 回合内进度（工具结束、子任务结束）。已关闭的回合不重开。
    Progress {
        follow_matched_wait: bool,
    },
    Wait {
        approval: bool,
    },
    Finish {
        failed: bool,
    },
    EndProcess,
}

impl Occupancy {
    pub fn records_turn(self) -> bool {
        !matches!(self, Self::Metadata)
    }

    pub fn allows_phase_change(
        self,
        dead: bool,
        closed: bool,
        finished: bool,
        waiting: bool,
        matching_blocker: bool,
    ) -> bool {
        match self {
            Self::Metadata => false,
            Self::OpenTurn => true,
            Self::ResumeWork {
                follow_matched_wait,
            } => !dead && (!waiting || (follow_matched_wait && matching_blocker)),
            Self::Progress {
                follow_matched_wait,
            } => !closed && (!waiting || (follow_matched_wait && matching_blocker)),
            Self::Wait { .. } | Self::Finish { .. } => !closed,
            Self::EndProcess => !finished,
        }
    }
}

impl AgentEventKind {
    pub fn occupancy(self) -> Occupancy {
        match self {
            Self::SessionStarted | Self::SessionTitleChanged => Occupancy::Metadata,
            Self::PromptSubmitted => Occupancy::OpenTurn,
            Self::ToolStarted => Occupancy::ResumeWork {
                follow_matched_wait: true,
            },
            Self::SubagentStarted => Occupancy::ResumeWork {
                follow_matched_wait: false,
            },
            Self::ToolFinished | Self::ToolFailed => Occupancy::Progress {
                follow_matched_wait: true,
            },
            Self::SubagentStopped => Occupancy::Progress {
                follow_matched_wait: false,
            },
            Self::ApprovalRequested => Occupancy::Wait { approval: true },
            Self::InputRequested => Occupancy::Wait { approval: false },
            Self::TurnSucceeded => Occupancy::Finish { failed: false },
            Self::TurnFailed => Occupancy::Finish { failed: true },
            Self::SessionEnded => Occupancy::EndProcess,
        }
    }

    pub fn is_turn_event(self) -> bool {
        self.occupancy().records_turn()
    }
}

#[cfg(test)]
mod turn_event_tests {
    use super::{AGENT_EVENT_VERSION, AgentEvent, AgentEventKind, Occupancy};

    #[test]
    fn occupancy_is_the_kind_translation() {
        use Occupancy::*;
        let check = |kind: AgentEventKind, expected: Occupancy| {
            assert_eq!(kind.occupancy(), expected, "{kind:?}");
            assert_eq!(kind.is_turn_event(), expected.records_turn(), "{kind:?}");
        };
        check(AgentEventKind::SessionStarted, Metadata);
        check(AgentEventKind::SessionTitleChanged, Metadata);
        check(AgentEventKind::PromptSubmitted, OpenTurn);
        check(
            AgentEventKind::ToolStarted,
            ResumeWork {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::SubagentStarted,
            ResumeWork {
                follow_matched_wait: false,
            },
        );
        check(
            AgentEventKind::ToolFinished,
            Progress {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::ToolFailed,
            Progress {
                follow_matched_wait: true,
            },
        );
        check(
            AgentEventKind::SubagentStopped,
            Progress {
                follow_matched_wait: false,
            },
        );
        check(AgentEventKind::ApprovalRequested, Wait { approval: true });
        check(AgentEventKind::InputRequested, Wait { approval: false });
        check(AgentEventKind::TurnSucceeded, Finish { failed: false });
        check(AgentEventKind::TurnFailed, Finish { failed: true });
        check(AgentEventKind::SessionEnded, EndProcess);
    }

    #[test]
    fn v1_payload_without_conversation_title_remains_compatible() {
        let event: AgentEvent =
            serde_json::from_str(r#"{"version":1,"provider":"codex","kind":"prompt_submitted"}"#)
                .unwrap();
        assert_eq!(event.conversation_title, None);
    }

    #[test]
    fn only_the_new_title_event_requires_v2_on_the_wire() {
        assert_eq!(
            AgentEvent::new("codex", AgentEventKind::PromptSubmitted).version,
            1
        );
        assert_eq!(
            AgentEvent::new("opencode", AgentEventKind::SessionTitleChanged).version,
            AGENT_EVENT_VERSION
        );
    }
}
