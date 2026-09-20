//! 状态通道（见 docs/archive/state-channel-plan.md）：GUI 侧订阅 smeltd 广播的会话状态
//! 镜像。这个模块本身不碰 GPUI，纯数据结构 + 阻塞的 socket 通信；GUI 那边的
//! `Entity`/`Global` 包装留在 main crate，跟 acp-view 未来渲染层要用同一份数据
//! 模型（会话相位翻译成这个结构），别再复制一遍判断逻辑。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::automation::AutomationFile;
use crate::daemon_protocol::DaemonOperation;
use crate::session_control::RemoteSessionSnapshot;
use crate::workspace_menu::WorkspaceMenuSnapshot;
use smelt_plugin_api::{
    CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SESSION_STATE_SCHEMA_VERSION,
    CORE_SNAPSHOT_AUTOMATIONS, CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_SESSIONS,
    CORE_SNAPSHOT_WORKSPACE_MENU, CORE_TOPIC_AUTOMATIONS_CHANGED,
    CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_WORKSPACE_MENU_CHANGED, CoreProjectionEvent,
    CoreSessionPhase, DeliveryClass, EVENT_BUS_PROTOCOL_VERSION, EventClientAuth, EventSource,
    EventSubscribeRequest, FirstPartyClientKind, ProtocolRange, SessionStateChanged,
    SessionStateRemoved, SessionStatesSnapshot, SubscribeErrorCode, SubscribeMessage,
    SubscribeRequest, SubscriptionDeclaration, SubscriptionId, Topic,
};

/// 断线后快速首试、连续失败逐步退让，避免 daemon 正在升级/尚未启动时 GUI 和网关
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

/// smeltd 的 unix socket 路径（`~/.smelt/smeltd.sock`），用时顺手建好父目录。
pub fn smeltd_sock_path() -> PathBuf {
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    let _ = std::fs::create_dir_all(&dir);
    dir.join("smeltd.sock")
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
    /// 守护下发的启动命令；远程网关在菜单快照缺少 agent 信息时用它兜底识别 provider。
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

    /// 用 subscribe 镜像判断会话是否还能 attach。
    /// `None`：当前订阅还没给出可用快照，调用方不能当成已死。
    /// `Some(false)`：当前 epoch 的快照里该 id 无运行时（已退出 / 幽灵条目）。
    ///
    /// `subscription_live` 必须是**这一条** subscribe 连接仍活着。守护 exec
    /// 换代后旧连接已死，上一 epoch 留下的 primed 快照不能再当权威答案，
    /// 否则会把「正在交接」误判成「shell 已死」并丢掉未发送输入。
    pub fn runtime_alive_from_mirror(
        subscription_live: bool,
        primed: bool,
        state: Option<&Self>,
    ) -> Option<bool> {
        if !subscription_live || !primed {
            return None;
        }
        Some(state.is_some_and(Self::has_runtime))
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

    /// 进入当前 phase 多久（秒）；phase_since 为 0 则 None。
    pub fn phase_age_secs(&self) -> Option<u64> {
        if self.phase_since == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(now.saturating_sub(self.phase_since))
    }
}

/// 终端 attachment 断线后的下一步。权威答案是 reattach 本身；镜像只用来
/// 确认「当前 epoch 已经明确没有 runtime」，避免在 shell 真退出后永远重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalReconnectAction {
    /// 守护还没起来。
    Wait,
    /// 立刻 reattach。镜像未知时也走这条——attach 成功则会话还活着。
    Reattach,
    /// 当前订阅快照明确说这个 id 没有 runtime，停止补发。
    GiveUp,
}

pub fn terminal_reconnect_action(
    daemon_up: bool,
    runtime_alive: Option<bool>,
) -> TerminalReconnectAction {
    if !daemon_up {
        return TerminalReconnectAction::Wait;
    }
    match runtime_alive {
        Some(false) => TerminalReconnectAction::GiveUp,
        Some(true) | None => TerminalReconnectAction::Reattach,
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
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Deserialize)]
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

/// `event_subscribe`/`subscribe` 连接映射到桌面和网关现有状态模型后的事件。
pub enum DaemonStateEvent {
    /// 首帧：全量快照。
    Snapshot {
        sessions: Vec<DaemonSessionState>,
        /// daemon 持有的远程目录复用同一条订阅连接下发，但与 agent runtime phase
        /// 分开，避免消费者把“所有权”误当成“运行状态”。
        remote_sessions: Option<RemoteSessionSnapshot>,
        /// 桌面发布的侧栏菜单。网关只消费这一份，不读工作区快照。
        workspace_menu: Option<WorkspaceMenuSnapshot>,
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
    /// 自动化投影的完整替换。store_id 进一步保护跨 daemon epoch 的迟到响应。
    Automations(AutomationFile),
    /// 当前这条 subscribe 连接已断开（守护 exec / 崩溃 / socket 没了）。
    /// 不是协议帧，由客户端在 `subscribe_daemon_states_blocking` 返回后注入。
    Disconnected,
}

#[derive(Clone, Copy)]
struct SessionWatermark {
    generation: u64,
    revision: u64,
    removed: bool,
}

/// 单条 event subscription 的 session generation/revision 水位。删除会留下 tombstone，
/// 因而同一 runtime generation 的迟到更新不能复活已删除会话。
#[derive(Default)]
struct DaemonStateRevisions {
    revisions: HashMap<String, SessionWatermark>,
}

impl DaemonStateRevisions {
    fn reset_from_snapshot(&mut self, sessions: &[DaemonSessionState]) {
        self.revisions = sessions
            .iter()
            .map(|state| {
                (
                    state.id.clone(),
                    SessionWatermark {
                        generation: state.generation,
                        revision: state.revision,
                        removed: false,
                    },
                )
            })
            .collect();
    }

    fn accept_update(&mut self, state: &DaemonSessionState) -> bool {
        let Some(current) = self.revisions.get(&state.id).copied() else {
            self.revisions.insert(
                state.id.clone(),
                SessionWatermark {
                    generation: state.generation,
                    revision: state.revision,
                    removed: false,
                },
            );
            return true;
        };
        let accepted = if state.generation > current.generation {
            true
        } else if state.generation < current.generation || current.removed {
            false
        } else if state.revision == 0 {
            current.revision == 0
        } else {
            state.revision > current.revision
        };
        if accepted {
            self.revisions.insert(
                state.id.clone(),
                SessionWatermark {
                    generation: state.generation,
                    revision: state.revision,
                    removed: false,
                },
            );
        }
        accepted
    }

    fn accept_removal(&mut self, removed: &SessionStateRemoved) -> bool {
        let Some(current) = self.revisions.get(&removed.session_id).copied() else {
            self.revisions.insert(
                removed.session_id.clone(),
                SessionWatermark {
                    generation: removed.generation,
                    revision: removed.last_revision,
                    removed: true,
                },
            );
            return true;
        };
        let accepted = if removed.generation > current.generation {
            true
        } else if removed.generation < current.generation || current.removed {
            false
        } else {
            removed.last_revision >= current.revision
        };
        if accepted {
            self.revisions.insert(
                removed.session_id.clone(),
                SessionWatermark {
                    generation: removed.generation,
                    revision: removed.last_revision.max(current.revision),
                    removed: true,
                },
            );
        }
        accepted
    }
}

/// 远程目录、桌面菜单、任务和自动化都是整表替换，因此各自只需一个 revision 水位。
#[derive(Default)]
struct ReplacementRevisions {
    revision: Option<u64>,
}

type RemoteSessionRevisions = ReplacementRevisions;
type WorkspaceMenuRevisions = ReplacementRevisions;
type AutomationFileRevisions = ReplacementRevisions;

impl ReplacementRevisions {
    fn reset_from_snapshot(&mut self, revision: Option<u64>) {
        self.revision = revision.filter(|revision| *revision != 0);
    }

    fn accept_update(&mut self, revision: u64) -> bool {
        if revision == 0 {
            return self.revision.is_none();
        }
        if self.revision.is_some_and(|last| revision <= last) {
            return false;
        }
        self.revision = Some(revision);
        true
    }
}

#[derive(Clone, Copy)]
struct SubscriptionShape {
    remote_sessions: bool,
    workspace_menu: bool,
    automations: bool,
}

#[derive(Default)]
struct PendingSnapshots {
    active: bool,
    sessions: Option<Vec<DaemonSessionState>>,
    remote_sessions_seen: bool,
    remote_sessions: Option<RemoteSessionSnapshot>,
    workspace_menu_seen: bool,
    workspace_menu: Option<WorkspaceMenuSnapshot>,
    automations_seen: bool,
    automations: Option<AutomationFile>,
}

impl PendingSnapshots {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    fn complete(&self, shape: SubscriptionShape) -> bool {
        self.sessions.is_some()
            && (!shape.remote_sessions || self.remote_sessions_seen)
            && (!shape.workspace_menu || self.workspace_menu_seen)
            && (!shape.automations || self.automations_seen)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonEventClientError {
    Protocol(String),
    Server {
        code: SubscribeErrorCode,
        message: String,
    },
}

impl fmt::Display for DaemonEventClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(message) => write!(f, "event subscription protocol error: {message}"),
            Self::Server { code, message } => {
                write!(f, "event subscription rejected ({code:?}): {message}")
            }
        }
    }
}

impl std::error::Error for DaemonEventClientError {}

pub struct DaemonStateEventClient {
    handshake: EventSubscribeRequest,
    shape: SubscriptionShape,
    pending: PendingSnapshots,
    revisions: DaemonStateRevisions,
    remote_revisions: RemoteSessionRevisions,
    workspace_menu_revisions: WorkspaceMenuRevisions,
    automation_revisions: AutomationFileRevisions,
}

impl DaemonStateEventClient {
    pub fn desktop() -> Self {
        Self::first_party(
            FirstPartyClientKind::Desktop,
            "desktop",
            &[
                CORE_TOPIC_SESSION_STATE_CHANGED,
                CORE_TOPIC_SESSION_REMOVED,
                CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
                CORE_TOPIC_WORKSPACE_MENU_CHANGED,
                CORE_TOPIC_AUTOMATIONS_CHANGED,
            ],
            SubscriptionShape {
                remote_sessions: true,
                workspace_menu: true,
                automations: true,
            },
        )
    }

    pub fn remote_gateway() -> Self {
        Self::first_party(
            FirstPartyClientKind::RemoteGateway,
            "remote-gateway",
            &[
                CORE_TOPIC_SESSION_STATE_CHANGED,
                CORE_TOPIC_SESSION_REMOVED,
                CORE_TOPIC_REMOTE_SESSIONS_CHANGED,
                CORE_TOPIC_WORKSPACE_MENU_CHANGED,
                // 自动化 Run 停下来等审批时，手机是唯一在场的客户端，所以网关订阅
                // 自动化投影。
                CORE_TOPIC_AUTOMATIONS_CHANGED,
            ],
            SubscriptionShape {
                remote_sessions: true,
                workspace_menu: true,
                automations: true,
            },
        )
    }

    fn first_party(
        kind: FirstPartyClientKind,
        subscription_prefix: &str,
        topics: &[&str],
        shape: SubscriptionShape,
    ) -> Self {
        let subscription_id = SubscriptionId::new(format!(
            "{subscription_prefix}-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .expect("generated subscription id is valid");
        let handshake = EventSubscribeRequest {
            auth: EventClientAuth::FirstParty { kind },
            request: SubscribeRequest {
                protocol: ProtocolRange {
                    min: EVENT_BUS_PROTOCOL_VERSION,
                    max: EVENT_BUS_PROTOCOL_VERSION,
                },
                subscription: SubscriptionDeclaration {
                    id: subscription_id,
                    topics: topics
                        .iter()
                        .map(|topic| Topic::new(*topic).expect("core topic is valid"))
                        .collect(),
                    delivery: DeliveryClass::Ephemeral,
                },
                cursor: None,
            },
        };
        let mut pending = PendingSnapshots::default();
        pending.begin();
        Self {
            handshake,
            shape,
            pending,
            revisions: DaemonStateRevisions::default(),
            remote_revisions: RemoteSessionRevisions::default(),
            workspace_menu_revisions: WorkspaceMenuRevisions::default(),
            automation_revisions: AutomationFileRevisions::default(),
        }
    }

    pub fn handshake(&self) -> &EventSubscribeRequest {
        &self.handshake
    }

    pub fn request_line(&self) -> Result<String, DaemonEventClientError> {
        let value = serde_json::json!({
            "op": DaemonOperation::EventSubscribe,
            "auth": &self.handshake.auth,
            "request": &self.handshake.request,
        });
        serde_json::to_string(&value)
            .map(|mut line| {
                line.push('\n');
                line
            })
            .map_err(|error| DaemonEventClientError::Protocol(error.to_string()))
    }

    pub fn decode_line(
        &mut self,
        line: &str,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        let message = serde_json::from_str::<SubscribeMessage<serde_json::Value>>(line)
            .map_err(|error| DaemonEventClientError::Protocol(error.to_string()))?;
        self.decode_message(message)
    }

    pub fn decode_message(
        &mut self,
        message: SubscribeMessage<serde_json::Value>,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match message {
            SubscribeMessage::Snapshot {
                kind,
                revision,
                payload,
                ..
            } => self.decode_snapshot(kind.0.as_str(), revision, payload),
            SubscribeMessage::Event { envelope, .. } => {
                if self.pending.active && !replacement_projection_topic(envelope.topic.as_str()) {
                    return Ok(None);
                }
                let schema_supported =
                    if envelope.topic.as_str() == CORE_TOPIC_SESSION_STATE_CHANGED {
                        matches!(
                            envelope.schema_version,
                            1..=CORE_SESSION_STATE_SCHEMA_VERSION
                        )
                    } else {
                        envelope.schema_version == 1
                    };
                if !schema_supported {
                    return Err(DaemonEventClientError::Protocol(format!(
                        "unsupported schema version {} for {}",
                        envelope.schema_version, envelope.topic
                    )));
                }
                if envelope.source != EventSource::Core {
                    return Err(DaemonEventClientError::Protocol(
                        "daemon projection event did not come from core".to_string(),
                    ));
                }
                self.decode_event(envelope.topic.as_str(), envelope.payload)
            }
            SubscribeMessage::Lag { .. } => {
                self.pending.begin();
                Ok(None)
            }
            SubscribeMessage::Error { code, message } => {
                Err(DaemonEventClientError::Server { code, message })
            }
            SubscribeMessage::Cursor { .. } => Err(DaemonEventClientError::Protocol(
                "ephemeral subscription received a durable cursor".to_string(),
            )),
        }
    }

    fn decode_snapshot(
        &mut self,
        kind: &str,
        revision: u64,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        if !self.pending.active {
            if let Some(event) =
                self.decode_standalone_projection_snapshot(kind, revision, payload.clone())?
            {
                return Ok(Some(event));
            }
            self.pending.begin();
        }
        match kind {
            CORE_SNAPSHOT_SESSIONS => {
                let snapshot: SessionStatesSnapshot = decode_payload(payload, kind)?;
                if !matches!(
                    snapshot.projection_version,
                    1..=CORE_SESSION_PROJECTION_VERSION
                ) || snapshot.revision != revision
                {
                    return Err(DaemonEventClientError::Protocol(format!(
                        "invalid {kind} projection version or revision"
                    )));
                }
                self.pending.sessions = Some(
                    snapshot
                        .sessions
                        .into_iter()
                        .map(daemon_session_state)
                        .collect(),
                );
            }
            CORE_SNAPSHOT_REMOTE_SESSIONS if self.shape.remote_sessions => {
                self.pending.remote_sessions = decode_projection_snapshot(payload, kind, revision)?;
                self.pending.remote_sessions_seen = true;
            }
            CORE_SNAPSHOT_WORKSPACE_MENU if self.shape.workspace_menu => {
                self.pending.workspace_menu = decode_projection_snapshot(payload, kind, revision)?;
                self.pending.workspace_menu_seen = true;
            }
            CORE_SNAPSHOT_AUTOMATIONS if self.shape.automations => {
                match decode_projection_snapshot(payload, kind, revision) {
                    Ok(file) => self.pending.automations = file,
                    Err(error) => {
                        eprintln!("[daemon-state] {error}");
                        self.pending.automations = None;
                    }
                }
                self.pending.automations_seen = true;
            }
            _ => {
                eprintln!("[daemon-state] ignoring unexpected snapshot kind {kind}");
            }
        }
        if !self.pending.complete(self.shape) {
            return Ok(None);
        }
        let pending = std::mem::take(&mut self.pending);
        let sessions = pending
            .sessions
            .expect("complete snapshot batch contains sessions");
        self.revisions.reset_from_snapshot(&sessions);
        self.remote_revisions
            .reset_from_snapshot(pending.remote_sessions.as_ref().map(|value| value.revision));
        self.workspace_menu_revisions
            .reset_from_snapshot(pending.workspace_menu.as_ref().map(|value| value.revision));
        self.automation_revisions
            .reset_from_snapshot(pending.automations.as_ref().map(|value| value.revision));
        Ok(Some(DaemonStateEvent::Snapshot {
            sessions,
            remote_sessions: pending.remote_sessions,
            workspace_menu: pending.workspace_menu,
            automations: pending.automations,
        }))
    }

    fn decode_event(
        &mut self,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match topic {
            CORE_TOPIC_SESSION_STATE_CHANGED => {
                let state = daemon_session_state(decode_payload(payload, topic)?);
                Ok(self
                    .revisions
                    .accept_update(&state)
                    .then_some(DaemonStateEvent::Update(state)))
            }
            CORE_TOPIC_SESSION_REMOVED => {
                let removed: SessionStateRemoved = decode_payload(payload, topic)?;
                Ok(self
                    .revisions
                    .accept_removal(&removed)
                    .then_some(DaemonStateEvent::Removed {
                        id: removed.session_id,
                    }))
            }
            CORE_TOPIC_REMOTE_SESSIONS_CHANGED if self.shape.remote_sessions => {
                let snapshot: RemoteSessionSnapshot = decode_projection_event(payload, topic)?;
                Ok(self
                    .remote_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::RemoteSessions(snapshot)))
            }
            CORE_TOPIC_WORKSPACE_MENU_CHANGED if self.shape.workspace_menu => {
                let snapshot: WorkspaceMenuSnapshot = decode_projection_event(payload, topic)?;
                Ok(self
                    .workspace_menu_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::WorkspaceMenu(snapshot)))
            }
            CORE_TOPIC_AUTOMATIONS_CHANGED if self.shape.automations => {
                let snapshot: AutomationFile = decode_projection_event(payload, topic)?;
                Ok(self
                    .automation_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::Automations(snapshot)))
            }
            _ => Err(DaemonEventClientError::Protocol(format!(
                "unexpected event topic {topic}"
            ))),
        }
    }

    fn decode_standalone_projection_snapshot(
        &mut self,
        kind: &str,
        revision: u64,
        payload: serde_json::Value,
    ) -> Result<Option<DaemonStateEvent>, DaemonEventClientError> {
        match kind {
            CORE_SNAPSHOT_AUTOMATIONS if self.shape.automations => {
                let snapshot: Option<AutomationFile> =
                    match decode_projection_snapshot(payload, kind, revision) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            eprintln!("[daemon-state] {error}");
                            return Ok(None);
                        }
                    };
                let Some(snapshot) = snapshot else {
                    return Ok(None);
                };
                Ok(self
                    .automation_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::Automations(snapshot)))
            }
            CORE_SNAPSHOT_REMOTE_SESSIONS if self.shape.remote_sessions => {
                let Some(snapshot): Option<RemoteSessionSnapshot> =
                    decode_projection_snapshot(payload, kind, revision)?
                else {
                    return Ok(None);
                };
                Ok(self
                    .remote_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::RemoteSessions(snapshot)))
            }
            CORE_SNAPSHOT_WORKSPACE_MENU if self.shape.workspace_menu => {
                let Some(snapshot): Option<WorkspaceMenuSnapshot> =
                    decode_projection_snapshot(payload, kind, revision)?
                else {
                    return Ok(None);
                };
                Ok(self
                    .workspace_menu_revisions
                    .accept_update(snapshot.revision)
                    .then_some(DaemonStateEvent::WorkspaceMenu(snapshot)))
            }
            _ => Ok(None),
        }
    }
}

fn replacement_projection_topic(topic: &str) -> bool {
    matches!(
        topic,
        CORE_TOPIC_AUTOMATIONS_CHANGED
            | CORE_TOPIC_REMOTE_SESSIONS_CHANGED
            | CORE_TOPIC_WORKSPACE_MENU_CHANGED
    )
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
) -> Result<T, DaemonEventClientError> {
    serde_json::from_value(payload).map_err(|error| {
        DaemonEventClientError::Protocol(format!("invalid {label} payload: {error}"))
    })
}

fn decode_projection(
    payload: serde_json::Value,
    label: &str,
) -> Result<CoreProjectionEvent, DaemonEventClientError> {
    let projection: CoreProjectionEvent = decode_payload(payload, label)?;
    if projection.projection_version != CORE_PROJECTION_VERSION {
        return Err(DaemonEventClientError::Protocol(format!(
            "unsupported {label} projection version {}",
            projection.projection_version
        )));
    }
    Ok(projection)
}

fn decode_projection_snapshot<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
    revision: u64,
) -> Result<Option<T>, DaemonEventClientError> {
    let projection = decode_projection(payload, label)?;
    if projection.revision != revision {
        return Err(DaemonEventClientError::Protocol(format!(
            "{label} snapshot revision mismatch"
        )));
    }
    if projection.data.is_null() {
        Ok(None)
    } else {
        decode_payload(projection.data, label).map(Some)
    }
}

fn decode_projection_event<T: serde::de::DeserializeOwned>(
    payload: serde_json::Value,
    label: &str,
) -> Result<T, DaemonEventClientError> {
    let projection = decode_projection(payload, label)?;
    if projection.data.is_null() {
        return Err(DaemonEventClientError::Protocol(format!(
            "{label} event contains a null projection"
        )));
    }
    decode_payload(projection.data, label)
}

fn daemon_session_state(state: SessionStateChanged) -> DaemonSessionState {
    DaemonSessionState {
        id: state.session_id,
        generation: state.generation,
        revision: state.revision,
        phase: match state.phase {
            CoreSessionPhase::Connecting => DaemonPhase::Connecting,
            CoreSessionPhase::Thinking => DaemonPhase::Thinking,
            CoreSessionPhase::ExecutingTool => DaemonPhase::ExecutingTool,
            CoreSessionPhase::AwaitingApproval => DaemonPhase::AwaitingApproval,
            CoreSessionPhase::WaitingForUser => DaemonPhase::WaitingForUser,
            CoreSessionPhase::Succeeded => DaemonPhase::Succeeded,
            CoreSessionPhase::Failed => DaemonPhase::Failed,
            CoreSessionPhase::Idle => DaemonPhase::Idle,
            CoreSessionPhase::Dead => DaemonPhase::Dead,
        },
        pending_question: state.pending_question,
        title: state.title,
        launch: state.launch,
        provider: state.provider,
        conversation_id: state.conversation_id,
        cwd: state.cwd,
        phase_since: state.phase_since_unix_seconds,
        updated_at: state.updated_at_unix_seconds,
        structured_events: state.structured_events,
        turn_events: state.turn_events,
        runtime: state.runtime,
        ..Default::default()
    }
}

/// 阻塞：连接正式 `event_subscribe`，逐行解析稳定 DTO，直到连接断开或协议错误。
/// Ephemeral 订阅不发送 ack；Lag 后只等待 daemon 的恢复 snapshots。
pub fn subscribe_daemon_states_blocking(tx: &smol::channel::Sender<DaemonStateEvent>) {
    let Ok(mut s) = UnixStream::connect(smeltd_sock_path()) else {
        return;
    };
    let mut client = DaemonStateEventClient::desktop();
    let Ok(request) = client.request_line() else {
        return;
    };
    if s.write_all(request.as_bytes()).is_err() {
        return;
    }
    let reader = BufReader::new(s);
    for line in reader.lines().map_while(Result::ok) {
        let event = match client.decode_line(&line) {
            Ok(Some(event)) => event,
            Ok(None) => continue,
            Err(error) => {
                eprintln!("[daemon-state] event subscribe decode failed: {error}");
                return;
            }
        };
        if tx.try_send(event).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn state(id: &str, revision: u64) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            generation: 1,
            revision,
            ..Default::default()
        }
    }

    fn session_dto(id: &str, generation: u64, revision: u64) -> SessionStateChanged {
        SessionStateChanged {
            session_id: id.to_string(),
            generation,
            revision,
            cwd: Some("/workspace".to_string()),
            launch: Some("agent".to_string()),
            provider: Some("codex".to_string()),
            conversation_id: Some(format!("conv-{id}")),
            agent_mcp: false,
            title: Some(id.to_string()),
            phase: CoreSessionPhase::Thinking,
            phase_since_unix_seconds: 10,
            pending_question: None,
            tokens_used: None,
            branch: None,
            dirty_files: Vec::new(),
            updated_at_unix_seconds: 11,
            structured_events: true,
            turn_events: true,
            agent_event_version: Some(1),
            runtime: true,
        }
    }

    fn sessions_snapshot(
        revision: u64,
        sessions: Vec<SessionStateChanged>,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Snapshot {
            kind: smelt_plugin_api::SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()),
            watermark: 1,
            revision,
            payload: serde_json::to_value(SessionStatesSnapshot {
                projection_version: CORE_SESSION_PROJECTION_VERSION,
                revision,
                sessions,
            })
            .unwrap(),
        }
    }

    fn projection_snapshot(
        kind: &str,
        revision: u64,
        data: serde_json::Value,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Snapshot {
            kind: smelt_plugin_api::SnapshotKind(kind.to_string()),
            watermark: 1,
            revision,
            payload: serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision,
                data,
            })
            .unwrap(),
        }
    }

    fn event_message(
        topic: &str,
        payload: serde_json::Value,
    ) -> SubscribeMessage<serde_json::Value> {
        SubscribeMessage::Event {
            sequence: Some(1),
            envelope: smelt_plugin_api::EventEnvelope {
                event_id: smelt_plugin_api::EventId::new("event-1").unwrap(),
                topic: Topic::new(topic).unwrap(),
                schema_version: 1,
                occurred_at_ms: 1,
                aggregate: None,
                aggregate_revision: None,
                source: EventSource::Core,
                correlation_id: smelt_plugin_api::CorrelationId::new("correlation-1").unwrap(),
                causation_id: None,
                hop_count: 0,
                payload,
            },
        }
    }

    fn complete_desktop_snapshot(
        client: &mut DaemonStateEventClient,
        revision: u64,
        sessions: Vec<SessionStateChanged>,
    ) -> DaemonStateEvent {
        assert!(
            client
                .decode_message(sessions_snapshot(revision, sessions))
                .unwrap()
                .is_none()
        );
        for kind in [
            CORE_SNAPSHOT_REMOTE_SESSIONS,
            CORE_SNAPSHOT_WORKSPACE_MENU,
            CORE_SNAPSHOT_AUTOMATIONS,
        ] {
            let event = client
                .decode_message(projection_snapshot(kind, 0, serde_json::Value::Null))
                .unwrap();
            if kind == CORE_SNAPSHOT_AUTOMATIONS {
                return event.expect("last expected snapshot completes the batch");
            }
            assert!(event.is_none());
        }
        unreachable!()
    }

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
    fn runtime_alive_waits_until_mirror_is_primed() {
        let live = DaemonSessionState {
            id: "s1".into(),
            runtime: true,
            ..Default::default()
        };
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(true, false, Some(&live)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(true, false, None),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(true, true, Some(&live)),
            Some(true)
        );
        let ghost = DaemonSessionState {
            runtime: false,
            ..live
        };
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(true, true, Some(&ghost)),
            Some(false)
        );
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(true, true, None),
            Some(false)
        );
    }

    #[test]
    fn runtime_alive_ignores_stale_snapshot_while_subscription_is_down() {
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
            DaemonSessionState::runtime_alive_from_mirror(false, true, Some(&live)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(false, true, Some(&ghost)),
            None
        );
        assert_eq!(
            DaemonSessionState::runtime_alive_from_mirror(false, true, None),
            None
        );
    }

    #[test]
    fn reconnect_tries_attach_until_current_epoch_says_dead() {
        use super::{TerminalReconnectAction, terminal_reconnect_action};

        assert_eq!(
            terminal_reconnect_action(false, None),
            TerminalReconnectAction::Wait
        );
        assert_eq!(
            terminal_reconnect_action(false, Some(false)),
            TerminalReconnectAction::Wait
        );
        // 守护已起来但新订阅还没到：立刻 reattach，不要空等镜像。
        assert_eq!(
            terminal_reconnect_action(true, None),
            TerminalReconnectAction::Reattach
        );
        assert_eq!(
            terminal_reconnect_action(true, Some(true)),
            TerminalReconnectAction::Reattach
        );
        assert_eq!(
            terminal_reconnect_action(true, Some(false)),
            TerminalReconnectAction::GiveUp
        );
    }

    #[test]
    fn revision_tracker_rejects_duplicate_and_older_updates() {
        let mut tracker = DaemonStateRevisions::default();
        tracker.reset_from_snapshot(&[state("a", 10), state("b", 3)]);

        assert!(!tracker.accept_update(&state("a", 10)));
        assert!(!tracker.accept_update(&state("a", 9)));
        assert!(tracker.accept_update(&state("a", 11)));
        assert!(tracker.accept_update(&state("b", 4)));
        assert!(tracker.accept_update(&state("legacy", 0)));
    }

    #[test]
    fn a_new_subscription_snapshot_resets_the_daemon_epoch() {
        let mut tracker = DaemonStateRevisions::default();
        tracker.reset_from_snapshot(&[state("a", 100)]);
        tracker.reset_from_snapshot(&[state("a", 2)]);

        assert!(tracker.accept_update(&state("a", 3)));
    }

    #[test]
    fn removal_tombstone_rejects_late_generation_and_preserves_other_revisions() {
        let mut revisions = DaemonStateRevisions::default();
        revisions.reset_from_snapshot(&[state("keep", 10), state("gone", 4)]);

        assert!(revisions.accept_removal(&SessionStateRemoved {
            session_id: "gone".to_string(),
            generation: 1,
            last_revision: 4,
            removed_at_ms: 1,
        }));
        assert!(
            !revisions.accept_update(&state("keep", 10)),
            "删除别人不能把 keep 的水位清掉"
        );
        let late = state("gone", 5);
        assert!(
            !revisions.accept_update(&late),
            "同一 generation 的迟到更新不能复活删除会话"
        );
        let mut recreated = state("gone", 1);
        recreated.generation = 2;
        assert!(revisions.accept_update(&recreated));
    }

    #[test]
    fn desktop_handshake_uses_formal_protocol_and_unique_ephemeral_identity() {
        let first = DaemonStateEventClient::desktop();
        let second = DaemonStateEventClient::desktop();
        let line: serde_json::Value =
            serde_json::from_str(first.request_line().unwrap().trim()).unwrap();
        assert_eq!(line["op"], "event_subscribe");
        assert_eq!(
            line["auth"],
            serde_json::json!({
                "type": "first_party",
                "kind": "desktop"
            })
        );
        assert_eq!(line["request"]["subscription"]["delivery"], "ephemeral");
        assert!(line["request"]["cursor"].is_null());
        let topics = line["request"]["subscription"]["topics"]
            .as_array()
            .unwrap();
        assert_eq!(topics.len(), 5);
        assert!(
            topics
                .iter()
                .any(|topic| topic == CORE_TOPIC_AUTOMATIONS_CHANGED)
        );
        assert_ne!(
            first.handshake().request.subscription.id,
            second.handshake().request.subscription.id
        );
        assert!(line["auth"].get("capabilities").is_none());
    }

    #[test]
    fn remote_revision_tracker_rejects_duplicate_and_older_replacements() {
        let mut tracker = RemoteSessionRevisions::default();
        tracker.reset_from_snapshot(Some(10));

        assert!(!tracker.accept_update(10));
        assert!(!tracker.accept_update(9));
        assert!(tracker.accept_update(11));

        tracker.reset_from_snapshot(None);
        assert!(tracker.accept_update(1));
        assert!(!tracker.accept_update(0));

        let mut legacy = RemoteSessionRevisions::default();
        assert!(legacy.accept_update(0));
        assert!(legacy.accept_update(0));
    }

    #[test]
    fn session_projection_carries_the_conversation_id_across_versions() {
        let mut client = DaemonStateEventClient::desktop();
        let event = complete_desktop_snapshot(&mut client, 7, vec![session_dto("session-1", 3, 4)]);
        assert!(
            matches!(
                event,
                DaemonStateEvent::Snapshot { sessions, .. }
                    if sessions[0].conversation_id.as_deref() == Some("conv-session-1")
            ),
            "快照必须把 provider 的对话 id 带到 UI"
        );

        // 旧 daemon（未带对话 id、schema 停在上一版）仍要能解码，否则升级期订阅会整条断掉。
        let mut older = serde_json::to_value(session_dto("session-1", 3, 5)).unwrap();
        older.as_object_mut().unwrap().remove("conversation_id");
        let mut message = event_message(CORE_TOPIC_SESSION_STATE_CHANGED, older);
        if let SubscribeMessage::Event { envelope, .. } = &mut message {
            envelope.schema_version = CORE_SESSION_STATE_SCHEMA_VERSION - 1;
        }
        assert!(matches!(
            client.decode_message(message).unwrap(),
            Some(DaemonStateEvent::Update(state))
                if state.revision == 5 && state.conversation_id.is_none()
        ));
    }

    #[test]
    fn formal_messages_map_snapshot_events_and_revisions() {
        let mut legacy_value = serde_json::to_value(session_dto("legacy", 1, 1)).unwrap();
        legacy_value.as_object_mut().unwrap().remove("provider");
        let legacy: SessionStateChanged = serde_json::from_value(legacy_value).unwrap();
        assert_eq!(
            legacy.provider, None,
            "v1 daemon 没有 provider 时必须可读取"
        );

        let mut client = DaemonStateEventClient::desktop();
        let event = complete_desktop_snapshot(&mut client, 7, vec![session_dto("session-1", 3, 4)]);
        assert!(matches!(
            event,
            DaemonStateEvent::Snapshot { sessions, .. }
                if sessions.len() == 1
                    && sessions[0].generation == 3
                    && sessions[0].revision == 4
                    && sessions[0].provider.as_deref() == Some("codex")
        ));

        let update = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 3, 5)).unwrap(),
        );
        assert!(matches!(
            client.decode_message(update).unwrap(),
            Some(DaemonStateEvent::Update(state))
                if state.revision == 5 && state.provider.as_deref() == Some("codex")
        ));
        let duplicate = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 3, 5)).unwrap(),
        );
        assert!(client.decode_message(duplicate).unwrap().is_none());

        let automations = AutomationFile {
            revision: 10,
            ..Default::default()
        };
        let projection = CoreProjectionEvent {
            projection_version: CORE_PROJECTION_VERSION,
            revision: 10,
            data: serde_json::to_value(&automations).unwrap(),
        };
        assert!(matches!(
            client
                .decode_message(event_message(
                    CORE_TOPIC_AUTOMATIONS_CHANGED,
                    serde_json::to_value(projection).unwrap(),
                ))
                .unwrap(),
            Some(DaemonStateEvent::Automations(snapshot)) if snapshot.revision == 10
        ));
    }

    #[test]
    fn lag_waits_for_all_recovery_snapshots_before_resuming_events() {
        let mut client = DaemonStateEventClient::desktop();
        let _ = complete_desktop_snapshot(&mut client, 7, vec![session_dto("session-1", 1, 10)]);
        assert!(
            client
                .decode_message(SubscribeMessage::Lag {
                    dropped: 2,
                    resume_after: Some(10),
                })
                .unwrap()
                .is_none()
        );
        let ignored = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 1, 11)).unwrap(),
        );
        assert!(client.decode_message(ignored).unwrap().is_none());

        let recovered =
            complete_desktop_snapshot(&mut client, 2, vec![session_dto("session-1", 1, 2)]);
        assert!(matches!(recovered, DaemonStateEvent::Snapshot { .. }));
        let next = event_message(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(session_dto("session-1", 1, 3)).unwrap(),
        );
        assert!(matches!(
            client.decode_message(next).unwrap(),
            Some(DaemonStateEvent::Update(state)) if state.revision == 3
        ));
    }

    #[test]
    fn error_and_legacy_frames_are_not_treated_as_state_events() {
        let mut client = DaemonStateEventClient::desktop();
        let Err(error) = client.decode_message(SubscribeMessage::Error {
            code: SubscribeErrorCode::Rejected,
            message: "denied".to_string(),
        }) else {
            panic!("server error must terminate the subscription");
        };
        assert!(matches!(
            error,
            DaemonEventClientError::Server {
                code: SubscribeErrorCode::Rejected,
                ..
            }
        ));
        let Err(error) = client.decode_line(r#"{"sessions":[]}"#) else {
            panic!("legacy frame must not decode");
        };
        assert!(error.to_string().contains("protocol error"));
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

    fn automation_file(revision: u64, store_id: &str) -> AutomationFile {
        AutomationFile {
            schema_version: 1,
            store_id: store_id.into(),
            revision,
            ..Default::default()
        }
    }

    fn automations_changed(file: &AutomationFile) -> SubscribeMessage<serde_json::Value> {
        event_message(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            serde_json::to_value(CoreProjectionEvent {
                projection_version: CORE_PROJECTION_VERSION,
                revision: file.revision,
                data: serde_json::to_value(file).unwrap(),
            })
            .unwrap(),
        )
    }

    #[test]
    fn desktop_snapshot_batch_delivers_automation_file() {
        let file = automation_file(1440, "store-a");
        let mut client = DaemonStateEventClient::desktop();
        assert!(
            client
                .decode_message(sessions_snapshot(1, vec![session_dto("s1", 1, 1)]))
                .unwrap()
                .is_none()
        );
        for kind in [CORE_SNAPSHOT_REMOTE_SESSIONS, CORE_SNAPSHOT_WORKSPACE_MENU] {
            assert!(
                client
                    .decode_message(projection_snapshot(kind, 0, serde_json::Value::Null))
                    .unwrap()
                    .is_none()
            );
        }
        let event = client
            .decode_message(projection_snapshot(
                CORE_SNAPSHOT_AUTOMATIONS,
                file.revision,
                serde_json::to_value(&file).unwrap(),
            ))
            .unwrap();
        match event {
            Some(DaemonStateEvent::Snapshot {
                automations: Some(got),
                ..
            }) => {
                assert_eq!(got.store_id, file.store_id);
                assert_eq!(got.revision, 1440);
            }
            _ => panic!("expected Snapshot with automations"),
        }
    }

    #[test]
    fn automations_events_are_not_dropped_during_incomplete_snapshot_batch() {
        let mut client = DaemonStateEventClient::desktop();
        assert!(
            client
                .decode_message(sessions_snapshot(1, vec![session_dto("s1", 1, 1)]))
                .unwrap()
                .is_none()
        );
        let file = automation_file(12, "store-b");
        match client.decode_message(automations_changed(&file)).unwrap() {
            Some(DaemonStateEvent::Automations(got)) => {
                assert_eq!(got.store_id, "store-b");
                assert_eq!(got.revision, 12);
            }
            _ => panic!("automations.changed must not wait for the rest of the snapshot batch"),
        }
    }

    #[test]
    fn standalone_automations_snapshot_after_batch_is_an_incremental_event() {
        let mut client = DaemonStateEventClient::desktop();
        let _ = complete_desktop_snapshot(&mut client, 1, vec![session_dto("s1", 1, 1)]);
        let file = automation_file(20, "store-c");
        match client
            .decode_message(projection_snapshot(
                CORE_SNAPSHOT_AUTOMATIONS,
                file.revision,
                serde_json::to_value(&file).unwrap(),
            ))
            .unwrap()
        {
            Some(DaemonStateEvent::Automations(got)) => assert_eq!(got.revision, 20),
            _ => panic!("late automations snapshot must not start a new batch"),
        }
    }

    #[test]
    fn reconnect_backoff_grows_exponentially_and_is_capped() {
        assert_eq!(daemon_reconnect_backoff(0), Duration::from_millis(250));
        assert_eq!(daemon_reconnect_backoff(1), Duration::from_millis(500));
        assert_eq!(daemon_reconnect_backoff(4), Duration::from_secs(4));
        assert_eq!(daemon_reconnect_backoff(5), Duration::from_secs(8));
        assert_eq!(daemon_reconnect_backoff(u32::MAX), Duration::from_secs(8));
    }
}
