//! ACP 会话托管：open/watch/action/kill/restart，以及无缝升级所需的 ACP 快照。
//!
//! 从 `main.rs` 整块搬出。连接与归约仍在 `smelt_core::acp_session`；这里只跑守护侧的会话活体。

use super::*;

// ===================== ACP 会话托管 =====================
//
// 跟终端会话是两条平行的托管逻辑：没有 PTY/网格，「画面」就是
// `smelt_core::acp_session::ConversationSnapshot`（entries + phase + 待办卡片），由
// `smelt_core::acp_session::apply_event` 把子进程 agent 发来的协议事件归约
// 出来——归约逻辑本身跟 GPUI 无关，谁接手连接谁跑，见该模块文件头注释（核心
// 原因：`ConversationEvent::Permission`/`Elicitation` 带的 responder 绑在连接线程上，
// 没法跨进程传。因此每个会话由独立 session host 跑完整事件循环；主 smeltd
// 只持有可重建镜像和一条控制 socket，exec 时不再搬 SDK 内存。
//
// 状态管理复用终端会话已有的 `SessionState`/`Phase`/`broadcast_state`/
// `subscribe` 机制：`AcpSession.state` 就是一份跟终端会话同类型的
// `Arc<Mutex<SessionState>>`，list/subscribe 汇总时两边的 Vec 拼在一起即可。
// ACP 会话 id 沿用 GUI 现有的 `acp-` 前缀约定，GUI 靠这个前缀判断该走
// open/watch 还是 acp_open/acp_watch。
//
// 协议：
//   {"op":"acp_open","id":"acp-..","cwd":"..",
//    "launch":{"command":"..","env":{"KEY":"value"}},"agent":"claude",
//    "resume_id":".."}
//     → 只认结构化 `launch`。
//     → 已存在且还活着（有 handle）就直接接上；已存在但 Ended（没有 handle）
//       就用请求带的 launch + 已知的旧 session id（没有才退回请求带的 resume_id）
//       重新 spawn（「重新开始」）；都不存在就全新建。丢失 daemon slot 时会先起
//       一个 replacement 进程，再尝试按 resume_id 恢复协议状态；明确的历史缺失
//       只有在本地投影为空时才继续创建新的 conversation，transient failure 和
//       有本地历史的恢复失败只返回错误，不会偷偷新建。回一份
//       `{"snapshot": ConversationSnapshot}`，之后每次归约有实质变化再推一份同形状的
//       行。同 id 只允许一个控制连接，第二次 open 顶掉前一个。
//   {"op":"acp_watch","id":".."} → 只读镜像，会话必须已存在，可多个并存。
//   {"op":"acp_kill","id":".."} → 回 {"ok":true}，杀子进程、从表里摘掉、
//     踢掉所有 client/watcher。
//
// acp_open 连接内不是终端那套二进制帧，是纯 JSON 行、双向：
//   客户端 → 守护：一行 `AcpUserAction` 的 JSON
//   守护 → 客户端：一行 `{"snapshot": ConversationSnapshot}`
// 断开 acp_open 连接（切标签/关标签/App 退出）只摘连接，不杀会话——这正是
// 这层要解决的问题（GUI 退出不该带走 ACP 对话）。真要杀走 acp_kill。
//
// 「无缝升级」交接 session host 控制 socket + 镜像水位。SDK future、outstanding
// callback、审批 responder 和 prompt 队列始终留在宿主进程，所以 Running/审批中也
// 能升级；新 daemon 接管后发 Refresh 全量追平 exec 窗口。旧版 direct-fd handoff
// 仍保留兼容，只对那类遗留会话要求一次协议静默边界，接管后立即迁入独立宿主。

pub(crate) struct AcpOut {
    /// ACP 快照也复用有界 attachment 邮箱；归约线程只入队，不直接写 socket。
    pub(crate) client: Option<OutputAttachment>,
    pub(crate) watchers: Vec<OutputAttachment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpRestorePolicy {
    /// 保留恢复失败，交给用户重试或决定如何处理。
    Strict,
    /// provider 明确说历史不存在时，允许空白会话改走 `session/new`。
    FreshOnMissing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcpRestoreState {
    /// 当前没有一次待恢复的历史连接。
    Fresh,
    /// 已经开始过一次恢复尝试。必须保留首次尝试时的判断，即使
    /// `HistoryReplayStarted` 后本地 entries 被清空。
    Pending {
        session_id: String,
        policy: AcpRestorePolicy,
    },
    /// provider 曾经成功恢复过历史；后续历史缺失不能再被当成空白会话。
    Restored { session_id: String },
}

impl AcpRestoreState {
    pub(crate) fn begin(
        &mut self,
        resume_session_id: Option<&str>,
        has_local_entries: bool,
    ) -> Option<AcpRestorePolicy> {
        let Some(resume_session_id) = resume_session_id else {
            *self = Self::Fresh;
            return None;
        };
        let policy = match self {
            Self::Pending { session_id, policy } if session_id == resume_session_id => *policy,
            Self::Restored { session_id } if session_id == resume_session_id => {
                AcpRestorePolicy::Strict
            }
            Self::Fresh => {
                if has_local_entries {
                    AcpRestorePolicy::Strict
                } else {
                    AcpRestorePolicy::FreshOnMissing
                }
            }
            Self::Pending { .. } | Self::Restored { .. } => {
                if has_local_entries {
                    AcpRestorePolicy::Strict
                } else {
                    AcpRestorePolicy::FreshOnMissing
                }
            }
        };
        *self = Self::Pending {
            session_id: resume_session_id.to_string(),
            policy,
        };
        Some(policy)
    }

    pub(crate) fn mark_restored(&mut self, session_id: Option<&str>) {
        let session_id = match self {
            Self::Pending { session_id, .. } | Self::Restored { session_id } => {
                Some(session_id.clone())
            }
            Self::Fresh => session_id.map(ToString::to_string),
        };
        if let Some(session_id) = session_id {
            *self = Self::Restored { session_id };
        }
    }
}

pub(crate) struct AcpSession {
    pub(crate) instance: u64,
    pub(crate) reduced: Mutex<smelt_core::acp_session::AcpSessionState>,
    pub(crate) snapshot_revision: AtomicU64,
    pub(crate) connection_generation: AtomicU64,
    /// 串行化回合收尾与 prompt 闸门释放。看门狗和迟到的 ACP 更新可能同时
    /// 判定同一回合结束；必须在同一临界区内重新检查状态并派发下一条 prompt，
    /// 否则旧回合会把新回合的 gate 再次打开。
    pub(crate) turn_completion: Mutex<()>,
    /// 服务端 prompt 闸门。GUI 侧会乐观排队，但多个客户端可能共用一个 ACP
    /// 会话，且快照可能与 provider 的最终更新竞态，最终顺序必须由 daemon 保证。
    pub(crate) prompt_in_flight: AtomicBool,
    pub(crate) pending_prompts: Mutex<VecDeque<QueuedAcpPrompt>>,
    /// 主 daemon 只持有独立 session-host 的控制连接；真正的 ACP SDK handle
    /// 留在宿主进程。session-host 自己运行本模块时则只使用下面的直接 handle。
    pub(crate) hosted_handle: Mutex<Option<acp_runtime_host::HostedConversationHandle>>,
    /// 最近已合并的宿主快照版本，单独于对 GUI 发布的 `snapshot_revision`。
    pub(crate) host_snapshot_revision: AtomicU64,
    pub(crate) handle: Mutex<Option<smelt_core::acp_conn::ConversationHandle>>,
    /// 上次 retire 未能 waitpid 的直接子进程。未清空前禁止 relaunch，也不得
    /// 把 sid 从表里摘掉——否则新 open 会在旧进程还活着时 session/load。
    pub(crate) unreaped_pid: Mutex<Option<i32>>,
    pub(crate) cwd: Option<String>,
    /// 旧 handoff 格式兼容字段；当前恢复路径不再读取 agent 私有 transcript。
    pub(crate) agent_needs_transcript_check: bool,
    /// 四色状态，跟终端会话共用同一个类型/同一套广播机制。
    pub(crate) state: Arc<Mutex<SessionState>>,
    /// Serializes snapshot creation, initial attachment, and incremental fanout.
    pub(crate) output_gate: Mutex<()>,
    pub(crate) out: Mutex<AcpOut>,
    /// 最近一次真正 open/relaunch 用过的完整启动规格（含 env），供 `acp_restart`
    /// 重启时复用——`state.launch` 只存了命令字符串给状态展示用，重启需要
    /// 完整结构化的 `ConversationLaunchSpec`（env 可能带空格，不能靠字符串拼回去）。
    pub(crate) launch_spec: Mutex<Option<smelt_core::agent_kind::ConversationLaunchSpec>>,
    /// 完整 launch + 临时环境的单向摘要。handoff 不落临时凭据本体，但必须
    /// 保留相等性判断，否则 daemon exec 后 GUI 一重连就会误判配置变化、杀掉
    /// 仍在活跃的独立宿主。
    pub(crate) runtime_spec_fingerprint: Mutex<Option<String>>,
    /// ACP 恢复 supervisor 的生命周期状态，不属于可持久化的对话投影。
    pub(crate) restore_state: Mutex<AcpRestoreState>,
    /// 外部智能体/网页配置的临时环境变量。仅在当前 daemon 生命周期内保存，以便
    /// 强制重启同一 ACP 子进程；不参与 handoff/落盘，防止凭据写入磁盘。
    pub(crate) ephemeral_env: Mutex<BTreeMap<String, String>>,
    /// 用户输入的宿主级路由。`None` 表示尚未确认（旧 handoff 缺失该字段），
    /// 不能当成 Direct；第一次 attach 才采用客户端值。Task Runtime delivery
    /// 不读取它，只由 `acp_submit_input` 使用。
    pub(crate) conversation_binding: Mutex<Option<smelt_core::conversation::ConversationBinding>>,
    /// 产品级智能体会话身份。ACP 仍只负责执行；该绑定供快照/UI/lifecycle registry
    /// 选择插件 controller，缺失表示普通 ACP 会话或旧 handoff。
    pub(crate) agent_session: Mutex<Option<smelt_plugin_api::AgentSessionBinding>>,
    /// 串行化所有交互式输入路由。插件调用可能阻塞；没有这把门时两个客户端会
    /// 同时把同一个首轮预设各带一次。
    pub(crate) conversation_submit: Mutex<()>,
    /// 只装饰下一条成功路由的交互输入。Task/Peer delivery 不读取也不消费它。
    pub(crate) pending_agent_preset: Mutex<Option<String>>,
}

#[derive(Clone)]
pub(crate) struct QueuedAcpPrompt {
    pub(crate) text: String,
    pub(crate) images: Vec<smelt_core::acp_conn::PromptImage>,
    pub(crate) delivery_id: Option<String>,
}

pub(crate) type AcpSessions = Arc<AcpRegistry<AcpSession>>;

/// prod 注册表：每个 ACP 会话跑在独立的 smeltd 宿主进程里。
pub(crate) fn new_acp_sessions() -> AcpSessions {
    Arc::new(AcpRegistry::new(Arc::clone(&SPAWN_GATE)))
}

/// 单测注册表：同进程 direct driver。libtest 二进制不是可用的 smeltd
/// entrypoint，拿它去 spawn 宿主只会起一堆跑不起来的进程，所以测试显式选这个。
#[cfg(test)]
pub(crate) fn new_test_acp_sessions() -> AcpSessions {
    Arc::new(AcpRegistry::new_same_process(Arc::clone(&SPAWN_GATE)))
}

pub(crate) fn acp_runtime_alive(sess: &AcpSession) -> bool {
    sess.hosted_handle.lock().unwrap().is_some() || sess.handle.lock().unwrap().is_some()
}

#[derive(Clone)]
pub(crate) struct AcpOpenRequest {
    pub(crate) id: String,
    pub(crate) cwd: Option<String>,
    pub(crate) launch: smelt_core::agent_kind::ConversationLaunchSpec,
    pub(crate) ephemeral_env: BTreeMap<String, String>,
    pub(crate) agent_needs_transcript_check: bool,
    pub(crate) resume_id: Option<String>,
    /// Pi `--fork` 源 session。新开会话，不得拿它去抢源会话的 resume 锁。
    pub(crate) fork_id: Option<String>,
    /// 分叉副本的切点（与 `fork_id` 搭配）：整拷打开后、重放前切到该用户消息
    /// 之前。见 `smelt_core::acp_conn::AcpForkCut`。
    pub(crate) fork_cut: Option<smelt_core::acp_conn::AcpForkCut>,
    pub(crate) tail_limit: Option<usize>,
    /// `None` 表示客户端未指定。已有会话仅在 daemon 自己仍未知时采用该值。
    pub(crate) conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
    /// 与 conversation binding 正交的产品级智能体/controller 实例绑定。
    pub(crate) agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    /// 仅在创建新的 daemon 会话时采用；热 attach 不得用客户端旧存档覆盖已消费值。
    pub(crate) pending_agent_preset: Option<String>,
}

pub(crate) fn parse_acp_open_request(v: &serde_json::Value) -> Option<AcpOpenRequest> {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return None;
    }
    let launch = v.get("launch").cloned().and_then(|value| {
        serde_json::from_value::<smelt_core::agent_kind::ConversationLaunchSpec>(value).ok()
    })?;
    if launch.command.trim().is_empty() {
        return None;
    }
    let conversation_binding = match v.get("conversation_binding") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    let agent_session = match v.get("agent_session") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    Some(AcpOpenRequest {
        id,
        cwd: v["cwd"].as_str().map(String::from),
        launch,
        ephemeral_env: v
            .get("ephemeral_env")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        agent_needs_transcript_check: v["agent"].as_str().unwrap_or("claude") == "claude",
        resume_id: normalize_resume_id(v["resume_id"].as_str().map(String::from)),
        fork_id: normalize_resume_id(v["fork_id"].as_str().map(String::from)),
        fork_cut: v
            .get("fork_cut")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tail_limit: v["tail_limit"]
            .as_u64()
            .map(|value| (value as usize).clamp(1, 500)),
        conversation_binding,
        agent_session,
        pending_agent_preset: v["pending_agent_preset"]
            .as_str()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(String::from),
    })
}

pub(crate) fn normalize_resume_id(resume_id: Option<String>) -> Option<String> {
    resume_id.and_then(|id| {
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_string())
    })
}

pub(crate) fn select_resume_id(
    requested: Option<String>,
    known_history: Option<String>,
) -> Option<String> {
    // relaunch 时 GUI 持久化的 canonical id 必须优先；daemon 里的 runtime id
    // 可能来自一次空恢复，不能反过来覆盖真正的 transcript id。
    normalize_resume_id(requested).or_else(|| normalize_resume_id(known_history))
}

pub(crate) fn known_acp_resume_id(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Option<String> {
    normalize_resume_id(reduced.history_session_id.clone()).or_else(|| {
        (!reduced.entries.is_empty())
            .then(|| normalize_resume_id(reduced.acp_session_id.clone()))
            .flatten()
    })
}

/// 活会话的全部独占身份。恢复后 canonical history id 与当前 runtime id 可能
/// 不同；两者都必须登记，避免从另一条入口并发 load 同一 provider 上下文。
pub(crate) fn live_acp_owner_ids(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Vec<String> {
    let mut ids = Vec::new();
    for id in [
        normalize_resume_id(reduced.history_session_id.clone()),
        normalize_resume_id(reduced.acp_session_id.clone()),
    ]
    .into_iter()
    .flatten()
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

pub(crate) fn acp_open_needs_relaunch(
    created: bool,
    alive: bool,
    has_launch_command: bool,
    requested_resume_id: Option<&str>,
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> bool {
    created
        || (!alive && has_launch_command)
        // A handoff/reconnect can leave a live handle in Starting without a
        // canonical id.  Do not hot-attach that half-started runtime when the
        // GUI still has the persisted history identity; relaunch it with that
        // identity so the normal load/new supervisor runs again.
        || (alive
            && requested_resume_id.is_some()
            && matches!(
                reduced.phase,
                smelt_core::daemon_state::DaemonPhase::Connecting
            )
            && reduced.history_session_id.as_deref() != requested_resume_id)
}

/// 已有 ACP 控制会话仍活着时，默认请求只是 attach；但启动命令或临时环境变了，
/// attach 不可能把新权限/凭据注入旧 agent 进程，必须带着历史身份重启一次。
/// 没有记录过的 launch 无法比较规格，不能因此杀掉仍活着的进程。
pub(crate) fn acp_runtime_needs_relaunch(
    alive: bool,
    current_launch: Option<&smelt_core::agent_kind::ConversationLaunchSpec>,
    current_ephemeral_env: &BTreeMap<String, String>,
    requested_launch: &smelt_core::agent_kind::ConversationLaunchSpec,
    requested_ephemeral_env: &BTreeMap<String, String>,
) -> bool {
    alive
        && current_launch.is_some_and(|current| {
            current != requested_launch || current_ephemeral_env != requested_ephemeral_env
        })
}

pub(crate) fn acp_runtime_spec_fingerprint(
    launch: &smelt_core::agent_kind::ConversationLaunchSpec,
    ephemeral_env: &BTreeMap<String, String>,
) -> String {
    use sha2::{Digest, Sha256};

    let encoded = serde_json::to_vec(&(launch, ephemeral_env)).unwrap_or_default();
    let digest = Sha256::digest(encoded);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// ACP 当前活动 → 状态通道 Phase。Thinking 还要看 entries 里有没有进行中的
/// 工具调用，细分成「执行工具」/「思考中」。
pub(crate) fn compute_acp_daemon_phase(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Phase {
    use smelt_core::acp_chat::has_unfinished_tool_call;
    use smelt_core::daemon_state::DaemonPhase;
    let executing_tool = !reduced.replaying_history && has_unfinished_tool_call(&reduced.entries);
    match reduced.phase {
        DaemonPhase::Connecting => Phase::Connecting,
        // `TurnEnded` 在 ACP 归约器里落成 Idle + completed_unread。若这里只投影
        // Idle，统一状态订阅永远看不到完成边沿，Dock/菜单栏/系统通知只能碰运气
        // 等别的 fallback。完成事实必须在 daemon 层直接成为 Succeeded；下一条
        // prompt 会清 completed_unread 并切回 Thinking。这个终态还必须优先于
        // 悬空工具明细：adapter 常会先发 TurnEnded、后补 ToolFinished，后者不能
        // 把已经确认的完成重新解释成“执行工具”。
        //
        // 没有 completed_unread 的 Idle 同样不能靠未收尾工具回到 ExecutingTool：
        // 那是回合被掐断后欠下的终态，不是当前活动。
        DaemonPhase::Idle if reduced.completed_unread => match reduced.turn_outcome {
            Some(smelt_core::acp_session::AcpTurnOutcome::Cancelled) => Phase::Idle,
            Some(outcome) if outcome.failure_message().is_some() => Phase::Failed,
            // 旧 handoff/快照没有 outcome；它们过去只表达“有完成结果”，兼容
            // 解释为成功，不能在升级后把历史正常完成突然翻成失败。
            Some(_) | None => Phase::Succeeded,
        },
        DaemonPhase::Idle => Phase::Idle,
        DaemonPhase::Thinking | DaemonPhase::ExecutingTool if executing_tool => {
            Phase::ExecutingTool
        }
        DaemonPhase::Thinking | DaemonPhase::ExecutingTool => Phase::Thinking,
        DaemonPhase::AwaitingApproval => Phase::AwaitingApproval,
        DaemonPhase::WaitingForUser => Phase::WaitingForUser,
        DaemonPhase::Succeeded => Phase::Succeeded,
        // Dead 只表示 Fatal / 恢复失败 / 连接意外断开；用户主动关闭会先把 slot
        // 摘出 registry 并使旧 drain 失效，不会走到这里广播。因此这是可通知的
        // 失败事实，而不是无声的普通 Dead。
        DaemonPhase::Dead | DaemonPhase::Failed => Phase::Failed,
    }
}

pub(crate) fn acp_pending_question(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Option<String> {
    if reduced.phase == smelt_core::daemon_state::DaemonPhase::Dead
        && !reduced.end_reason.trim().is_empty()
    {
        return Some(reduced.end_reason.clone());
    }
    if matches!(reduced.phase, smelt_core::daemon_state::DaemonPhase::Idle)
        && reduced.completed_unread
        && let Some(message) = reduced
            .turn_outcome
            .and_then(smelt_core::acp_session::AcpTurnOutcome::failure_message)
    {
        return Some(message.to_string());
    }
    reduced
        .permissions
        .first()
        .map(|p| p.question.clone())
        .or_else(|| reduced.elicitation.as_ref().map(|e| e.message.clone()))
}

/// 把归约状态里的相位/待办问句同步进四色 `SessionState` 并广播。跟旧版 GUI
/// `AcpView::sync_daemon_state` 是同一件事，只是现在算在 smeltd 侧。
pub(crate) fn update_acp_daemon_state(sess: &AcpSession, subscribers: &EventHubHandle) {
    let (phase, pending_question, title) = {
        let reduced = sess.reduced.lock().unwrap();
        (
            compute_acp_daemon_phase(&reduced),
            acp_pending_question(&reduced),
            reduced.resolved_session_title(),
        )
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let snapshot = {
        let mut st = sess.state.lock().unwrap();
        let _ = commit_session_phase(&mut st, PhaseSource::AcpProjection, phase);
        st.pending_question = pending_question;
        st.title = title;
        st.updated_at = now;
        bump_state_revision(&mut st);
        st.clone()
    };
    broadcast_state(subscribers, &snapshot);
}

/// 推一份最新快照给控制连接 + 全部旁观者。这里只复制到各 attachment 的输出邮箱；
/// 写线程发现断线或邮箱超限后，下一次入队会摘掉该连接（读循环也会在 EOF 时清理）。
/// `should_persist` 是
/// "这次变化是怎么发生的"这个上下文，调用方按场景传：事件驱动的走
/// `ApplyOutcome::should_persist`；用户动作（发 prompt/选权限）驱动的固定
/// false——跟旧版行为一致，用户主动发起的变化不单独触发落盘，等下一次
/// 协议事件（通常是 TurnEnded）时一并存。
pub(crate) fn push_acp_snapshot_since(
    sess: &AcpSession,
    should_persist: bool,
    entries_offset: Option<usize>,
) {
    let _output_gate = sess.output_gate.lock().unwrap();
    let mut snap = {
        let reduced = sess.reduced.lock().unwrap();
        let offset = entries_offset.unwrap_or(reduced.entries.len());
        let mut snapshot = reduced.to_snapshot_since(should_persist, offset);
        snapshot.snapshot_revision = sess.snapshot_revision.fetch_add(1, Ordering::SeqCst) + 1;
        snapshot
    };
    set_conversation_snapshot(sess, &mut snap);
    let provider_pid = sess
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|handle| handle.stdio.lock().unwrap().map(|stdio| stdio.pid));
    let mut payload = serde_json::json!({
        "snapshot": snap,
        "provider_pid": provider_pid,
    })
    .to_string()
    .into_bytes();
    payload.push(b'\n');
    let mut out = sess.out.lock().unwrap();
    if let Some(client) = out.client.take() {
        if client.enqueue(&payload) {
            out.client = Some(client);
        } else {
            client.close();
        }
    }
    let mut live_watchers = Vec::with_capacity(out.watchers.len());
    for watcher in out.watchers.drain(..) {
        if watcher.enqueue(&payload) {
            live_watchers.push(watcher);
        } else {
            watcher.close();
        }
    }
    out.watchers = live_watchers;
}

pub(crate) fn push_acp_snapshot(sess: &AcpSession, should_persist: bool) {
    let offset = sess.reduced.lock().unwrap().entries.len().saturating_sub(1);
    push_acp_snapshot_since(sess, should_persist, Some(offset));
}

pub(crate) fn set_conversation_snapshot(
    sess: &AcpSession,
    snapshot: &mut smelt_core::acp_session::ConversationSnapshot,
) {
    snapshot.conversation_state = Some(smelt_core::conversation::ConversationStateSnapshot {
        binding: sess.conversation_binding.lock().unwrap().clone(),
        agent_session: sess.agent_session.lock().unwrap().clone(),
        pending_agent_preset: sess.pending_agent_preset.lock().unwrap().clone(),
    });
}

/// 只在 daemon 自己还不知道路由时采用客户端值。已确认的 Direct/Plugin 不能被
/// 客户端存档覆盖，否则热 attach 会把 Task Runtime 刚写上的绑定打回旧快照。
fn adopt_unknown_conversation_binding(
    sess: &AcpSession,
    client: Option<smelt_core::conversation::ConversationBinding>,
) {
    let mut current = sess.conversation_binding.lock().unwrap();
    if current.is_none() {
        *current = client;
    }
}

/// 与输入 binding 相同，只允许在 daemon 尚未知时采用调用方值；热 attach 不能
/// 用旧客户端存档覆盖 Task Runtime 已登记的 controller 实例。
fn adopt_unknown_agent_session(
    sess: &AcpSession,
    client: Option<smelt_plugin_api::AgentSessionBinding>,
) {
    let mut current = sess.agent_session.lock().unwrap();
    if current.is_none() {
        *current = client;
    }
}

/// Direct / 自动化会话不声明插件路由，打开时不必读贡献快照。
/// 产品级会话只读启停时发布的缓存，不能去抢 Bun 控制锁。
pub(crate) fn conversation_needs_plugin_contributions(
    binding: Option<&smelt_core::conversation::ConversationBinding>,
    agent_session: Option<&smelt_plugin_api::AgentSessionBinding>,
) -> bool {
    agent_session.is_some()
        || matches!(
            binding,
            Some(smelt_core::conversation::ConversationBinding::Plugin { .. })
        )
}

fn contribution_sets_for_conversation(
    binding: Option<&smelt_core::conversation::ConversationBinding>,
    agent_session: Option<&smelt_plugin_api::AgentSessionBinding>,
) -> Vec<smelt_plugin_api::PluginContributionSet> {
    if conversation_needs_plugin_contributions(binding, agent_session) {
        crate::plugin_runtime::contributions()
    } else {
        Vec::new()
    }
}

pub(crate) fn validate_conversation_state(
    binding: Option<&smelt_core::conversation::ConversationBinding>,
    agent_session: Option<&smelt_plugin_api::AgentSessionBinding>,
    contribution_sets: &[smelt_plugin_api::PluginContributionSet],
) -> Result<(), String> {
    use smelt_core::conversation::ConversationBinding;
    use smelt_plugin_api::Contribution;

    let Some(ConversationBinding::Plugin { plugin_id, route }) = binding else {
        return if agent_session.is_none() {
            Ok(())
        } else {
            Err("agent session has no plugin input route".to_string())
        };
    };
    if let Some(agent_session) = agent_session
        && (&agent_session.agent.plugin_id != plugin_id
            || &agent_session.controller.plugin_id != plugin_id
            || &agent_session.instance.plugin_id != plugin_id)
    {
        return Err("agent session and input route belong to different plugins".to_string());
    }
    // 会话历史在插件暂不可用/停用时仍应可打开。活动声明不存在时只保留经过
    // 命名空间一致性校验的身份；真正产生副作用的 invocation 会再次调用本函数，
    // 随后由路由查找明确拒绝 unavailable contribution。
    let Some(set) = contribution_sets
        .iter()
        .find(|set| &set.plugin_id == plugin_id)
    else {
        return Ok(());
    };
    if !set.contributions.iter().any(|contribution| {
        matches!(contribution, Contribution::InputRoute { id, .. } if id == &route.contribution_id)
    }) {
        return Err("conversation input route is not declared by the plugin".to_string());
    }
    let Some(agent_session) = agent_session else {
        return Ok(());
    };
    let declared_controller =
        set.contributions
            .iter()
            .find_map(|contribution| match contribution {
                Contribution::Agent { id, controller, .. }
                    if id == &agent_session.agent.contribution_id =>
                {
                    Some(controller)
                }
                _ => None,
            });
    if declared_controller != Some(&agent_session.controller.contribution_id) {
        return Err("agent does not declare the bound session controller".to_string());
    }
    let declared_input_route =
        set.contributions
            .iter()
            .find_map(|contribution| match contribution {
                Contribution::SessionController { id, input_route }
                    if id == &agent_session.controller.contribution_id =>
                {
                    Some(input_route)
                }
                _ => None,
            });
    if declared_input_route != Some(&route.contribution_id) {
        return Err("session controller does not declare the bound input route".to_string());
    }
    Ok(())
}

/// 在持有 [`AcpSession::turn_completion`] 时，根据最新状态释放刚结束回合的
/// prompt 闸门并最多派发一条排队消息。
///
/// `TurnEnded` 是回合结束的唯一事实。未完成工具只影响展示：迟到的
/// `ToolFinished` 仍可改写旧 tool id；下一条 prompt 发出时再收尾仍未终态的工具。
/// 不按时间猜尾包，也不为悬空工具再握闸门。
pub(crate) fn settle_acp_turn_locked(
    sess: &AcpSession,
    turn_ended: bool,
    ready: bool,
    subscribers: &EventHubHandle,
) {
    let idle = matches!(
        sess.reduced.lock().unwrap().phase,
        smelt_core::daemon_state::DaemonPhase::Idle
    );
    if !idle {
        return;
    }
    let gate_was_held = sess.prompt_in_flight.swap(false, Ordering::SeqCst);
    if gate_was_held || turn_ended || ready {
        flush_pending_acp_prompt_locked(sess, subscribers);
    }
}

/// 事件 drain：整个会话生命周期只有这一条线程在改 `reduced`（`apply_acp_user_action`
/// 里权限/选择题相关的写也在这条线程外发生，但两边改的是不相交的字段/走
/// 互斥锁，不会踩踏）。通道关闭（连接线程收尾）就退出；如果退出时相位还不是
/// `Ended`（没收到终态事件就断，比如连接线程 panic），兜底补一个 Ended，不让
/// GUI 永远卡在「运行中」。恢复失败由这里的 supervisor 统一决定是报错还是
/// 对空白会话重新走 `session/new`。
pub(crate) fn start_acp_event_drain(
    slot: Arc<AcpSlot<AcpSession>>,
    event_rx: smol::channel::Receiver<smelt_core::acp_conn::ConversationEvent>,
    subscribers: EventHubHandle,
    restore_policy: Option<AcpRestorePolicy>,
    session_id: String,
    acp_sessions: AcpSessions,
    generation: u64,
) {
    thread::spawn(move || {
        let sess = &slot.value;
        let mut fallback_to_fresh = false;
        smol::block_on(async {
            while let Ok(ev) = event_rx.recv().await {
                let _turn_completion = sess.turn_completion.lock().unwrap();
                if sess.connection_generation.load(Ordering::SeqCst) != generation {
                    break;
                }
                let turn_ended = ev.ends_turn();
                let ready = matches!(&ev, smelt_core::acp_conn::ConversationEvent::Ready { .. });
                if matches!(restore_policy, Some(AcpRestorePolicy::FreshOnMissing))
                    && matches!(
                        &ev,
                        smelt_core::acp_conn::ConversationEvent::RestoreFailed(failure)
                            if failure.allows_fresh_session()
                    )
                {
                    // 这是一个没有任何本地消息的失效历史身份：不要先把失败态
                    // 广播给 GUI，直接在旧连接收尾后用同一个 smeltd slot 开
                    // `session/new`。有本地投影的历史和瞬态错误继续走严格失败路径。
                    fallback_to_fresh = true;
                    continue;
                }
                if matches!(
                    &ev,
                    smelt_core::acp_conn::ConversationEvent::Ready {
                        kind: smelt_core::acp_conn::ReadyKind::ResumedWithReplay
                            | smelt_core::acp_conn::ReadyKind::ResumedKeepHistory,
                        ..
                    }
                ) {
                    let history_id = sess.reduced.lock().unwrap().history_session_id.clone();
                    sess.restore_state
                        .lock()
                        .unwrap()
                        .mark_restored(history_id.as_deref());
                }
                let (outcome, owner_ids) = {
                    let mut st = sess.reduced.lock().unwrap();
                    let outcome = smelt_core::acp_session::apply_event(&mut st, ev);
                    let owner_ids = live_acp_owner_ids(&st);
                    (outcome, owner_ids)
                };
                if let Err(error) = acp_sessions.try_acquire_resumes(&owner_ids, &session_id) {
                    let mut reduced = sess.reduced.lock().unwrap();
                    smelt_core::acp_session::force_end(
                        &mut reduced,
                        smelt_core::acp_session::AcpEndKind::SessionOwnershipConflict,
                        error.to_string(),
                    );
                    drop(reduced);
                    push_acp_snapshot_since(sess, true, outcome.entries_offset);
                    update_acp_daemon_state(sess, &subscribers);
                    break;
                }
                push_acp_snapshot_since(sess, outcome.should_persist, outcome.entries_offset);
                update_acp_daemon_state(sess, &subscribers);
                // TurnEnded / Ready 之后只要相位已 Idle 就放闸。迟到工具终态
                // 按 tool id 归到旧条目，不会把新回合重新打开。
                settle_acp_turn_locked(sess, turn_ended, ready, &subscribers);
            }
        });
        let _lifecycle = slot.lifecycle.lock().unwrap();
        let _turn_completion = sess.turn_completion.lock().unwrap();
        if sess.connection_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if !retire_acp_runtime(sess) {
            eprintln!("[acp] 会话 {session_id} 未能证明旧 provider 已退出，拒绝再 spawn");
            let mut reduced = sess.reduced.lock().unwrap();
            smelt_core::acp_session::force_end(
                &mut reduced,
                smelt_core::acp_session::AcpEndKind::ProviderFailed,
                "旧 provider 未能退出",
            );
            drop(reduced);
            push_acp_snapshot_since(sess, true, None);
            update_acp_daemon_state(sess, &subscribers);
            return;
        }
        if fallback_to_fresh && let Some(launch) = sess.launch_spec.lock().unwrap().clone() {
            // `acp_relaunch(None)` 会清掉旧的 canonical history id，再走
            // `session/new`；标题、smeltd slot 和 GUI 控制连接都保持不变。
            drop(_turn_completion);
            acp_sessions.release_resume_for(&session_id);
            acp_relaunch(
                &slot,
                &session_id,
                launch,
                None,
                None,
                None,
                Arc::clone(&acp_sessions),
                &subscribers,
            );
            return;
        }
        let already_ended = matches!(
            sess.reduced.lock().unwrap().phase,
            smelt_core::daemon_state::DaemonPhase::Dead
        );
        if already_ended {
            // session host 的 payload 还带 provider_pid。终态事件可能先于 waitpid
            // 到达；收尸后再发一帧 None，清掉主 daemon 手里的旧 pid 水位。
            push_acp_snapshot_since(sess, false, None);
        } else {
            let mut reduced = sess.reduced.lock().unwrap();
            smelt_core::acp_session::force_end(
                &mut reduced,
                smelt_core::acp_session::AcpEndKind::TransportDisconnected,
                "连接意外中断",
            );
            drop(reduced);
            push_acp_snapshot_since(sess, true, None);
        }
        update_acp_daemon_state(sess, &subscribers);
    });
}

/// 主 daemon 的镜像 drain。权威状态和所有 responder 都在独立 session host；
/// 这里仅按版本合并快照、维护 provider-session 所有权并向 GUI/状态总线广播。
/// exec 可能让旧 reader 已从 socket 取走、却尚未来得及归约的字节消失；版本缺口
/// 或 entry 前缀缺失时必须要求 host 发 offset=0 的全量快照。
pub(crate) fn start_hosted_snapshot_drain(
    slot: Arc<AcpSlot<AcpSession>>,
    snapshot_rx: smol::channel::Receiver<acp_runtime_host::HostedSnapshotEnvelope>,
    subscribers: EventHubHandle,
    session_id: String,
    acp_sessions: AcpSessions,
    generation: u64,
) {
    thread::spawn(move || {
        let sess = &slot.value;
        let mut awaiting_full_snapshot = false;
        smol::block_on(async {
            while let Ok(envelope) = snapshot_rx.recv().await {
                if sess.connection_generation.load(Ordering::SeqCst) != generation {
                    break;
                }
                let _provider_pid = envelope.provider_pid;
                let snapshot = envelope.snapshot;
                let revision = snapshot.snapshot_revision;
                let seen = sess.host_snapshot_revision.load(Ordering::SeqCst);
                if revision != 0 && revision <= seen {
                    continue;
                }

                let incremental_gap = snapshot.entries_offset > 0
                    && seen != 0
                    && revision != 0
                    && revision > seen.saturating_add(1);
                if (awaiting_full_snapshot || incremental_gap) && snapshot.entries_offset != 0 {
                    if !awaiting_full_snapshot {
                        awaiting_full_snapshot = true;
                        let request_ok = sess
                            .hosted_handle
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|host| host.request_full_snapshot().is_ok());
                        if !request_ok {
                            break;
                        }
                    }
                    continue;
                }

                let should_persist = snapshot.should_persist;
                let requested_offset = snapshot.entries_offset;
                let owner_ids = {
                    let mut reduced = sess.reduced.lock().unwrap();
                    match reduced.merge_hosted_snapshot(snapshot) {
                        Ok(_) => live_acp_owner_ids(&reduced),
                        Err(error) => {
                            eprintln!("[acp] 会话 {session_id} 宿主快照无法连续合并：{error}");
                            drop(reduced);
                            awaiting_full_snapshot = true;
                            let request_ok = sess
                                .hosted_handle
                                .lock()
                                .unwrap()
                                .as_ref()
                                .is_some_and(|host| host.request_full_snapshot().is_ok());
                            if !request_ok {
                                break;
                            }
                            continue;
                        }
                    }
                };
                awaiting_full_snapshot = false;
                if revision != 0 {
                    sess.host_snapshot_revision
                        .store(revision, Ordering::SeqCst);
                }

                if let Err(error) = acp_sessions.try_acquire_resumes(&owner_ids, &session_id) {
                    let mut reduced = sess.reduced.lock().unwrap();
                    smelt_core::acp_session::force_end(
                        &mut reduced,
                        smelt_core::acp_session::AcpEndKind::SessionOwnershipConflict,
                        error.to_string(),
                    );
                    drop(reduced);
                    push_acp_snapshot_since(sess, true, Some(requested_offset));
                    update_acp_daemon_state(sess, &subscribers);
                    break;
                }
                acp_sessions.retain_resumes_for(&session_id, &owner_ids);

                let gate_held = {
                    let reduced = sess.reduced.lock().unwrap();
                    reduced.turn_started_at_ms.is_some()
                        || matches!(
                            reduced.phase,
                            smelt_core::daemon_state::DaemonPhase::Thinking
                                | smelt_core::daemon_state::DaemonPhase::ExecutingTool
                                | smelt_core::daemon_state::DaemonPhase::AwaitingApproval
                                | smelt_core::daemon_state::DaemonPhase::WaitingForUser
                        )
                };
                sess.prompt_in_flight.store(gate_held, Ordering::SeqCst);
                push_acp_snapshot_since(sess, should_persist, Some(requested_offset));
                update_acp_daemon_state(sess, &subscribers);
            }
        });

        let _lifecycle = slot.lifecycle.lock().unwrap();
        if sess.connection_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if !retire_acp_runtime(sess) {
            eprintln!("[acp] 会话 {session_id} 的独立宿主未能退出");
        }
        let already_ended = matches!(
            sess.reduced.lock().unwrap().phase,
            smelt_core::daemon_state::DaemonPhase::Dead
        );
        if !already_ended {
            let mut reduced = sess.reduced.lock().unwrap();
            smelt_core::acp_session::force_end(
                &mut reduced,
                smelt_core::acp_session::AcpEndKind::TransportDisconnected,
                "ACP 会话宿主意外中断",
            );
            drop(reduced);
            push_acp_snapshot_since(sess, true, None);
        }
        update_acp_daemon_state(sess, &subscribers);
    });
}

/// spawn 一次连接（首次建会话 / 「重新开始」共用）：先按旧版 GUI `restart()`
/// 的规则重置回合态字段，再起连接线程、挂事件 drain。
#[allow(clippy::too_many_arguments)]
pub(crate) fn acp_relaunch(
    slot: &Arc<AcpSlot<AcpSession>>,
    id: &str,
    mut launch: smelt_core::agent_kind::ConversationLaunchSpec,
    resume_id: Option<String>,
    fork_id: Option<String>,
    fork_cut: Option<smelt_core::acp_conn::AcpForkCut>,
    acp_sessions: AcpSessions,
    subscribers: &EventHubHandle,
) {
    let sess = &slot.value;
    let (generation, restore_policy) = {
        let _turn_completion = sess.turn_completion.lock().unwrap();
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        // 每次换连接都推进代数。旧 drain 即使随后才看到 event_rx 关闭，也不能
        // 把新连接的 handle/Starting 状态清掉。
        let generation = sess.connection_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let restore_policy = {
            let mut reduced = sess.reduced.lock().unwrap();
            let previous_history_session_id = reduced.history_session_id.clone();
            let restore_policy = sess.restore_state.lock().unwrap().begin(
                if fork_id.is_some() {
                    None
                } else {
                    resume_id.as_deref()
                },
                !reduced.entries.is_empty(),
            );
            smelt_core::acp_session::reset_for_restart(&mut reduced);
            // `resume_id` 只是这次 session/load 的候选身份，不能在 load 成功前
            // 写进 canonical history。否则 GUI 收到 spawn 后的第一份 Starting
            // 快照就会把一个可能不存在的 id 持久化成“已确认历史”，下一次启动
            // 又会无条件进入恢复。真正成功恢复时由 Ready 事件登记 history id；
            // session/new 的首条 prompt 发出后也由 note_prompt_sent 登记。
            // 同一 canonical id 的重启保留它，以便瞬断后重试；切换到另一个
            // candidate 或明确走 session/new 时清掉旧身份，避免 Ready 误沿用旧值。
            if previous_history_session_id.as_deref() != resume_id.as_deref() {
                reduced.history_session_id = None;
            }
            restore_policy
        };
        (generation, restore_policy)
    };
    // 无头恢复不会经过 GUI 的 AgentHostState 迁移；daemon 自己也要把 Smelt
    // 发布过的旧默认值升到当前适配器，否则历史会话 relaunch 仍会拉旧包。
    upgrade_released_adapter_launch(&mut launch);
    let needs_check = sess.agent_needs_transcript_check;
    let ephemeral_env = sess.ephemeral_env.lock().unwrap().clone();
    // 记住这次真正用过的完整启动规格，`acp_restart` 卡死重启时不必依赖 GUI
    // 重新把 launch 传一遍（GUI 那条 acp_open 连接可能压根没断，不会重新握手）。
    *sess.launch_spec.lock().unwrap() = Some(launch.clone());
    *sess.runtime_spec_fingerprint.lock().unwrap() =
        Some(acp_runtime_spec_fingerprint(&launch, &ephemeral_env));
    let agent_mcp = smelt_core::agent_bus::cross_agent_enabled()
        && smelt_core::agent_bus::mcp_executable_path().is_file();
    let agent_token = {
        let mut state = sess.state.lock().unwrap();
        state.agent_mcp = agent_mcp;
        state.agent_token.clone()
    };
    let agent_mcp_cli_args = if agent_mcp {
        acp_agent_mcp_cli_args(&launch, id, &agent_token)
    } else {
        Vec::new()
    };
    sess.state.lock().unwrap().launch = Some(launch.command.clone());

    // 会话跑独立宿主还是同进程 direct driver，由注册表在构造时定好，而不是在这里
    // 反问「我是不是测试构建」。单测二进制的 `current_exe()` 是 libtest harness，
    // 不是可用的 smeltd entrypoint，所以测试注册表选 SameProcess；独立宿主边界由
    // 专门的 IPC/handoff 用例覆盖。
    if acp_sessions.spawn_policy() == acp_registry::AcpSpawnPolicy::HostedProcess
        && !acp_runtime_host::is_session_host_process()
    {
        let seed_snapshot = sess.reduced.lock().unwrap().to_snapshot(false);
        let initial_open = serde_json::json!({
            "op": "acp_open",
            "id": id,
            "cwd": sess.cwd,
            "launch": launch,
            "ephemeral_env": ephemeral_env,
            "agent": if needs_check { "claude" } else { "other" },
            "resume_id": resume_id,
            "fork_id": fork_id,
            "fork_cut": &fork_cut,
            "conversation_binding": sess.conversation_binding.lock().unwrap().clone(),
            "agent_session": sess.agent_session.lock().unwrap().clone(),
            "host_agent_token": agent_token,
            "host_seed_snapshot": seed_snapshot,
        });
        match acp_runtime_host::HostedConversationHandle::spawn(
            &initial_open,
            &acp_sessions.spawn_gate(),
        ) {
            Ok(hosted) => {
                let snapshot_rx = hosted.snapshot_rx();
                *sess.hosted_handle.lock().unwrap() = Some(hosted);
                sess.host_snapshot_revision.store(0, Ordering::SeqCst);
                push_acp_snapshot(sess, false);
                update_acp_daemon_state(sess, subscribers);
                start_hosted_snapshot_drain(
                    Arc::clone(slot),
                    snapshot_rx,
                    subscribers.clone(),
                    id.to_string(),
                    acp_sessions,
                    generation,
                );
            }
            Err(error) => {
                eprintln!("[acp] 会话 {id} 的独立宿主启动失败：{error}");
                let mut reduced = sess.reduced.lock().unwrap();
                smelt_core::acp_session::force_end(
                    &mut reduced,
                    smelt_core::acp_session::AcpEndKind::ProviderFailed,
                    format!("ACP 会话宿主启动失败：{error}"),
                );
                drop(reduced);
                push_acp_snapshot_since(sess, true, None);
                update_acp_daemon_state(sess, subscribers);
            }
        }
        return;
    }

    let app_launch = smelt_core::acp_conn::ConversationLaunch {
        launch,
        ephemeral_env,
        cwd: sess.cwd.clone(),
        sid: id.to_string(),
        agent_token,
        agent_mcp,
        agent_mcp_cli_args,
        resume_session_id: if fork_id.is_some() {
            None
        } else {
            resume_id.map(agent_client_protocol::schema::v1::SessionId::new)
        },
        fork_session_id: fork_id.map(agent_client_protocol::schema::v1::SessionId::new),
        fork_cut,
        resume_needs_transcript_check: needs_check,
    };
    let handle =
        smelt_core::acp_conn::spawn_agent_runtime(app_launch, Some(acp_sessions.spawn_gate()));
    let event_rx = handle.event_rx.clone();
    *sess.handle.lock().unwrap() = Some(handle);
    push_acp_snapshot(sess, false); // 刚 spawn，还没有新内容，不用触发落盘
    update_acp_daemon_state(sess, subscribers);
    start_acp_event_drain(
        Arc::clone(slot),
        event_rx,
        subscribers.clone(),
        restore_policy,
        id.to_string(),
        acp_sessions,
        generation,
    );
}

pub(crate) fn upgrade_released_adapter_launch(
    launch: &mut smelt_core::agent_kind::ConversationLaunchSpec,
) -> bool {
    let Some(agent) =
        smelt_core::agent_kind::ConversationAgentKind::from_command_loose(&launch.command)
    else {
        return false;
    };
    let Some(upgraded) = agent.upgrade_released_default_command(&launch.command) else {
        return false;
    };
    launch.command = upgraded;
    true
}

/// 在会话生命周期锁内淘汰当前 provider。先推进 generation，旧 drain 从此不能
/// 再改归约状态；随后等待连接线程退出，再 SIGKILL + 死亡证明。只有证明了才
/// 返回 true（亲生：`waitpid`；收养：组缺席探针——handoff 后 successor 永远
/// 不是父进程）。失败时留下 unreaped_pid，调用方不得 spawn。
pub(crate) fn retire_acp_runtime(sess: &AcpSession) -> bool {
    sess.connection_generation.fetch_add(1, Ordering::SeqCst);
    // unreaped 重试只做 prove、不重杀：上次已经发过 SIGKILL；此 pid 若已被
    // 复用，重杀会误伤无辜进程组。探得仍在就继续留着不 spawn（fail closed）。
    // 先 take 掉再判：`if let` 的 scrutinee guard 会活过整个 body，里面再
    // lock 同一把就是自死锁（之前 instant-true 从不进 body，雷没爆过）。
    let unreaped = sess.unreaped_pid.lock().unwrap().take();
    if let Some(pid) = unreaped
        && !smelt_core::acp_conn::prove_process_group_dead(pid, ACP_SHUTDOWN_GRACE)
    {
        *sess.unreaped_pid.lock().unwrap() = Some(pid);
        return false;
    }
    if let Some(hosted) = sess.hosted_handle.lock().unwrap().take() {
        let pid = hosted.pid();
        if hosted.shutdown_and_wait(ACP_SHUTDOWN_GRACE) {
            return true;
        }
        *sess.unreaped_pid.lock().unwrap() = Some(pid);
        return false;
    }
    let Some(handle) = sess.handle.lock().unwrap().take() else {
        return true;
    };
    let pid = handle.stdio.lock().unwrap().map(|process| process.pid);
    if smelt_core::acp_conn::shutdown_and_wait(handle, ACP_SHUTDOWN_GRACE) {
        return true;
    }
    if let Some(pid) = pid {
        *sess.unreaped_pid.lock().unwrap() = Some(pid);
    }
    false
}

pub(crate) fn make_acp_session(
    id: &str,
    cwd: Option<String>,
    agent_needs_transcript_check: bool,
    conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
    agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    pending_agent_preset: Option<String>,
) -> AcpSession {
    let instance = next_session_instance();
    AcpSession {
        instance,
        reduced: Mutex::new(smelt_core::acp_session::AcpSessionState::default()),
        snapshot_revision: AtomicU64::new(0),
        connection_generation: AtomicU64::new(0),
        turn_completion: Mutex::new(()),
        prompt_in_flight: AtomicBool::new(false),
        pending_prompts: Mutex::new(VecDeque::new()),
        hosted_handle: Mutex::new(None),
        host_snapshot_revision: AtomicU64::new(0),
        handle: Mutex::new(None),
        unreaped_pid: Mutex::new(None),
        cwd: cwd.clone(),
        agent_needs_transcript_check,
        state: Arc::new(Mutex::new(SessionState {
            id: id.to_string(),
            instance,
            cwd,
            launch: None,
            provider: None,
            conversation_id: None,
            agent_mcp: false,
            agent_token: uuid::Uuid::new_v4().simple().to_string(),
            title: None,
            prompt_title: None,
            phase: Phase::Idle,
            phase_since: 0,
            pending_question: None,
            tokens_used: None,
            branch: None,
            dirty_files: Vec::new(),
            revision: 0,
            updated_at: 0,
            structured_events: true,
            turn_events: true,
            agent_event_version: None,
            active_blocker: None,
            runtime: true,
        })),
        output_gate: Mutex::new(()),
        out: Mutex::new(AcpOut {
            client: None,
            watchers: Vec::new(),
        }),
        launch_spec: Mutex::new(None),
        runtime_spec_fingerprint: Mutex::new(None),
        restore_state: Mutex::new(AcpRestoreState::Fresh),
        ephemeral_env: Mutex::new(BTreeMap::new()),
        conversation_binding: Mutex::new(conversation_binding),
        agent_session: Mutex::new(agent_session),
        conversation_submit: Mutex::new(()),
        pending_agent_preset: Mutex::new(pending_agent_preset),
    }
}

/// 在调用方取得 daemon 回合槽位后发送 prompt。状态回显也放在这里，避免排队的
/// prompt 在上一轮 TurnEnded 归约前就被记录到消息流。
pub(crate) fn send_acp_prompt_reserved(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    delivery_id: Option<String>,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let handle = sess
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .map(|h| (h.cmd_tx.clone(), Arc::clone(&h.in_flight_rpc)));
    let Some((cmd_tx, in_flight_rpc)) = handle else {
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        return Err("ACP session is not running");
    };
    let shown_images = images.clone();
    let shown_text = text.clone();
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::Prompt { text, images })
        .is_ok()
    {
        smelt_core::acp_session::note_prompt_sent_with_delivery(
            &mut sess.reduced.lock().unwrap(),
            shown_text,
            shown_images,
            delivery_id,
        );
        push_acp_snapshot(sess, false);
        update_acp_daemon_state(sess, subscribers);
        Ok(())
    } else {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        Err("ACP command channel is busy or closed")
    }
}

/// 把一条消息插进正在跑的回合。只有 `supports_mid_turn_input` 的驱动能走这里：
/// 不占 `prompt_in_flight` 闸门（当前回合仍归上一条 prompt），不进 daemon 队列。
/// 先进入 steering 队列，等 Pi 吃掉后再写入消息流。
/// 返回 `Ok(false)` 表示这条会话不支持插入，调用方应回退到排队。
pub(crate) fn send_acp_mid_turn_input(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    subscribers: &EventHubHandle,
) -> Result<bool, &'static str> {
    let handle = sess.handle.lock().unwrap().as_ref().map(|h| {
        (
            h.cmd_tx.clone(),
            Arc::clone(&h.in_flight_rpc),
            h.supports_mid_turn_input,
        )
    });
    let Some((cmd_tx, in_flight_rpc, supported)) = handle else {
        return Err("ACP session is not running");
    };
    if !supported {
        return Ok(false);
    }
    // 先入本地队列再发给 Pi，避免回执/queue_update 抢在乐观更新前面造成重复。
    smelt_core::acp_session::queue_mid_turn_input(
        &mut sess.reduced.lock().unwrap(),
        text.clone(),
        images.clone(),
    );
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::Steer { text, images })
        .is_err()
    {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        let mut state = sess.reduced.lock().unwrap();
        let _ = state.queued_steering.pop();
        let _ = state.queued_steering_images.pop();
        return Err("ACP command channel is busy or closed");
    }
    push_acp_snapshot(sess, false);
    update_acp_daemon_state(sess, subscribers);
    Ok(true)
}

fn send_acp_follow_up(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    delivery_id: Option<String>,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let handle = sess.handle.lock().unwrap().as_ref().map(|h| {
        (
            h.cmd_tx.clone(),
            Arc::clone(&h.in_flight_rpc),
            h.supports_native_queue,
        )
    });
    let Some((cmd_tx, in_flight_rpc, supported)) = handle else {
        return Err("ACP session is not running");
    };
    if !supported {
        return apply_acp_user_action_inner(
            sess,
            smelt_core::acp_session::AcpUserAction::Prompt {
                text,
                images,
                delivery_id,
            },
            subscribers,
            true,
        );
    }
    if !sess.prompt_in_flight.load(Ordering::SeqCst) {
        return apply_acp_user_action_inner(
            sess,
            smelt_core::acp_session::AcpUserAction::Prompt {
                text,
                images,
                delivery_id,
            },
            subscribers,
            true,
        );
    }
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::FollowUp { text, images })
        .is_err()
    {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        return Err("ACP command channel is busy or closed");
    }
    push_acp_snapshot_since(sess, false, None);
    Ok(())
}

fn send_acp_compact(sess: &AcpSession) -> Result<(), &'static str> {
    let handle = sess.handle.lock().unwrap().as_ref().map(|h| {
        (
            h.cmd_tx.clone(),
            Arc::clone(&h.in_flight_rpc),
            h.supports_compaction,
        )
    });
    let Some((cmd_tx, in_flight_rpc, supported)) = handle else {
        return Err("ACP session is not running");
    };
    if !supported {
        return Err("agent does not support compaction");
    }
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::Compact {
            custom_instructions: None,
        })
        .is_err()
    {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        return Err("ACP command channel is busy or closed");
    }
    Ok(())
}

fn send_acp_clear_queue(sess: &AcpSession) -> Result<(), &'static str> {
    let handle = sess.handle.lock().unwrap().as_ref().map(|h| {
        (
            h.cmd_tx.clone(),
            Arc::clone(&h.in_flight_rpc),
            h.supports_native_queue,
        )
    });
    let Some((cmd_tx, in_flight_rpc, supported)) = handle else {
        return Err("ACP session is not running");
    };
    if !supported {
        return Err("agent does not support native queue");
    }
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::ClearQueue)
        .is_err()
    {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        return Err("ACP command channel is busy or closed");
    }
    Ok(())
}

/// 回退到某条历史用户消息。入口是 GUI 传来的 entry 下标，但下标背后的一切
/// 验证（相位、消息类型、同文本序号）都以 daemon 自己的投影为准——GUI 的
/// 本地列表可能与投影有短暂分页差异，不能拿它当事实源。
fn send_acp_rewind(sess: &AcpSession, entry_index: usize) -> Result<(), &'static str> {
    let handle = sess.handle.lock().unwrap().as_ref().map(|h| {
        (
            h.cmd_tx.clone(),
            Arc::clone(&h.in_flight_rpc),
            h.supports_rewind,
        )
    });
    let Some((cmd_tx, in_flight_rpc, supported)) = handle else {
        return Err("ACP session is not running");
    };
    if !supported {
        return Err("agent does not support rewind");
    }
    // 回退会丢弃之后的所有回合与未决审批，只能在完全空闲的会话上发生。
    // 相位不是 Idle 时拒绝，不给“边跑边回退”留出任何竞态窗口。
    let (phase, target) = {
        let reduced = sess.reduced.lock().unwrap();
        (
            reduced.phase,
            reduced.entries.get(entry_index).map(|entry| match entry {
                smelt_core::acp_chat::AcpEntry::User(text) => Some(text.clone()),
                smelt_core::acp_chat::AcpEntry::UserWithImages { text, .. } => Some(text.clone()),
                _ => None,
            }),
        )
    };
    if phase != smelt_core::daemon_state::DaemonPhase::Idle {
        return Err("rewind requires an idle session");
    }
    let Some(Some(text)) = target else {
        return Err("rewind target is not a user message");
    };
    if smelt_core::acp_chat::is_interrupt_marker(&text) {
        return Err("rewind target is not a user message");
    }
    // 同文本消息靠序号区分：agent 的可分叉列表里取第 N 条同文本消息，N 必须与
    // 本地同文本计数一致。中断标记不是真消息，不参与计数。
    let occurrence = {
        let reduced = sess.reduced.lock().unwrap();
        reduced
            .entries
            .iter()
            .take(entry_index)
            .filter(|entry| match entry {
                smelt_core::acp_chat::AcpEntry::User(t) => t == &text,
                smelt_core::acp_chat::AcpEntry::UserWithImages { text: t, .. } => t == &text,
                _ => false,
            })
            .count()
    };
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::Rewind {
            text,
            occurrence,
            truncate_from: entry_index,
        })
        .is_err()
    {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        return Err("ACP command channel is busy or closed");
    }
    Ok(())
}

/// 回合结束后只释放一条 daemon 队列中的 prompt。调用方必须持有/// `turn_completion`；剩余消息等下一次 TurnEnded，保证 provider 永远不会看到
/// 并发回合。
pub(crate) fn flush_pending_acp_prompt_locked(sess: &AcpSession, subscribers: &EventHubHandle) {
    if sess.prompt_in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(prompt) = sess.pending_prompts.lock().unwrap().pop_front() else {
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        return;
    };
    if send_acp_prompt_reserved(
        sess,
        prompt.text.clone(),
        prompt.images.clone(),
        prompt.delivery_id.clone(),
        subscribers,
    )
    .is_err()
    {
        sess.pending_prompts.lock().unwrap().push_front(prompt);
    }
}

/// Clears a completed delivery from the daemon's deduplication ledger. It is
pub(crate) fn apply_acp_user_action(
    sess: &AcpSession,
    action: smelt_core::acp_session::AcpUserAction,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    apply_acp_user_action_inner(sess, action, subscribers, false)
}

fn apply_acp_user_action_inner(
    sess: &AcpSession,
    action: smelt_core::acp_session::AcpUserAction,
    subscribers: &EventHubHandle,
    allow_automation_continuation: bool,
) -> Result<(), &'static str> {
    use smelt_core::acp_conn::ConversationCommand;
    use smelt_core::acp_session::AcpUserAction;
    if let AcpUserAction::Prompt { delivery_id, .. } | AcpUserAction::FollowUp { delivery_id, .. } =
        &action
        && let Some(smelt_core::conversation::ConversationBinding::Automation { run_id }) =
            &*sess.conversation_binding.lock().unwrap()
        && !allow_automation_continuation
        && delivery_id.as_deref() != Some(run_id.as_str())
    {
        return Err("automation Run input is daemon-owned");
    }
    if let Some(hosted) = sess.hosted_handle.lock().unwrap().as_ref() {
        return hosted.send_action(&action).map_err(|error| {
            eprintln!("[acp] 转发用户动作到独立宿主失败：{error}");
            "ACP session host is unavailable"
        });
    }
    match action {
        AcpUserAction::Refresh => {
            push_acp_snapshot_since(sess, false, Some(0));
            Ok(())
        }
        AcpUserAction::RestartRuntime => Err("runtime restart must be handled by the session host"),
        AcpUserAction::Prompt {
            text,
            images,
            delivery_id,
        } => {
            // 从取得 gate 到把本地投影切到 Running 必须与回合收尾串行。否则
            // 看门狗/迟到事件可能在 gate 已被这条新 prompt 占住、但状态尚未
            // 更新时把 gate 再次释放，从而并发派发下一条 prompt。
            let _turn_completion = sess.turn_completion.lock().unwrap();
            if sess.handle.lock().unwrap().is_none() {
                return Err("ACP session is not running");
            }
            if delivery_id.as_deref().is_some_and(|delivery_id| {
                sess.reduced
                    .lock()
                    .unwrap()
                    .accepted_delivery_ids
                    .contains(delivery_id)
            }) {
                // 上一次确认快照可能在客户端崩溃前没有落地；重复请求只重发事实，
                // 绝不再次进入 provider 队列。
                push_acp_snapshot_since(sess, false, None);
                return Ok(());
            }
            if let Some(delivery_id) = delivery_id.as_ref() {
                sess.reduced
                    .lock()
                    .unwrap()
                    .accepted_delivery_ids
                    .insert(delivery_id.clone());
            }
            if sess.prompt_in_flight.swap(true, Ordering::SeqCst) {
                // 回合正在跑。驱动支持中途插入（Pi 的 steer）就直接送进当前
                // 回合，不占闸门；不支持的才排到回合结束后再发。
                match send_acp_mid_turn_input(sess, text.clone(), images.clone(), subscribers) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        if let Some(delivery_id) = delivery_id.as_deref() {
                            sess.reduced
                                .lock()
                                .unwrap()
                                .accepted_delivery_ids
                                .remove(delivery_id);
                        }
                        return Err(error);
                    }
                }
                sess.pending_prompts
                    .lock()
                    .unwrap()
                    .push_back(QueuedAcpPrompt {
                        text,
                        images,
                        delivery_id,
                    });
                push_acp_snapshot_since(sess, false, None);
                return Ok(());
            }
            let result =
                send_acp_prompt_reserved(sess, text, images, delivery_id.clone(), subscribers);
            if result.is_err()
                && let Some(delivery_id) = delivery_id.as_deref()
            {
                sess.reduced
                    .lock()
                    .unwrap()
                    .accepted_delivery_ids
                    .remove(delivery_id);
            }
            result
        }
        AcpUserAction::FollowUp {
            text,
            images,
            delivery_id,
        } => send_acp_follow_up(sess, text, images, delivery_id, subscribers),
        AcpUserAction::Compact => send_acp_compact(sess),
        AcpUserAction::ClearQueue => send_acp_clear_queue(sess),
        AcpUserAction::RewindToMessage { entry_index } => send_acp_rewind(sess, entry_index),
        AcpUserAction::Cancel => {
            let result = {
                let handle = sess.handle.lock().unwrap();
                let Some(h) = handle.as_ref() else {
                    return Err("ACP session is not running");
                };
                h.cmd_tx
                    .try_send(ConversationCommand::Cancel)
                    .map_err(|_| "ACP command channel is busy or closed")
            };
            if result.is_ok() {
                smelt_core::acp_session::note_cancel_requested(&mut sess.reduced.lock().unwrap());
                push_acp_snapshot(sess, false);
                update_acp_daemon_state(sess, subscribers);
            }
            result
        }
        AcpUserAction::SetConfigOption {
            config_id,
            value_id,
            boolean,
        } => {
            let handle = sess.handle.lock().unwrap();
            let Some(h) = handle.as_ref() else {
                return Err("ACP session is not running");
            };
            // Config changes can be queued next to the initial prompt; keep the
            // in-flight value as a count so both ACP requests reach the driver.
            h.in_flight_rpc.fetch_add(1, Ordering::SeqCst);
            let value = match boolean {
                Some(flag) => smelt_core::acp_conn::ConfigValue::Boolean(flag),
                None => smelt_core::acp_conn::ConfigValue::Select(value_id),
            };
            let result = h
                .cmd_tx
                .try_send(ConversationCommand::SetConfigOption { config_id, value })
                .map_err(|_| "ACP command channel is busy or closed");
            if result.is_err() {
                h.in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
            }
            result
        }
        AcpUserAction::SetSessionTitle { title } => {
            let handle = sess.handle.lock().unwrap();
            let Some(h) = handle.as_ref() else {
                return Err("ACP session is not running");
            };
            h.cmd_tx
                .try_send(ConversationCommand::SetSessionTitle(title))
                .map_err(|_| "ACP command channel is busy or closed")
        }
        AcpUserAction::PermissionSelect {
            tool_call_id,
            option_id,
        } => {
            let mut reduced = sess.reduced.lock().unwrap();
            let exists = reduced.permissions.iter().any(|card| {
                card.tool_call_id == tool_call_id
                    && card
                        .options
                        .iter()
                        .any(|option| option.option_id == option_id)
            });
            if !exists {
                return Err("permission request or option not found");
            }
            smelt_core::acp_session::select_permission(&mut reduced, &tool_call_id, &option_id);
            drop(reduced);
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationChoose { field_ix, opt_ix } => {
            let auto_submit = {
                let mut reduced = sess.reduced.lock().unwrap();
                smelt_core::acp_session::choose_elicitation(&mut reduced, field_ix, opt_ix)
            };
            if auto_submit {
                return submit_acp_elicitation(sess, subscribers);
            }
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationText { field_ix, value } => {
            smelt_core::acp_session::set_elicitation_text(
                &mut sess.reduced.lock().unwrap(),
                field_ix,
                value,
            );
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationSubmit => submit_acp_elicitation(sess, subscribers),
        AcpUserAction::ElicitationDismiss => {
            smelt_core::acp_session::dismiss_elicitation(&mut sess.reduced.lock().unwrap());
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
    }
}

pub(crate) fn submit_acp_elicitation(
    sess: &AcpSession,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let recovered_answer = {
        let reduced = sess.reduced.lock().unwrap();
        smelt_core::acp_session::recovered_elicitation_answer(&reduced)
    };
    if let Some(text) = recovered_answer {
        smelt_core::acp_session::dismiss_elicitation(&mut sess.reduced.lock().unwrap());
        return apply_acp_user_action_inner(
            sess,
            smelt_core::acp_session::AcpUserAction::Prompt {
                text,
                images: Vec::new(),
                delivery_id: None,
            },
            subscribers,
            true,
        );
    }

    smelt_core::acp_session::submit_elicitation(&mut sess.reduced.lock().unwrap());
    push_acp_snapshot(sess, false);
    update_acp_daemon_state(sess, subscribers);
    Ok(())
}

/// 用户输入与 Task/Peer/Automation delivery 的分界。Direct 绑定进入 ACP；插件
/// 绑定只调用 contribution；自动化 Run 拒绝额外 composer 输入。插件失败原样返回，
/// 绝不补发本地 ACP。
pub(crate) fn dispatch_conversation_input_with<F>(
    sess: &AcpSession,
    mut input: smelt_core::conversation::ConversationInput,
    subscribers: &EventHubHandle,
    invoke_plugin: F,
) -> Result<
    smelt_core::conversation::ConversationInputRoute,
    smelt_core::conversation::ConversationSubmitError,
>
where
    F: FnOnce(
        &smelt_plugin_api::PluginId,
        &smelt_plugin_api::PluginInputRouteBinding,
        &smelt_core::conversation::ConversationInput,
        Option<&str>,
        Option<&smelt_plugin_api::AgentSessionBinding>,
    ) -> Result<(), smelt_core::conversation::ConversationSubmitError>,
{
    use smelt_core::conversation::{ConversationBinding, ConversationInputRoute};

    let _submit = sess.conversation_submit.lock().unwrap();
    let pending_preset = sess.pending_agent_preset.lock().unwrap().clone();
    let binding = sess.conversation_binding.lock().unwrap().clone();
    let agent_session = sess.agent_session.lock().unwrap().clone();
    if agent_session.is_some() && !matches!(binding, Some(ConversationBinding::Plugin { .. })) {
        return Err(smelt_core::conversation::ConversationSubmitError::rejected(
            "agent session has no plugin input route",
        ));
    }
    if let (Some(ConversationBinding::Plugin { plugin_id, .. }), Some(agent_session)) =
        (&binding, &agent_session)
        && (&agent_session.agent.plugin_id != plugin_id
            || &agent_session.controller.plugin_id != plugin_id
            || &agent_session.instance.plugin_id != plugin_id)
    {
        return Err(smelt_core::conversation::ConversationSubmitError::rejected(
            "agent session and input route belong to different plugins",
        ));
    }
    let binding = binding.unwrap_or_default();
    let route = match binding {
        ConversationBinding::Direct => {
            input.text = smelt_core::conversation::merge_agent_preset(
                pending_preset.as_deref(),
                &input.text,
            );
            if !sess.reduced.lock().unwrap().supports_image && !input.images.is_empty() {
                let note = format!(
                    "[该任务附带 {} 张图片，但当前智能体不支持图片输入，图片未转发。]",
                    input.images.len()
                );
                input.text = if input.text.trim().is_empty() {
                    note
                } else {
                    format!("{}\n\n{note}", input.text)
                };
                input.images.clear();
            }
            apply_acp_user_action(
                sess,
                smelt_core::acp_session::AcpUserAction::Prompt {
                    text: input.text,
                    images: input.images,
                    delivery_id: None,
                },
                subscribers,
            )
            .map_err(smelt_core::conversation::ConversationSubmitError::rejected)?;
            ConversationInputRoute::Direct
        }
        ConversationBinding::Automation { .. } => {
            return Err(smelt_core::conversation::ConversationSubmitError::rejected(
                "automation Run input is daemon-owned",
            ));
        }
        ConversationBinding::Plugin { plugin_id, route } => {
            invoke_plugin(
                &plugin_id,
                &route,
                &input,
                pending_preset.as_deref(),
                agent_session.as_ref(),
            )?;
            ConversationInputRoute::Plugin
        }
    };
    if pending_preset.is_some() {
        *sess.pending_agent_preset.lock().unwrap() = None;
        // 插件路由不会产生 ACP 事件；显式发布消费状态，确保所有客户端同步，
        // 并让 workspace 在进程退出前清掉已使用的预设。
        push_acp_snapshot_since(sess, true, None);
    }
    Ok(route)
}

fn invoke_plugin_input_route(
    plugin_id: &smelt_plugin_api::PluginId,
    route: &smelt_plugin_api::PluginInputRouteBinding,
    input: &smelt_core::conversation::ConversationInput,
    agent_preset: Option<&str>,
    agent_session: Option<&smelt_plugin_api::AgentSessionBinding>,
) -> Result<(), smelt_core::conversation::ConversationSubmitError> {
    use smelt_core::conversation::ConversationSubmitError;
    use smelt_plugin_api::{
        ConversationInputImage, InputRouteInvocationPayload, InvocationId, InvocationOperation,
        InvocationRequest, InvocationResponse,
    };

    let binding = smelt_core::conversation::ConversationBinding::Plugin {
        plugin_id: plugin_id.clone(),
        route: route.clone(),
    };
    let contribution_sets = crate::plugin_runtime::contributions();
    validate_conversation_state(Some(&binding), agent_session, &contribution_sets)
        .map_err(ConversationSubmitError::rejected)?;

    let contribution = contribution_sets
        .into_iter()
        .find(|set| &set.plugin_id == plugin_id)
        .and_then(|set| {
            set.contributions.into_iter().find(|contribution| {
                contribution.id() == &route.contribution_id
                    && matches!(
                        contribution,
                        smelt_plugin_api::Contribution::InputRoute { .. }
                    )
            })
        })
        .ok_or_else(|| {
            ConversationSubmitError::rejected("conversation input route is unavailable")
        })?;
    let smelt_plugin_api::Contribution::InputRoute { operation, .. } = contribution else {
        unreachable!("filtered above")
    };
    let invocation_id = InvocationId::new(format!("input-{}", uuid::Uuid::new_v4().simple()))
        .map_err(|error| ConversationSubmitError::rejected(error.to_string()))?;
    let request = InvocationRequest {
        invocation_id,
        contribution_id: route.contribution_id.clone(),
        operation: InvocationOperation::new(operation.as_str())
            .map_err(|error| ConversationSubmitError::rejected(error.to_string()))?,
        payload: serde_json::to_value(InputRouteInvocationPayload {
            submission_id: input.submission_id.clone(),
            context: route.context.clone(),
            agent_preset: agent_preset.map(String::from),
            text: input.text.clone(),
            images: input
                .images
                .iter()
                .map(|image| ConversationInputImage {
                    mime: image.mime.clone(),
                    data_base64: image.data_b64.clone(),
                })
                .collect(),
        })
        .map_err(|error| ConversationSubmitError::rejected(error.to_string()))?,
        deadline_ms: crate::event_hub::now_ms().saturating_add(30_000),
    };
    match crate::plugin_runtime::invoke(plugin_id.clone(), request, Duration::from_secs(30))
        .map_err(ConversationSubmitError::unknown)?
    {
        InvocationResponse::Success { .. } => Ok(()),
        InvocationResponse::Error { code, message, .. } => Err(match code {
            smelt_plugin_api::InvocationErrorCode::InvalidRequest
            | smelt_plugin_api::InvocationErrorCode::Rejected
            | smelt_plugin_api::InvocationErrorCode::Conflict => {
                ConversationSubmitError::rejected(message)
            }
            smelt_plugin_api::InvocationErrorCode::Internal => {
                ConversationSubmitError::unknown(message)
            }
        }),
    }
}

pub(crate) fn conversation_submit_response(
    result: Result<
        smelt_core::conversation::ConversationInputRoute,
        smelt_core::conversation::ConversationSubmitError,
    >,
) -> serde_json::Value {
    match result {
        Ok(route) => serde_json::json!({"ok": true, "route": route}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    }
}

fn submit_config_values(v: &serde_json::Value) -> Vec<(String, String)> {
    v.get("config_values")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn apply_submit_config_values(
    sess: &AcpSession,
    config_values: &[(String, String)],
    subscribers: &EventHubHandle,
) -> Result<(), smelt_core::conversation::ConversationSubmitError> {
    if config_values.is_empty() || sess.handle.lock().unwrap().is_none() {
        return Ok(());
    }
    for (config_id, value_id) in config_values {
        if config_id.trim().is_empty() || value_id.trim().is_empty() {
            continue;
        }
        let boolean = {
            let reduced = sess.reduced.lock().unwrap();
            reduced
                .config_options
                .iter()
                .find(|config| config.config_id == *config_id)
                .and_then(|config| config.boolean.map(|_| value_id == "true"))
        };
        apply_acp_user_action(
            sess,
            smelt_core::acp_session::AcpUserAction::SetConfigOption {
                config_id: config_id.clone(),
                value_id: value_id.clone(),
                boolean,
            },
            subscribers,
        )
        .map_err(smelt_core::conversation::ConversationSubmitError::rejected)?;
    }
    Ok(())
}

pub(crate) fn handle_acp_submit_input(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let config_values = submit_config_values(v);
    let input = v
        .get("input")
        .cloned()
        .ok_or_else(|| {
            smelt_core::conversation::ConversationSubmitError::rejected(
                "missing conversation input",
            )
        })
        .and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                smelt_core::conversation::ConversationSubmitError::rejected(error.to_string())
            })
        });
    let result = input.and_then(|mut input: smelt_core::conversation::ConversationInput| {
        input.ensure_submission_id();
        if input.text.trim().is_empty() && input.images.is_empty() {
            return Err(smelt_core::conversation::ConversationSubmitError::rejected(
                "conversation input is empty",
            ));
        }
        let slot = acp_sessions.get(id).ok_or_else(|| {
            smelt_core::conversation::ConversationSubmitError::rejected("ACP session not found")
        })?;
        acp_sessions
            .with_current(id, &slot, |session| {
                apply_submit_config_values(session, &config_values, subscribers)?;
                dispatch_conversation_input_with(
                    session,
                    input,
                    subscribers,
                    invoke_plugin_input_route,
                )
            })
            .ok_or_else(|| {
                smelt_core::conversation::ConversationSubmitError::rejected(
                    "ACP session was replaced",
                )
            })?
    });
    let response = conversation_submit_response(result);
    let _ = writeln!(conn, "{response}");
}

/// Apply one protocol action without attaching a control client. Prompt here always means
/// direct ACP delivery; interactive callers must use `acp_submit_input` so binding is honored.
pub(crate) fn handle_acp_action(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();

    let action_value = match v.get("action").cloned() {
        Some(v) => v,
        None => {
            let _ = writeln!(conn, r#"{{"ok":false,"error":"missing ACP action"}}"#);
            return;
        }
    };
    let action: smelt_core::acp_session::AcpUserAction = match serde_json::from_value(action_value)
    {
        Ok(a) => a,
        Err(_) => {
            let _ = writeln!(conn, r#"{{"ok":false,"error":"invalid ACP action"}}"#);
            return;
        }
    };

    let slot = match acp_sessions.get(id) {
        Some(s) => s,
        None => {
            let _ = writeln!(conn, r#"{{"ok":false,"error":"ACP session not found"}}"#);
            return;
        }
    };

    let managed_binding = acp_sessions
        .with_current(id, &slot, |session| {
            match &*session.conversation_binding.lock().unwrap() {
                Some(smelt_core::conversation::ConversationBinding::Plugin { .. })
                    if matches!(
                        &action,
                        smelt_core::acp_session::AcpUserAction::Prompt { .. }
                            | smelt_core::acp_session::AcpUserAction::FollowUp { .. }
                    ) =>
                {
                    Some("interactive prompt must use acp_submit_input")
                }
                Some(smelt_core::conversation::ConversationBinding::Automation { .. })
                    if matches!(
                        &action,
                        smelt_core::acp_session::AcpUserAction::Prompt { .. }
                            | smelt_core::acp_session::AcpUserAction::FollowUp { .. }
                    ) =>
                {
                    Some("automation Run input is daemon-owned")
                }
                Some(smelt_core::conversation::ConversationBinding::Automation { .. })
                    if matches!(
                        &action,
                        smelt_core::acp_session::AcpUserAction::Cancel
                            | smelt_core::acp_session::AcpUserAction::Compact
                            | smelt_core::acp_session::AcpUserAction::ClearQueue
                            | smelt_core::acp_session::AcpUserAction::RewindToMessage { .. }
                            | smelt_core::acp_session::AcpUserAction::SetConfigOption { .. }
                            | smelt_core::acp_session::AcpUserAction::RestartRuntime
                    ) =>
                {
                    Some("automation Run action is daemon-owned")
                }
                _ => None,
            }
        })
        .flatten();
    if let Some(error) = managed_binding {
        let _ = writeln!(
            conn,
            "{}",
            serde_json::json!({
                "ok": false,
                "error": error
            })
        );
        return;
    }

    let result = acp_sessions
        .with_current(id, &slot, |session| {
            apply_acp_user_action(session, action, subscribers)
        })
        .unwrap_or(Err("ACP session was replaced"));

    let response = match result {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    };
    let _ = writeln!(conn, "{response}");
}

pub(crate) fn handle_acp_open(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    subscribers: EventHubHandle,
) {
    let Some(req) = parse_acp_open_request(v) else {
        write_rejected_acp_snapshot(conn, "无效的 ACP 打开请求");
        return;
    };
    let id = req.id.clone();
    let mut slot = match ensure_acp_session(&req, &sessions, &acp_sessions, &subscribers) {
        Ok(slot) => slot,
        Err(error) => {
            write_rejected_acp_snapshot(conn, &error);
            return;
        }
    };
    let attached_fd = loop {
        let lifecycle = slot.lifecycle.lock().unwrap();
        let still_current = acp_sessions
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(&current, &slot));
        if !still_current {
            drop(lifecycle);
            slot = match ensure_acp_session(&req, &sessions, &acp_sessions, &subscribers) {
                Ok(current) => current,
                Err(error) => {
                    write_rejected_acp_snapshot(conn, &error);
                    return;
                }
            };
            continue;
        }

        let sess = &slot.value;
        let attached_fd = {
            let _output_gate = sess.output_gate.lock().unwrap();
            let Ok(c) = conn.try_clone() else { return };
            let reduced = sess.reduced.lock().unwrap();
            let offset = req
                .tail_limit
                .map(|limit| reduced.entries.len().saturating_sub(limit))
                .unwrap_or(0);
            let mut snapshot = reduced.to_snapshot_since(false, offset);
            snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
            drop(reduced);
            set_conversation_snapshot(sess, &mut snapshot);
            let provider_pid = sess
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|handle| handle.stdio.lock().unwrap().map(|stdio| stdio.pid));
            let mut initial = serde_json::json!({
                "snapshot": snapshot,
                "provider_pid": provider_pid,
            })
            .to_string()
            .into_bytes();
            initial.push(b'\n');
            let Ok(attachment) = OutputAttachment::new(c, initial, &id, "acp-client") else {
                return;
            };
            let fd = attachment.fd;
            let mut out = sess.out.lock().unwrap();
            if let Some(old) = out.client.take() {
                old.close(); // 顶掉旧连接（同 id 只允许一个控制连接）
            }
            out.client = Some(attachment);
            fd
        };
        drop(lifecycle);
        break attached_fd;
    };
    let sess = &slot.value;

    // 动作循环：一行一个 AcpUserAction 的 JSON，直到客户端断开。
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(action) = serde_json::from_str(line.trim()) else {
            continue; // 认不出的行跳过，别让一条坏数据掐断整条连接
        };
        if matches!(
            &action,
            smelt_core::acp_session::AcpUserAction::RestartRuntime
        ) {
            let _ = restart_acp_session(&id, &acp_sessions, &subscribers);
            continue;
        }
        if acp_sessions
            .with_current(&id, &slot, |session| {
                let _ = apply_acp_user_action(session, action, &subscribers);
            })
            .is_none()
        {
            break;
        }
    }

    let _output_gate = sess.output_gate.lock().unwrap();
    let mut out = sess.out.lock().unwrap();
    if out.client.as_ref().is_some_and(|c| c.fd == attached_fd)
        && let Some(client) = out.client.take()
    {
        client.close();
    }
}

fn write_rejected_acp_snapshot(mut conn: UnixStream, reason: &str) {
    let mut state = smelt_core::acp_session::AcpSessionState::default();
    state.phase = smelt_core::daemon_state::DaemonPhase::Dead;
    state.end_reason = reason.to_string();
    state.end_kind = smelt_core::acp_session::AcpEndKind::ProviderFailed;
    let snapshot = state.to_snapshot(true);
    let _ = writeln!(conn, "{}", serde_json::json!({ "snapshot": snapshot }));
}

/// Ensure an ACP runtime exists without attaching a controlling client. Both desktop
/// `acp_open` and mobile `acp_create` use this path, so launch/resume semantics cannot drift.
pub(crate) fn ensure_acp_session(
    req: &AcpOpenRequest,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<Arc<AcpSlot<AcpSession>>, String> {
    if let Err(error) = validate_conversation_state(
        req.conversation_binding.as_ref(),
        req.agent_session.as_ref(),
        &contribution_sets_for_conversation(
            req.conversation_binding.as_ref(),
            req.agent_session.as_ref(),
        ),
    ) {
        eprintln!(
            "[acp] 拒绝会话 {} 的无效 conversation state：{error}",
            req.id
        );
        return Err(error);
    }
    loop {
        let (slot, created) = acp_sessions.reserve_with(&req.id, || {
            make_acp_session(
                &req.id,
                req.cwd.clone(),
                req.agent_needs_transcript_check,
                req.conversation_binding.clone(),
                req.agent_session.clone(),
                req.pending_agent_preset.clone(),
            )
        });
        if !created {
            adopt_unknown_conversation_binding(&slot.value, req.conversation_binding.clone());
            adopt_unknown_agent_session(&slot.value, req.agent_session.clone());
        }
        if let Some(error) =
            runtime_id_taken_by_other_kind(&req.id, sessions, acp_sessions, RemoteSessionKind::Acp)
        {
            if created {
                acp_sessions.remove_if_same(&req.id, &slot);
            }
            eprintln!("[acp] 拒绝会话 {}：{error}", req.id);
            return Err(error);
        }
        let lifecycle = slot.lifecycle.lock().unwrap();
        let still_current = acp_sessions
            .get(&req.id)
            .is_some_and(|current| Arc::ptr_eq(&current, &slot));
        if !still_current {
            drop(lifecycle);
            continue;
        }
        let alive = acp_runtime_alive(&slot.value);
        let (needs_relaunch, known_history) = {
            let reduced = slot.value.reduced.lock().unwrap();
            let current_launch = slot.value.launch_spec.lock().unwrap().clone();
            let current_ephemeral_env = slot.value.ephemeral_env.lock().unwrap().clone();
            let current_fingerprint = slot.value.runtime_spec_fingerprint.lock().unwrap().clone();
            let requested_fingerprint =
                acp_runtime_spec_fingerprint(&req.launch, &req.ephemeral_env);
            let runtime_spec_changed = if alive {
                current_fingerprint.map_or_else(
                    || {
                        acp_runtime_needs_relaunch(
                            alive,
                            current_launch.as_ref(),
                            &current_ephemeral_env,
                            &req.launch,
                            &req.ephemeral_env,
                        )
                    },
                    |current| current != requested_fingerprint,
                )
            } else {
                false
            };
            let needs_relaunch = acp_open_needs_relaunch(
                created,
                alive,
                !req.launch.command.is_empty(),
                req.resume_id.as_deref(),
                &reduced,
            ) || runtime_spec_changed;
            (needs_relaunch, known_acp_resume_id(&reduced))
        };
        let resume_id = select_resume_id(req.resume_id.clone(), known_history);
        if needs_relaunch {
            // provider session 独占：同一 resume_id 同一时刻只允许一个 ACP 会话
            // 持有。占用者还活着（即使正在重连）就拒绝后来的会话做 session/load，
            // 防止两个 agent 子进程并发操作同一个 AI 会话上下文。
            let resume_ids = resume_id.iter().cloned().collect::<Vec<_>>();
            if let Err(error) = acp_sessions.try_acquire_resumes(&resume_ids, &req.id) {
                eprintln!("[acp] 会话 {} 拒绝 relaunch：{error}", req.id);
                if created {
                    acp_sessions.remove_if_same(&req.id, &slot);
                }
                drop(lifecycle);
                return Err(error.to_string());
            }
            if (slot.value.unreaped_pid.lock().unwrap().is_some() || (alive && !created))
                && !retire_acp_runtime(&slot.value)
            {
                eprintln!(
                    "[acp] 会话 {} relaunch 前未能证明旧 provider 已退出，拒绝再 spawn",
                    req.id
                );
                drop(lifecycle);
                return Err(format!(
                    "会话 {} relaunch 前未能证明旧 provider 已退出",
                    req.id
                ));
            }
            // 到这里旧进程已经停止；现在才释放不属于本次 relaunch 的旧别名，
            // 整个切换期间始终至少有一个 owner，不给并发 load 留空窗。
            acp_sessions.retain_resumes_for(&req.id, &resume_ids);
            // 只在真正启动新进程前写入；热 attach 不能改变一个已运行子进程的
            // 环境，也不该把下一次任务的凭据混进现有会话。
            *slot.value.ephemeral_env.lock().unwrap() = req.ephemeral_env.clone();
            acp_relaunch(
                &slot,
                &req.id,
                req.launch.clone(),
                resume_id,
                req.fork_id.clone(),
                req.fork_cut.clone(),
                Arc::clone(acp_sessions),
                subscribers,
            );
        }
        drop(lifecycle);
        return Ok(slot);
    }
}

pub(crate) fn handle_acp_create(
    mut conn: UnixStream,
    v: &serde_json::Value,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    remote_sessions: &RemoteSessions,
) {
    let result = (|| -> Result<(), String> {
        let request =
            parse_acp_open_request(v).ok_or_else(|| "invalid ACP create request".to_string())?;
        let remote_session = match v.get("remote_session") {
            Some(value) => Some(
                serde_json::from_value::<RemoteAcpSession>(value.clone())
                    .map_err(|error| format!("invalid remote ACP session: {error}"))?,
            ),
            None => None,
        };
        create_daemon_acp_session(
            &request,
            remote_session,
            sessions,
            acp_sessions,
            subscribers,
            remote_sessions,
        )?;
        Ok(())
    })();
    let response = match result {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    };
    let _ = writeln!(conn, "{response}");
}

/// 创建一个无需 GUI 控制连接也能存活的 ACP 会话。移动端 `acp_create` 与 daemon
/// Task Runtime 共用，避免两条启动路径在 provider session 独占和远程目录事务上漂移。
pub(crate) fn create_daemon_acp_session(
    request: &AcpOpenRequest,
    mut remote_session: Option<RemoteAcpSession>,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    remote_sessions: &RemoteSessions,
) -> Result<Arc<AcpSlot<AcpSession>>, String> {
    if let Some(session) = &remote_session
        && session.id != request.id
    {
        return Err("remote ACP session id mismatch".to_string());
    }
    if let Some(session) = &mut remote_session {
        session.lifecycle = RemoteSessionLifecycle::Creating;
        let (_, snapshot) = mutate_remote_catalog(remote_sessions, |catalog| {
            catalog.upsert_acp(session.clone())
        })?;
        broadcast_remote_sessions(subscribers, &snapshot);
    }
    let slot = match ensure_acp_session(request, sessions, acp_sessions, subscribers) {
        Ok(slot) => slot,
        Err(error) => {
            if remote_session.is_some() {
                let _ = remove_remote_session_if_unbound(
                    remote_sessions,
                    subscribers,
                    RemoteSessionKind::Acp,
                    &request.id,
                );
            }
            return Err(error);
        }
    };
    if remote_session.is_some() {
        let activation = acp_sessions.with_current(&request.id, &slot, |session| {
            activate_remote_session_instance(
                remote_sessions,
                RemoteSessionKind::Acp,
                &request.id,
                session.instance,
            )
        });
        let snapshot = match activation {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(error)) => {
                let _ = acp_sessions.with_current(&request.id, &slot, |session| {
                    let _ = retire_acp_runtime(session);
                    let _ = remove_remote_session_if_unbound(
                        remote_sessions,
                        subscribers,
                        RemoteSessionKind::Acp,
                        &request.id,
                    );
                    acp_sessions.remove_if_same(&request.id, &slot);
                    forget_session(subscribers, &request.id, session.instance);
                });
                return Err(error);
            }
            None => {
                return Err("ACP runtime was replaced before catalog activation".to_string());
            }
        };
        broadcast_remote_sessions(subscribers, &snapshot);
    }
    Ok(slot)
}

/// 只读旁观：会话必须已存在（没有 ACP 版本的「不存在就兜底 spawn」——旁观一个
/// 没人开过的会话没有意义），不参与 client 顶替，可多个并存。
pub(crate) fn handle_acp_watch(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    acp_sessions: AcpSessions,
) {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let Some(slot) = acp_sessions.get(&id) else {
        return;
    };
    let lifecycle = slot.lifecycle.lock().unwrap();
    if !acp_sessions.is_current(&id, &slot) {
        return;
    }
    let sess = &slot.value;
    let attached_fd = {
        let _output_gate = sess.output_gate.lock().unwrap();
        let Ok(c) = conn.try_clone() else { return };
        let reduced = sess.reduced.lock().unwrap();
        let entries_total = reduced.entries.len();
        let current_history_id = reduced
            .history_session_id
            .as_deref()
            .or(reduced.acp_session_id.as_deref());
        let history_matches = v["history_session_id"]
            .as_str()
            .zip(current_history_id)
            .is_some_and(|(known, current)| known == current);
        let known_entries = v["known_entries"].as_u64().map(|n| n as usize);
        let tail_limit = v["tail_limit"].as_u64().map(|n| n as usize);
        let revision_matches =
            v["snapshot_revision"].as_u64() == Some(sess.snapshot_revision.load(Ordering::SeqCst));
        let offset = if history_matches && revision_matches {
            known_entries.unwrap_or(0).min(entries_total)
        } else if let Some(limit) = tail_limit {
            entries_total.saturating_sub(limit)
        } else {
            0
        };
        let mut snapshot = reduced.to_snapshot_since(false, offset);
        snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
        drop(reduced);
        set_conversation_snapshot(sess, &mut snapshot);
        let mut initial = serde_json::json!({ "snapshot": snapshot })
            .to_string()
            .into_bytes();
        initial.push(b'\n');
        let Ok(attachment) = OutputAttachment::new(c, initial, &id, "acp-watcher") else {
            return;
        };
        let fd = attachment.fd;
        sess.out.lock().unwrap().watchers.push(attachment);
        fd
    };
    drop(lifecycle);
    let mut scratch = [0u8; 64];
    let _ = reader.read(&mut scratch);
    let _output_gate = sess.output_gate.lock().unwrap();
    sess.out
        .lock()
        .unwrap()
        .watchers
        .retain(|w| w.fd != attached_fd);
}

/// One-shot bounded history read used by mobile upward pagination. Unlike `acp_watch`,
/// this does not register a watcher and therefore cannot leak a long-lived connection.
pub(crate) fn handle_acp_snapshot(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let Some(slot) = acp_sessions.get(id) else {
        return;
    };
    let before = v["before"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(usize::MAX);
    let limit = v["limit"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(100)
        .clamp(1, 500);
    let Some(snapshot) = acp_sessions.with_current(id, &slot, |session| {
        let _output_gate = session.output_gate.lock().unwrap();
        let reduced = session.reduced.lock().unwrap();
        let end = before.min(reduced.entries.len());
        let start = end.saturating_sub(limit);
        let mut snapshot = reduced.to_snapshot_range(false, start, end);
        snapshot.snapshot_revision = session.snapshot_revision.load(Ordering::SeqCst);
        drop(reduced);
        set_conversation_snapshot(session, &mut snapshot);
        snapshot
    }) else {
        return;
    };
    let _ = writeln!(conn, "{}", serde_json::json!({ "snapshot": snapshot }));
}

/// 杀会话：先在 lifecycle 锁内 retire（waitpid），再从表里摘掉。这是 Recreate：
/// 拆卸完成前 sid 仍在表里，并发 open 堵在同一把锁上，不会先 reserve 一个
/// 替换 slot 再用 sleep 去等。证不了退出就留在表里，让后续 open/restart 失败。
pub(crate) fn handle_acp_kill(
    conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    remote_sessions: &RemoteSessions,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let result = kill_acp_session(id, acp_sessions, remote_sessions, subscribers);
    let mut c = conn;
    let response = match result {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(c, "{response}");
}

pub(crate) fn kill_acp_session(
    id: &str,
    acp_sessions: &AcpSessions,
    remote_sessions: &RemoteSessions,
    subscribers: &EventHubHandle,
) -> Result<(), String> {
    let remote_owned =
        is_known_remote_session(remote_sessions, RemoteSessionKind::Acp, id) == Some(true);
    if remote_owned {
        match set_remote_lifecycle(
            remote_sessions,
            subscribers,
            RemoteSessionKind::Acp,
            id,
            RemoteSessionLifecycle::Closing,
        )? {
            true => {}
            false => return Err("remote session disappeared".to_string()),
        }
    }
    let slot = acp_sessions.get(id);
    let had_slot = slot.is_some();
    let mut removed_instance = None;
    let mut retire_failed = false;
    let mut error = if let Some(slot) = slot {
        let _lifecycle = slot.lifecycle.lock().unwrap();
        if !acp_sessions.is_current(id, &slot) {
            Some("ACP session changed during kill; retry".to_string())
        } else if !retire_acp_runtime(&slot.value) {
            eprintln!("[acp] kill 会话 {id} 时未能证明 provider 已退出，拒绝释放 sid");
            retire_failed = true;
            Some("ACP runtime 未能退出".to_string())
        } else {
            let sess = &slot.value;
            // 先把「会话已被终结」当成终态快照发出去，再断连。只断连的话客户端
            // 只能看到 EOF，会当成传输抖动用同一个 sid 重新 acp_open，把刚删掉的
            // 会话原地复活（手机删一条 PC 还开着的会话就是这个场景）。
            {
                let mut reduced = sess.reduced.lock().unwrap();
                smelt_core::acp_session::force_end(
                    &mut reduced,
                    smelt_core::acp_session::AcpEndKind::SessionTerminated,
                    "会话已被删除",
                );
            }
            // 会话马上就不存在了，这份终态不进归档（should_persist = false）。
            push_acp_snapshot_since(sess, false, None);
            let _output_gate = sess.output_gate.lock().unwrap();
            let mut out = sess.out.lock().unwrap();
            if let Some(c) = out.client.take() {
                c.close_after_flush();
            }
            for w in out.watchers.drain(..) {
                w.close_after_flush();
            }
            drop(out);
            let cleanup_error = if remote_owned {
                remove_remote_session_for_instance(
                    remote_sessions,
                    subscribers,
                    RemoteSessionKind::Acp,
                    id,
                    sess.instance,
                )
                .err()
            } else {
                None
            };
            acp_sessions.remove_if_same(id, &slot);
            removed_instance = Some(sess.instance);
            cleanup_error
        }
    } else {
        None
    };
    if error.is_none() && remote_owned && !had_slot {
        if let Err(remove_error) = remove_remote_session_if_unbound(
            remote_sessions,
            subscribers,
            RemoteSessionKind::Acp,
            id,
        ) {
            error = Some(remove_error);
        }
    } else if retire_failed && remote_owned {
        // 不能证明 provider 已退出时 runtime 仍在表内，目录也必须保持可见、可重试。
        // `Failed` 会被当前工作区投影过滤，用户反而失去再次 kill/restart 的入口。
        let _ = set_remote_lifecycle(
            remote_sessions,
            subscribers,
            RemoteSessionKind::Acp,
            id,
            RemoteSessionLifecycle::Active,
        );
    }
    if let Some(instance) = removed_instance {
        forget_session(subscribers, id, instance);
    } else if error.is_none() {
        forget_session(subscribers, id, 0);
    }
    error.map_or(Ok(()), Err)
}

/// 强制重启：agent 子进程失联/卡死（比如 `session/cancel` 打不断正在跑的工具
/// 调用）时的兜底——跟 `acp_kill` 共享"杀进程组"这一步，但**不**摘表、不断开
/// GUI 连接、不清 watchers：会话本体（entries/id/GUI 那条 acp_open 连接）
/// 全部原样留着，只是换一个新的子进程，带着 resume_session_id 去 `session/load`
/// 接回同一份历史，对用户来说是同一个标签、同一段对话，只是"服务端"心跳重启了。
pub(crate) fn handle_acp_restart(
    conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let result = restart_acp_session(id, acp_sessions, subscribers);
    let response = match result {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    };
    let mut c = conn;
    let _ = writeln!(c, "{}", response);
}

/// ACP runtime 的进程级重启原语。socket handler 和 daemon Task Runtime 共用，
/// 否则后台任务只能伪造一条本地状态，真正失联的 provider 永远不会被拉起。
pub(crate) fn restart_acp_session(
    id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let slot = acp_sessions.get(id).ok_or("ACP session not found")?;
    let _lifecycle = slot.lifecycle.lock().unwrap();
    if !acp_sessions.is_current(id, &slot) {
        return Err("ACP session was replaced");
    }
    let sess = &slot.value;
    if let Some(hosted) = sess.hosted_handle.lock().unwrap().as_ref() {
        return hosted
            .send_action(&smelt_core::acp_session::AcpUserAction::RestartRuntime)
            .map_err(|_| "ACP session host is unavailable");
    }
    let Some(launch) = sess.launch_spec.lock().unwrap().clone() else {
        return Err("no launch spec recorded for this session yet");
    };
    let resume_id = {
        let reduced = sess.reduced.lock().unwrap();
        known_acp_resume_id(&reduced)
    };
    // 续持自己的 provider session 占用（owner 是自身，正常情况下必然成功）；
    // 被其他会话抢占了才拒绝，避免与占用者并发 load 同一历史。
    let resume_ids = resume_id.iter().cloned().collect::<Vec<_>>();
    acp_sessions
        .try_acquire_resumes(&resume_ids, id)
        .map_err(|_| "provider session is held by another session")?;
    if !retire_acp_runtime(sess) {
        return Err("ACP runtime 未能退出");
    }
    acp_sessions.retain_resumes_for(id, &resume_ids);
    acp_relaunch(
        &slot,
        id,
        launch,
        resume_id,
        None,
        None,
        Arc::clone(acp_sessions),
        subscribers,
    );
    Ok(())
}

fn recorded_handoff_launch(sess: &AcpSession) -> smelt_core::agent_kind::ConversationLaunchSpec {
    sess.launch_spec.lock().unwrap().clone().unwrap_or_else(|| {
        smelt_core::agent_kind::ConversationLaunchSpec::from_command(
            sess.state
                .lock()
                .unwrap()
                .launch
                .clone()
                .unwrap_or_default(),
        )
    })
}

/// 交接 v2 typed 收集：与旧版同锁、同过滤、同 retire 语义，只换 typed 输出。
/// 旧 JSON 版已删：v2 写端不再产文件，legacy 文件读端只认旧二进制写的文件。
pub(crate) fn collect_acp_handoff_typed(
    acp_sessions: &AcpSessions,
) -> (
    Vec<crate::handoff_v2::manifest::AcpHandoff>,
    Vec<(crate::handoff_v2::manifest::FdRole, RawFd)>,
) {
    use crate::handoff_v2::manifest::{AcpHandoff, FdRole};
    let acp_session_list = acp_sessions.snapshot();
    let mut acp_items = Vec::new();
    let mut acp_fds = Vec::new();
    for (id, slot) in &acp_session_list {
        let sess = &slot.value;
        let hosted = sess
            .hosted_handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|host| {
                host.control_fd().map(|fd| {
                    (
                        fd,
                        host.pid(),
                        host.provider_pid(),
                        sess.host_snapshot_revision.load(Ordering::SeqCst),
                    )
                })
            });
        if let Some((host_fd, host_pid, provider_pid, host_snapshot_revision)) = hosted {
            let launch = recorded_handoff_launch(sess);
            let (agent_mcp, agent_token) = {
                let state = sess.state.lock().unwrap();
                (state.agent_mcp, state.agent_token.clone())
            };
            let mut snapshot = sess.reduced.lock().unwrap().to_snapshot(false);
            snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
            set_conversation_snapshot(sess, &mut snapshot);
            acp_items.push(AcpHandoff::Hosted {
                id: id.clone(),
                host_pid,
                provider_pid,
                host_snapshot_revision,
                cwd: sess.cwd.clone(),
                launch,
                agent_mcp,
                agent_token,
                agent_needs_transcript_check: sess.agent_needs_transcript_check,
                runtime_spec_fingerprint: sess.runtime_spec_fingerprint.lock().unwrap().clone(),
                conversation_binding: sess.conversation_binding.lock().unwrap().clone(),
                snapshot,
            });
            acp_fds.push((
                FdRole::AcpHost {
                    session_id: id.to_string(),
                },
                host_fd,
            ));
            continue;
        }
        let stdio = sess
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| *h.stdio.lock().unwrap());
        let Some(stdio) = stdio else { continue };
        let launch = recorded_handoff_launch(sess);
        let (agent_mcp, agent_token) = {
            let state = sess.state.lock().unwrap();
            (state.agent_mcp, state.agent_token.clone())
        };
        let (mut snapshot, pending_raw_line) = {
            let reduced = sess.reduced.lock().unwrap();
            (
                reduced.to_snapshot(false),
                reduced.pending_raw_request_line().map(str::to_string),
            )
        };
        set_conversation_snapshot(sess, &mut snapshot);
        acp_items.push(AcpHandoff::Direct {
            id: id.to_string(),
            pid: stdio.pid,
            cwd: sess.cwd.clone(),
            launch,
            agent_mcp,
            agent_token,
            agent_needs_transcript_check: sess.agent_needs_transcript_check,
            runtime_spec_fingerprint: sess.runtime_spec_fingerprint.lock().unwrap().clone(),
            conversation_binding: sess.conversation_binding.lock().unwrap().clone(),
            snapshot,
            pending_raw_line,
        });
        acp_fds.push((
            FdRole::AcpStdin {
                session_id: id.to_string(),
            },
            stdio.stdin_fd,
        ));
        acp_fds.push((
            FdRole::AcpStdout {
                session_id: id.to_string(),
            },
            stdio.stdout_fd,
        ));
    }

    let handed_off_ids: std::collections::HashSet<&str> =
        acp_items.iter().map(|item| item.id()).collect();
    for (id, slot) in &acp_session_list {
        if handed_off_ids.contains(id.as_str()) {
            continue;
        }
        let sess = &slot.value;
        // 这类会话通常还在 runtime 解析、尚无 fd 可交接。必须设置连接级停止
        // 标记；否则 exec 失败回滚并释放 spawn gate 后，旧线程会迟到 spawn。
        if !retire_acp_runtime(sess) {
            eprintln!("[acp] upgrade 淘汰未交接会话 {id} 时未能证明 provider 已退出");
        }
    }

    (acp_items, acp_fds)
}

pub(crate) fn acp_upgrade_blockers(acp_sessions: &AcpSessions) -> Vec<String> {
    use smelt_core::daemon_state::DaemonPhase;

    acp_sessions
        .snapshot()
        .into_iter()
        .filter_map(|(id, slot)| {
            // 独立 session host 持有 SDK future/responder/prompt queue，本 daemon
            // 只交接控制 socket；即使正处于 Running/审批中也没有内存状态要迁移。
            if slot.value.hosted_handle.lock().unwrap().is_some() {
                return None;
            }
            let (phase_blocks, has_unfinished_tool) = {
                let reduced = slot.value.reduced.lock().unwrap();
                // `phase` alone is not a reliable liveness signal.  A late event or an
                // old handoff can leave `Running` behind after the turn start timestamp
                // has already been cleared.  Such a session is safe to hand off; the
                // snapshot restore path will normalize any remaining dangling tools.
                let phase_blocks = match reduced.phase {
                    // Connecting 不是回合：握手卡住不能把升级永久拦住。
                    DaemonPhase::Connecting => false,
                    DaemonPhase::Thinking | DaemonPhase::ExecutingTool => {
                        reduced.turn_started_at_ms.is_some()
                    }
                    DaemonPhase::AwaitingApproval | DaemonPhase::WaitingForUser => {
                        reduced.turn_started_at_ms.is_some()
                            || !reduced.permissions.is_empty()
                            || reduced.elicitation.is_some()
                    }
                    DaemonPhase::Idle
                    | DaemonPhase::Dead
                    | DaemonPhase::Succeeded
                    | DaemonPhase::Failed => false,
                };
                let idle_without_active_turn = matches!(reduced.phase, DaemonPhase::Idle)
                    && reduced.turn_started_at_ms.is_none()
                    && reduced.permissions.is_empty()
                    && reduced.elicitation.is_none();
                let has_unfinished_tool = !reduced.replaying_history
                    && !idle_without_active_turn
                    && smelt_core::acp_chat::has_unfinished_tool_call(&reduced.entries);
                (phase_blocks, has_unfinished_tool)
            };
            let in_flight = slot
                .value
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|handle| handle.in_flight_rpc.load(Ordering::SeqCst) != 0);
            // `pending_prompts` 不在 ACP 快照中；只要 daemon 还握着回合闸门，
            // 交接就必须等它先派发/清空队列，不能把用户已经提交的输入丢掉。
            let prompt_queue_held = slot.value.prompt_in_flight.load(Ordering::SeqCst)
                || !slot.value.pending_prompts.lock().unwrap().is_empty();
            (phase_blocks || has_unfinished_tool || in_flight || prompt_queue_held).then_some(id)
        })
        .collect()
}
