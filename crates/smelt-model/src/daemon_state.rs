//! 运行时推给客户端的会话状态和事件：会话的相位、标题、需要关注的问题，远程目录、
//! 菜单、自动化投影的替换，以及断线重连的退避节奏。订阅请求怎么写、回来的行怎么
//! 解在 [`DaemonStateEventClient`]；连接本身在客户端。

use std::collections::HashSet;
use std::time::Duration;

pub use smelt_event_protocol::{TerminalProgress, TerminalProgressState};

use crate::automation::AutomationFile;
use crate::session_control::RemoteSessionSnapshot;
use crate::workspace_directory::WorkspaceDirectory;
use crate::workspace_menu::WorkspaceMenuSnapshot;

mod event_client;

pub use event_client::{DaemonEventClientError, DaemonStateEventClient, SubscriptionShape};

/// 断线后快速首试、连续失败逐步退让，避免 daemon 正在升级/尚未启动时各个客户端
/// 同步地每两秒撞一次 socket。上限保证守护恢复后不会长期不可见。
const DAEMON_RECONNECT_BASE: Duration = Duration::from_millis(250);

const DAEMON_RECONNECT_MAX: Duration = Duration::from_secs(8);

pub fn daemon_reconnect_backoff(attempt: u32) -> Duration {
    let multiplier = 1u128 << attempt.min(16);
    let millis = DAEMON_RECONNECT_BASE
        .as_millis()
        .saturating_mul(multiplier)
        .min(DAEMON_RECONNECT_MAX.as_millis());
    Duration::from_millis(millis.min(u64::MAX as u128) as u64)
}

/// 当前 epoch 的快照里，一个会话的运行时处在哪种状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePresence {
    /// 有活着的运行时，按 id attach。
    Live,
    /// 守护停过（崩溃、重启）后从会话目录恢复的条目：身份还在，进程没了。
    /// 按原 id 重新打开，守护会把能续接的 CLI 接回原来的对话。
    LeftOver,
    /// 快照里没有这个 id：shell 已经退出，不再接。
    Gone,
}

/// GUI 侧订阅状态通道的镜像（serde 反序列化；多出来的 JSON 字段自动忽略）。
/// 字段对齐 smeltd `SessionState` 广播——B 路线「语义面板」的数据源。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct DaemonSessionState {
    pub id: String,
    /// daemon runtime generation. A recreated session may restart its revision at a lower value.
    #[serde(default)]
    pub generation: u64,
    /// 当前 daemon 进程内的单调状态版本；同一订阅连接上只接受递增值。
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub phase: DaemonPhase,
    /// hook 上报的问句 / 当前工具名（PreToolUse 时 question 常是 tool_name）。
    #[serde(default)]
    pub pending_question: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// 守护下发的启动命令；远程客户端在菜单快照缺少 agent 信息时用它兜底识别 provider。
    #[serde(default)]
    pub launch: Option<String>,
    /// 结构化 hook 实际检测到的 provider；明确的启动类型仍由 UI 优先采用。
    #[serde(default)]
    pub provider: Option<String>,
    /// 该终端此刻正在对话的 provider 会话 id（由每条 hook 携带）。UI 用它把侧栏
    /// 会话对上 provider 自己的历史存档：重命名写进历史标题覆盖层，显示名也跟着
    /// 对话切换走。旧守护不报该字段时为 None，退化成纯终端标题语义。
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// unix 秒，进入当前 phase 的时刻。
    #[serde(default)]
    pub phase_since: u64,
    /// unix 秒，守护最后一次更新该会话状态的时刻。
    #[serde(default)]
    pub updated_at: u64,
    /// 该 pane 是否已经收到 hook/ACP 结构化事件。SessionStart 就会置位，用于
    /// 判断低可信终端通知是否需要抑制。
    #[serde(default)]
    pub structured_events: bool,
    /// 是否已收到回合级结构化事件（prompt / tool / 审批 / 终态）。SessionStart
    /// 单独不会置位；旧守护缺字段时为 false，客户端必须对活动状态 fail closed。
    #[serde(default)]
    pub turn_events: bool,
    /// 会话是否仍有客户端连接（list op 附带）；会话管理器据此展示在线/游离状态。
    /// 未连接不等于应删除：GUI 退出后终端与 ACP 会话可以继续由 daemon 托管。旧版
    /// 守护不报该字段，反序列化默认 true，避免把未知状态误判为游离。
    #[serde(default = "default_connected")]
    pub connected: bool,
    /// PTY `open` 的交互 attachment 数。ACP/旧版 daemon 没有该字段时为 0；结合
    /// `connected` 与 watcher 数可区分“桌面渲染层在线”和“仅移动旁观层在线”。
    #[serde(default)]
    pub interactive_connections: usize,
    /// PTY `watch` 的只读订阅数；移动端终端画面目前使用这条输出通道。
    #[serde(default)]
    pub watcher_connections: usize,
    /// 本进程是否仍持有 PTY/ACP 运行时。与 `connected`（有没有客户端 attach）正交：
    /// 崩溃恢复的幽灵会话 `runtime=false`，旧守护缺字段时默认 true，避免被误判成幽灵。
    #[serde(default = "default_runtime")]
    pub runtime: bool,
}

fn default_connected() -> bool {
    true
}

fn default_runtime() -> bool {
    true
}

impl Default for DaemonSessionState {
    fn default() -> Self {
        Self {
            id: String::new(),
            generation: 0,
            revision: 0,
            phase: DaemonPhase::default(),
            pending_question: None,
            title: None,
            launch: None,
            provider: None,
            conversation_id: None,
            cwd: None,
            phase_since: 0,
            updated_at: 0,
            structured_events: false,
            turn_events: false,
            connected: true,
            interactive_connections: 0,
            watcher_connections: 0,
            runtime: true,
        }
    }
}

impl DaemonSessionState {
    /// 结构面板用：phase 中文标签。
    pub fn phase_label(&self) -> &'static str {
        match self.effective_phase() {
            DaemonPhase::Connecting => "启动中",
            DaemonPhase::Thinking => "思考中",
            DaemonPhase::ExecutingTool => "执行工具",
            DaemonPhase::AwaitingApproval => "等你批准",
            DaemonPhase::WaitingForUser => "等你输入",
            DaemonPhase::Succeeded => "已完成",
            DaemonPhase::Failed => "失败",
            DaemonPhase::Idle => "空闲",
            DaemonPhase::Dead => "已结束",
        }
    }

    /// 本进程是否仍持有可 attach 的运行时。幽灵会话为 false。
    pub fn has_runtime(&self) -> bool {
        self.runtime
    }

    /// 相位是否已由回合级 hook / ACP 协议给出。SessionStart 不算。
    ///
    /// 旧守护不带 `turn_events`，无法区分其 phase 是结构化事件还是标题/OSC 猜测；
    /// 为避免把猜测泄漏到任一客户端，缺少该字段时必须 fail closed。
    pub fn phase_is_authoritative(&self) -> bool {
        self.turn_events
    }

    /// 可供状态与通知消费者展示的相位。没有可验证的回合来源时，活动/结果相位
    /// 一律降为空闲；启动和运行时结束仍是独立的明确生命周期事实。
    pub fn effective_phase(&self) -> DaemonPhase {
        if self.phase_is_authoritative()
            || matches!(
                self.phase,
                DaemonPhase::Connecting | DaemonPhase::Idle | DaemonPhase::Dead
            )
        {
            self.phase
        } else {
            DaemonPhase::Idle
        }
    }

    /// 用 subscribe 镜像判断会话的运行时还在不在、断了该怎么接。
    /// `None`：当前订阅还没给出可用快照，调用方不能当成已死。
    ///
    /// `subscription_live` 必须是**这一条** subscribe 连接仍活着。守护 exec
    /// 换代后旧连接已死，上一 epoch 留下的 primed 快照不能再当权威答案，
    /// 否则会把「正在交接」误判成「shell 已死」并丢掉未发送输入。
    pub fn runtime_presence_from_mirror(
        subscription_live: bool,
        primed: bool,
        state: Option<&Self>,
    ) -> Option<RuntimePresence> {
        if !subscription_live || !primed {
            return None;
        }
        Some(match state {
            Some(state) if state.has_runtime() => RuntimePresence::Live,
            Some(_) => RuntimePresence::LeftOver,
            None => RuntimePresence::Gone,
        })
    }

    /// 结构面板副文案：工具名或审批问句。
    pub fn detail_line(&self) -> Option<String> {
        if !self.phase_is_authoritative() {
            return None;
        }
        let q = self.pending_question.as_deref()?.trim();
        if q.is_empty() {
            return None;
        }
        Some(match self.effective_phase() {
            DaemonPhase::ExecutingTool => format!("🔧 {q}"),
            DaemonPhase::AwaitingApproval => format!("⚠ {q}"),
            DaemonPhase::WaitingForUser => format!("💬 {q}"),
            _ => q.to_string(),
        })
    }
}

/// 会话管理刚杀掉的 id，在 subscribe 确认消失前从快照里滤掉。
/// 返回过滤后的列表，并丢掉镜像里已经没有的 tombstone。
pub fn apply_identity_tombstones(
    snapshot: Vec<DaemonSessionState>,
    tombstones: &mut HashSet<String>,
) -> Vec<DaemonSessionState> {
    let live: HashSet<&str> = snapshot.iter().map(|state| state.id.as_str()).collect();
    tombstones.retain(|id| live.contains(id.as_str()));
    snapshot
        .into_iter()
        .filter(|state| !tombstones.contains(&state.id))
        .collect()
}

/// 跟 smeltd 的 `Phase`（见 `smeltd::session_state`）对应，同样 `rename_all = "snake_case"`。
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonPhase {
    /// ACP 握手 / 启动进程。侧栏仍算空闲，对话页才展示「启动中」。
    Connecting,
    Thinking,
    ExecutingTool,
    AwaitingApproval,
    WaitingForUser,
    Succeeded,
    Failed,
    #[default]
    Idle,
    Dead,
}

/// `event_subscribe`/`subscribe` 连接映射到客户端状态模型后的事件。
pub enum DaemonStateEvent {
    /// 首帧：全量快照。
    Snapshot {
        sessions: Vec<DaemonSessionState>,
        /// daemon 持有的远程目录复用同一条订阅连接下发，但与 agent runtime phase
        /// 分开，避免消费者把“所有权”误当成“运行状态”。
        remote_sessions: Option<RemoteSessionSnapshot>,
        /// 桌面发布的侧栏菜单。远程客户端只消费这一份，不读工作区快照。
        workspace_menu: Option<WorkspaceMenuSnapshot>,
        /// 运行时的工作区目录。没订、或运行时还没有这份投影时为 `None`。
        workspace: Option<WorkspaceDirectory>,
        /// daemon 唯一 owner 的自动化定义、状态与 Run 投影。
        automations: Option<AutomationFile>,
    },
    /// 之后每次：单个会话的变化。
    Update(DaemonSessionState),
    /// 增量删除。不能做成完整 `sessions` 帧——客户端会把带 `sessions` 的行当成
    /// 新 epoch 并整表替换，订阅首帧之后的旧删除快照会把后来新建的会话抹掉。
    Removed { id: String },
    /// 远程目录的完整替换。`revision` 仅在当前 daemon epoch 内有效；新的 subscribe
    /// 快照会重置消费者水位。
    RemoteSessions(RemoteSessionSnapshot),
    /// 桌面菜单的完整替换。水位规则与远程目录相同。
    WorkspaceMenu(WorkspaceMenuSnapshot),
    /// 工作区目录的完整替换。水位规则与远程目录相同。
    Workspace(WorkspaceDirectory),
    /// 自动化投影的完整替换。store_id 进一步保护跨 daemon epoch 的迟到响应。
    Automations(AutomationFile),
    /// 有人要看一段 daemon 里没在跑的对话，请持有启动规格的桌面端把它拉起来。
    /// 一次性请求，不是投影：没有快照、不参与水位。
    ConversationWakeRequested { session_id: String },
    /// 关注过的项目目录里有文件变了。
    ProjectChanged { root: String },
    /// 关注过的智能体的记忆目录里有文件变了（`None` 是用户全局记忆）。
    AgentMemoryChanged { agent_id: Option<String> },
    /// 会话里的终端程序发了一条 OSC 9/99/777 通知。一次性事件，不是投影；会话已有
    /// 结构化 hook 事件时运行时不会发。
    TerminalNotification { session_id: String, message: String },
    /// 会话里的终端程序用 OSC 9;4 报的进度变了（`None` = 清掉）。不是投影：运行时在
    /// 进度条还在时定期重发，后连上的客户端靠它补齐。
    TerminalProgress {
        session_id: String,
        progress: Option<TerminalProgress>,
    },
    /// CLI / Control API 要改外观，已由运行时校验过。一次性请求，不是投影。
    AppearanceChangeRequested {
        settings: std::collections::BTreeMap<String, serde_json::Value>,
    },
    /// 当前这条 subscribe 连接已断开（守护 exec / 崩溃 / socket 没了）。
    /// 不是协议帧，由客户端在 `subscribe_daemon_states_blocking` 返回后注入。
    Disconnected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_tombstones_hide_until_snapshot_drops_them() {
        let live = |id: &str| DaemonSessionState {
            id: id.into(),
            runtime: true,
            ..Default::default()
        };
        let mut tombstones = HashSet::from(["gone".into()]);
        let filtered = apply_identity_tombstones(vec![live("keep"), live("gone")], &mut tombstones);
        assert_eq!(
            filtered.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["keep"]
        );
        assert_eq!(tombstones, HashSet::from(["gone".into()]));

        let filtered = apply_identity_tombstones(vec![live("keep")], &mut tombstones);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "keep");
        assert!(tombstones.is_empty());
    }

    #[test]
    fn runtime_presence_waits_until_mirror_is_primed() {
        let live = DaemonSessionState {
            id: "s1".into(),
            runtime: true,
            ..Default::default()
        };
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(true, false, Some(&live)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(true, false, None),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(true, true, Some(&live)),
            Some(RuntimePresence::Live)
        );
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(true, true, None),
            Some(RuntimePresence::Gone)
        );
    }

    /// 守护崩溃后新守护从会话目录恢复的条目是 `runtime=false`：进程没了但身份还在，
    /// 要和「shell 已退出、条目已删」分开，客户端才知道该重新打开而不是判死。
    #[test]
    fn runtime_presence_tells_left_over_from_gone() {
        let left_over = DaemonSessionState {
            id: "s1".into(),
            runtime: false,
            ..Default::default()
        };
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(true, true, Some(&left_over)),
            Some(RuntimePresence::LeftOver)
        );
    }

    #[test]
    fn runtime_presence_ignores_stale_snapshot_while_subscription_is_down() {
        let live = DaemonSessionState {
            id: "s1".into(),
            runtime: true,
            ..Default::default()
        };
        let ghost = DaemonSessionState {
            runtime: false,
            ..live.clone()
        };
        // 守护 exec 后旧 subscribe 已死：即使上一帧还写着 runtime=false / 缺 id，
        // 也不能判死，否则 make install 会把还活着的普通 shell 输入丢掉。
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(false, true, Some(&live)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(false, true, Some(&ghost)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_presence_from_mirror(false, true, None),
            None
        );
    }

    #[test]
    fn missing_turn_events_defaults_to_not_authoritative() {
        let state: DaemonSessionState = serde_json::from_value(serde_json::json!({
            "id": "copilot",
            "structured_events": true
        }))
        .unwrap();
        assert!(state.structured_events);
        assert!(!state.turn_events);
        assert!(!state.phase_is_authoritative());

        let thinking: DaemonSessionState = serde_json::from_value(serde_json::json!({
            "id": "copilot",
            "phase": "thinking",
            "structured_events": true
        }))
        .unwrap();
        assert!(
            !thinking.phase_is_authoritative(),
            "旧守护的活动 phase 无法证明来源，必须 fail closed"
        );
        assert_eq!(thinking.effective_phase(), DaemonPhase::Idle);
        assert_eq!(thinking.detail_line(), None);

        let verified = DaemonSessionState {
            turn_events: true,
            ..thinking
        };
        assert!(verified.phase_is_authoritative());
        assert_eq!(verified.effective_phase(), DaemonPhase::Thinking);
    }
    #[test]
    fn reconnect_backoff_grows_exponentially_and_is_capped() {
        assert_eq!(
            daemon_reconnect_backoff(0),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            daemon_reconnect_backoff(1),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            daemon_reconnect_backoff(4),
            std::time::Duration::from_secs(4)
        );
        assert_eq!(
            daemon_reconnect_backoff(5),
            std::time::Duration::from_secs(8)
        );
        assert_eq!(
            daemon_reconnect_backoff(u32::MAX),
            std::time::Duration::from_secs(8)
        );
    }
}
