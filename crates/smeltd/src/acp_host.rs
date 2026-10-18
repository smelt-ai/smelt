//! ACP 会话托管：open/watch/action/kill/restart，以及无缝升级所需的 ACP 快照。
//!
//! 从 `main.rs` 整块搬出。连接与归约仍在 `smelt_core::acp_session`；这里只跑守护侧的会话活体。

use super::*;
use smelt_core::acp_wire::FrameMirror;
use std::collections::{BTreeSet, VecDeque};

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
//       `{"snapshot": ConversationSnapshot}`，之后归约有实质变化再推同形状的行。
//       写线程还没把上一份写完时，后到的变化并进同一份，不另排一条字节队列。
//       同 id 只允许一个控制连接，第二次 open 顶掉前一个。
//   {"op":"acp_watch","id":".."} → 只读镜像，可多个并存。会话没在跑但属于桌面菜单时
//     先请桌面端唤醒；唤醒不了回一行 `{"unavailable":{"reason":..}}`。
//     `acp_watch` / `acp_snapshot` 可带 `"include_debug": false`：不要工具参数和
//     runtime debug sidecar。后者含每次模型调用的完整 provider payload（图片会在
//     每次调用里重复一份），手机这类只看正文的旁观者不该为它付带宽。
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
// callback、审批 responder 和 prompt 队列始终留在宿主进程，因此底层 handoff 具备
// 恢复活跃回合的能力；但正常升级仍在主 daemon 统一等待回合 idle，避免主动打断
// 用户对话。新 daemon 接管后发 Refresh 全量追平 exec 窗口。旧版 direct-fd handoff
// 仍保留兼容，接管后立即迁入独立宿主。

pub(crate) struct AcpOut {
    /// 控制连接和旁观连接各自一条快照写线程。不复用终端的 8MB 字节邮箱：
    /// 快照是状态，排队副本超限会把还活着的宿主判成断线。
    pub(crate) client: Option<AcpSnapshotLink>,
    pub(crate) watchers: Vec<AcpSnapshotLink>,
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
    /// 自动化只关心相位、投递账本和闸门。流式文本不进这组，避免每来一个字就整段重算。
    pub(crate) automation_watch: Mutex<Option<AutomationWatch>>,
    pub(crate) pending_prompts: Mutex<VecDeque<QueuedAcpPrompt>>,
    /// 一次「立即发送」还没落地。文本是稳定键；附图在认领时抄下，因为 Pi 的 clear_queue 还原不带图。
    pub(crate) immediate_delivery: Mutex<Option<ImmediateDelivery>>,
    /// 已经写给 Pi、还没被收下的那条。收下之前不回显；拒绝后回到队列，不记回合失败。
    pub(crate) unaccepted_prompt: Mutex<Option<QueuedAcpPrompt>>,
    /// 界面上的「思考中」只是为了接住还在跑的 Pi，并不是一条已被收下的回合。
    /// 下一次 `ProviderIdle` 要把它收回去，进门才能放行。
    pub(crate) provider_hold: AtomicBool,
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
    /// 串行化所有交互式输入。没有这把门时两个客户端会同时把同一个首轮预设各带一次。
    pub(crate) conversation_submit: Mutex<()>,
    /// 只装饰下一条成功路由的交互输入。Task/Peer delivery 不读取也不消费它。
    pub(crate) pending_agent_preset: Mutex<Option<String>>,
    /// 已经写进 `acp_detail.history_session_id` 的身份。
    /// 和内存里的确认值一样就不再写库；行还不在时保持空，下一帧再试。
    pub(crate) durable_history_session_id: Mutex<Option<String>>,
    /// 闲置挂起的账本，见 `acp_idle`。
    pub(crate) idle: crate::acp_idle::AcpIdleState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpPromptOrigin {
    User,
}

/// 原生排队「立即发送」的单槽。clear_queue 还回这一条才发；没还回来就不能再发。
#[derive(Clone)]
pub(crate) struct ImmediateDelivery {
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    /// 这次 clear 的还原还没到。到之前不能发，否则队列里那份会再被带走。
    awaiting_clear: bool,
    /// 还原里确实有这一条，provider 空闲后发一次。
    send: bool,
}

pub(crate) struct QueuedAcpPrompt {
    pub(crate) text: String,
    pub(crate) images: Vec<smelt_core::acp_conn::PromptImage>,
    pub(crate) delivery_id: Option<String>,
    pub(crate) origin: AcpPromptOrigin,
    /// 正文已经在消息流里了。Pi 拒收后重发的是同一条气泡，不能再写一遍。
    pub(crate) echoed: bool,
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
    pub(crate) resume_id: Option<String>,
    /// Pi `--fork` 源 session。新开会话，不得拿它去抢源会话的 resume 锁。
    pub(crate) fork_id: Option<String>,
    /// 分叉副本的切点（与 `fork_id` 搭配）：整拷打开后、重放前切到该用户消息
    /// 之前。见 `smelt_core::acp_conn::AcpForkCut`。
    pub(crate) fork_cut: Option<smelt_core::acp_conn::AcpForkCut>,
    /// `None` 表示客户端未指定。已有会话仅在 daemon 自己仍未知时采用该值。
    pub(crate) conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
    /// 仅在创建新的 daemon 会话时采用；热 attach 不得用客户端旧存档覆盖已消费值。
    pub(crate) pending_agent_preset: Option<String>,
    /// 窗口只要活状态。会话宿主和守护之间的镜像必须带历史，否则分页没有正文。
    pub(crate) omit_history: bool,
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
    Some(AcpOpenRequest {
        id,
        cwd: v["cwd"].as_str().map(String::from),
        launch,
        ephemeral_env: v
            .get("ephemeral_env")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        resume_id: normalize_resume_id(v["resume_id"].as_str().map(String::from)),
        fork_id: normalize_resume_id(v["fork_id"].as_str().map(String::from)),
        fork_cut: v
            .get("fork_cut")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        // 开会话不读 tail_limit。旧客户端仍会发；历史分页在另一处按页取。
        conversation_binding,
        pending_agent_preset: v["pending_agent_preset"]
            .as_str()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(String::from),
        omit_history: v["omit_history"].as_bool().unwrap_or(false),
    })
}

pub(crate) fn normalize_resume_id(resume_id: Option<String>) -> Option<String> {
    resume_id.and_then(|id| {
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_string())
    })
}

pub(crate) fn select_resume_id(
    known: Option<String>,
    stored: smelt_store::StoredHistoryIdentity,
    requested: Option<String>,
) -> Option<String> {
    // 正在跟 agent 说话的进程记得身份，库和窗口缓存都不能盖过它。
    if let Some(id) = normalize_resume_id(known) {
        return Some(id);
    }
    match stored {
        // 这个槽位已经有行。确认过的 id 胜过窗口缓存。
        smelt_store::StoredHistoryIdentity::Bound(id) => normalize_resume_id(Some(id)),
        // 行在、列是空：明确还没确认，或已经放弃。窗口里的旧缓存不能填回来。
        smelt_store::StoredHistoryIdentity::Unbound => None,
        // 还没有这一行。历史页「继续」带的 id 可以用来开一个新槽位。
        smelt_store::StoredHistoryIdentity::NoRow => normalize_resume_id(requested),
    }
}

fn durable_history_identity(session_id: &str) -> smelt_store::StoredHistoryIdentity {
    // 没开库，或这一下读失败，都当成还没有行。测试里的显式 resume 仍能用。
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return smelt_store::StoredHistoryIdentity::NoRow;
    };
    store
        .stored_history_identity(session_id)
        .unwrap_or(smelt_store::StoredHistoryIdentity::NoRow)
}

/// 给已有会话选恢复身份。`allow_durable` 为假时不读库（分叉是新会话）。
/// 明确放弃旧历史的 `fallback_to_fresh` 不走这里，它直接把 resume 传成 `None`。
pub(crate) fn resolve_resume_id(
    session_id: &str,
    requested: Option<String>,
    known_history: Option<String>,
    allow_durable: bool,
) -> Option<String> {
    let requested = normalize_resume_id(requested);
    let known_history = normalize_resume_id(known_history);
    if !allow_durable {
        return known_history.or(requested);
    }
    if let Some(id) = known_history {
        return Some(id);
    }
    select_resume_id(None, durable_history_identity(session_id), requested)
}

pub(crate) fn known_acp_resume_id(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Option<String> {
    normalize_resume_id(reduced.history_session_id.clone()).or_else(|| {
        (!reduced.entries().is_empty())
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
    let executing_tool = !reduced.replaying_history && has_unfinished_tool_call(reduced.entries());
    match reduced.phase {
        DaemonPhase::Connecting => Phase::Connecting,
        // `TurnEnded` 在 ACP 归约器里落成 Idle + completed_unread，并写上
        // `turn_outcome`。成功边沿只认 `Succeeded`。没有结果、取消都不是成功；
        // 带失败文案的结果才是失败。下一条 prompt 会清 completed_unread 并切回
        // Thinking。已确认的成功必须优先于悬空工具明细：adapter 常会先发
        // TurnEnded、后补 ToolFinished，后者不能把完成重新解释成“执行工具”。
        //
        // 没有 completed_unread 的 Idle 同样不能靠未收尾工具回到 ExecutingTool：
        // 那是回合被掐断后欠下的终态，不是当前活动。
        DaemonPhase::Idle if reduced.completed_unread => {
            if reduced
                .turn_outcome
                .is_some_and(smelt_core::acp_session::AcpTurnOutcome::is_success)
            {
                Phase::Succeeded
            } else if reduced
                .turn_outcome
                .is_some_and(|outcome| outcome.failure_message().is_some())
            {
                Phase::Failed
            } else {
                Phase::Idle
            }
        }
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

/// 主 daemon 怎样把宿主这一帧落到 `acp_detail.history_session_id`。
/// 有确认过的 id 就记住，哪怕同一帧还带着放弃标志（新身份胜出）。
/// 没有 id、也没有放弃，就不能动库：重新开始会先把内存清掉，崩溃后仍要回到旧对话。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DurableHistoryWrite {
    Remember(String),
    Clear,
    Leave,
}

pub(crate) fn durable_history_write(
    history_id: Option<String>,
    abandoned: bool,
) -> DurableHistoryWrite {
    match normalize_resume_id(history_id) {
        Some(id) => DurableHistoryWrite::Remember(id),
        None if abandoned => DurableHistoryWrite::Clear,
        None => DurableHistoryWrite::Leave,
    }
}

fn persist_confirmed_history_id(sess: &AcpSession) {
    // 父镜像不写这一列。宿主帧由 snapshot drain 在合并之后写入；
    // 这里若用合并前拷出来的旧 id 再写一次，会把刚刚放弃的身份填回去。
    // 会话宿主进程没有打开 SQLite，调用会直接返回。
    if sess.hosted_handle.lock().unwrap().is_some() {
        return;
    }
    let Some(history_id) = ({
        let reduced = sess.reduced.lock().unwrap();
        normalize_resume_id(reduced.history_session_id.clone())
    }) else {
        return;
    };
    write_durable_history_id(sess, &history_id);
}

fn write_durable_history_id(sess: &AcpSession, history_id: &str) {
    if sess.durable_history_session_id.lock().unwrap().as_deref() == Some(history_id) {
        return;
    }
    let session_id = sess.state.lock().unwrap().id.clone();
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return;
    };
    match store.remember_history_session_id(&session_id, history_id) {
        Ok(true) => {
            *sess.durable_history_session_id.lock().unwrap() = Some(history_id.to_string());
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!("[acp] 写入对话恢复身份失败 id={session_id}: {error}");
        }
    }
}

fn clear_durable_history_id(sess: &AcpSession) {
    *sess.durable_history_session_id.lock().unwrap() = None;
    let session_id = sess.state.lock().unwrap().id.clone();
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return;
    };
    if let Err(error) = store.clear_history_session_id(&session_id) {
        eprintln!("[acp] 放弃对话恢复身份失败 id={session_id}: {error}");
    }
}

/// 宿主快照已经合并进父镜像。只有这条路径可以改托管会话的恢复身份。
fn apply_durable_history_from_host(sess: &AcpSession, history_id: Option<String>, abandoned: bool) {
    match durable_history_write(history_id, abandoned) {
        DurableHistoryWrite::Remember(id) => write_durable_history_id(sess, &id),
        DurableHistoryWrite::Clear => clear_durable_history_id(sess),
        DurableHistoryWrite::Leave => {}
    }
}

/// 把归约状态里的相位/待办问句同步进四色 `SessionState` 并广播。跟旧版 GUI
/// `AcpView::sync_daemon_state` 是同一件事，只是现在算在 smeltd 侧。
pub(crate) fn update_acp_daemon_state(sess: &AcpSession, subscribers: &EventHubHandle) {
    persist_confirmed_history_id(sess);
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
        let phase_changed = commit_session_phase(&mut st, PhaseSource::AcpProjection, phase);
        let question_changed = st.pending_question != pending_question;
        let title_changed = st.title != title;
        if !phase_changed && !question_changed && !title_changed {
            None
        } else {
            if question_changed {
                st.pending_question = pending_question;
            }
            if title_changed {
                st.title = title;
            }
            st.updated_at = now;
            bump_state_revision(&mut st);
            Some(st.clone())
        }
    };
    if let Some(snapshot) = snapshot {
        broadcast_state(subscribers, &snapshot);
    }
    note_automation_watch(sess);
}

#[derive(Clone, PartialEq)]
pub(crate) struct AutomationWatch {
    phase: smelt_core::daemon_state::DaemonPhase,
    active_delivery_id: Option<String>,
    completed_delivery_id: Option<String>,
    accepted_delivery_ids: BTreeSet<String>,
    turn_outcome: Option<smelt_core::acp_session::AcpTurnOutcome>,
    end_reason: String,
    prompt_in_flight: bool,
}

fn note_automation_watch(sess: &AcpSession) {
    let reduced = sess.reduced.lock().unwrap();
    let prompt_in_flight = sess.prompt_in_flight.load(Ordering::SeqCst);
    let mut slot = sess.automation_watch.lock().unwrap();
    let unchanged = slot.as_ref().is_some_and(|current| {
        current.phase == reduced.phase
            && current.active_delivery_id == reduced.active_delivery_id
            && current.completed_delivery_id == reduced.completed_delivery_id
            && current.accepted_delivery_ids == reduced.accepted_delivery_ids
            && current.turn_outcome == reduced.turn_outcome
            && current.end_reason == reduced.end_reason
            && current.prompt_in_flight == prompt_in_flight
    });
    if unchanged {
        return;
    }
    *slot = Some(AutomationWatch {
        phase: reduced.phase,
        active_delivery_id: reduced.active_delivery_id.clone(),
        completed_delivery_id: reduced.completed_delivery_id.clone(),
        accepted_delivery_ids: reduced.accepted_delivery_ids.clone(),
        turn_outcome: reduced.turn_outcome,
        end_reason: reduced.end_reason.clone(),
        prompt_in_flight,
    });
    drop(slot);
    drop(reduced);
    crate::automation_runtime::nudge_automation_driver();
}

/// 推一份最新快照给控制连接 + 全部旁观者。这里只复制到各 attachment 的输出邮箱；
/// 写线程发现断线或邮箱超限后，下一次入队会摘掉该连接（读循环也会在 EOF 时清理）。
/// 一条 ACP 控制/旁观连接的快照出口。
///
/// 终端邮箱按字节排队，堆到 8MB 就摘连接。快照不是日志，中间态没有独立价值，
/// 所以这里只记「最早还没写出去的 offset」和两个标志。写线程空闲时序列化一次，
/// 从那个 offset 覆盖到当前末尾。GUI 卡住只堵住这条写线程，不会把归约线程或
/// 连接本身判死。
struct SnapshotDirty {
    from: usize,
    should_persist: bool,
    include_runtime_debug: bool,
    include_envelope: bool,
    /// 这一帧要带给窗口的完整调试体。父镜像里只留账本，写完这帧就丢掉。
    runtime_debug: Option<Arc<smelt_core::acp_session::RuntimeDebug>>,
}

struct SnapshotMailbox {
    dirty: Option<SnapshotDirty>,
    lines: VecDeque<Vec<u8>>,
}

struct SnapshotLinkInner {
    mailbox: Mutex<SnapshotMailbox>,
    cv: Condvar,
    /// 下一帧发整段帧并重建发送方镜像：对方要了完整快照（`Refresh`），
    /// 例如守护交接后接手的读端手里没有镜像。
    resync: AtomicBool,
    closed: AtomicBool,
    finishing: AtomicBool,
    failed: AtomicBool,
    flush_done: AtomicBool,
    snapshots_sent: AtomicU64,
    /// 这张连接已经写出去的工具参数代表。没变就不再把整表放进下一帧。
    tool_debug_generation_sent: AtomicU64,
}

pub(crate) struct AcpSnapshotLink {
    pub(crate) fd: RawFd,
    inner: Arc<SnapshotLinkInner>,
    shutdown: UnixStream,
}

const ACP_SNAPSHOT_FLUSH_DEADLINE: Duration = Duration::from_secs(3);
/// 流式 token 不值得一帧一帧序列化。写线程把非落盘、非调试的脏标记再并 16ms，
/// 权限、回合结束和 runtime debug 仍立刻写出。
const ACP_SNAPSHOT_COALESCE: Duration = Duration::from_millis(16);

fn snapshot_dirty_is_urgent(job: &SnapshotDirty) -> bool {
    job.should_persist || job.include_runtime_debug || job.include_envelope
}

impl AcpSnapshotLink {
    fn mark(
        &self,
        from: usize,
        should_persist: bool,
        include_runtime_debug: bool,
        include_envelope: bool,
        runtime_debug: Option<Arc<smelt_core::acp_session::RuntimeDebug>>,
    ) {
        let mut mailbox = self.inner.mailbox.lock().unwrap();
        match mailbox.dirty.as_mut() {
            Some(existing) => {
                existing.from = existing.from.min(from);
                existing.should_persist |= should_persist;
                existing.include_runtime_debug |= include_runtime_debug;
                existing.include_envelope |= include_envelope;
                if runtime_debug.is_some() {
                    existing.runtime_debug = runtime_debug;
                }
            }
            None => {
                mailbox.dirty = Some(SnapshotDirty {
                    from,
                    should_persist,
                    include_runtime_debug,
                    include_envelope,
                    runtime_debug,
                });
            }
        }
        drop(mailbox);
        self.inner.cv.notify_one();
    }

    /// 下一帧改发整段帧。调用方随后要记脏，写线程才会出帧。
    fn request_full_frame(&self) {
        self.inner.resync.store(true, Ordering::SeqCst);
    }

    /// 历史页回包和快照共用写线程，避免和快照字节抢同一条 socket。
    fn enqueue_line(&self, line: Vec<u8>) {
        let mut mailbox = self.inner.mailbox.lock().unwrap();
        mailbox.lines.push_back(line);
        drop(mailbox);
        self.inner.cv.notify_one();
    }

    pub(crate) fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.cv.notify_one();
        let _ = self.shutdown.shutdown(Shutdown::Both);
    }

    /// 先把已记脏的终态写出去再断开。客户端不读时最多等一小段，避免写线程挂死。
    pub(crate) fn close_after_flush(self) {
        self.inner.finishing.store(true, Ordering::SeqCst);
        self.inner.cv.notify_one();
        let deadline = Instant::now() + ACP_SNAPSHOT_FLUSH_DEADLINE;
        let mut dirty = self.inner.mailbox.lock().unwrap();
        while !self.inner.flush_done.load(Ordering::SeqCst) && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (guard, timeout) = self.inner.cv.wait_timeout(dirty, remaining).unwrap();
            dirty = guard;
            if timeout.timed_out() {
                break;
            }
        }
        drop(dirty);
        self.close();
    }

    #[cfg(test)]
    fn has_failed(&self) -> bool {
        self.inner.failed.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn snapshots_sent(&self) -> u64 {
        self.inner.snapshots_sent.load(Ordering::SeqCst)
    }
}

impl Drop for AcpSnapshotLink {
    fn drop(&mut self) {
        if self.inner.finishing.load(Ordering::SeqCst) {
            return;
        }
        self.close();
    }
}

pub(crate) struct EncodedAcpSnapshot {
    pub bytes: Vec<u8>,
    /// 这次序列化时看到的工具参数代数。只有本帧带上了整表，写成功后才记到连接上。
    pub tool_debug_generation: u64,
    pub included_tool_debug: bool,
}

type SnapshotEncode = Arc<
    dyn Fn(
            SnapshotFrameKind<'_>,
            usize,
            bool,
            bool,
            bool,
            u64,
            Option<Arc<smelt_core::acp_session::RuntimeDebug>>,
        ) -> Option<EncodedAcpSnapshot>
        + Send
        + Sync,
>;

/// 这一帧怎么编码条目。两种都会更新这条连接的发送方镜像。
pub(crate) enum SnapshotFrameKind<'a> {
    /// 只发相对镜像的变化。
    Delta(&'a mut FrameMirror),
    /// 整段帧，镜像按它重建。
    Full(&'a mut FrameMirror),
}

/// 整段帧：编成一行字节，同时建出发送方镜像，对方收到后手里就是这一帧。
pub(crate) fn full_frame(frame: &serde_json::Value) -> (Vec<u8>, FrameMirror) {
    let mut mirror = FrameMirror::default();
    mirror.full(frame);
    let mut bytes = frame.to_string().into_bytes();
    bytes.push(b'\n');
    (bytes, mirror)
}

type SnapshotDetach = Arc<dyn Fn(RawFd) + Send + Sync>;

pub(crate) fn spawn_acp_snapshot_link(
    stream: UnixStream,
    initial: Vec<u8>,
    mirror: FrameMirror,
    tool_debug_generation_sent: u64,
    id: &str,
    kind: &'static str,
    encode: SnapshotEncode,
    detach: SnapshotDetach,
) -> std::io::Result<AcpSnapshotLink> {
    let fd = stream.as_raw_fd();
    stream.set_write_timeout(None)?;
    let shutdown = stream.try_clone()?;
    let inner = Arc::new(SnapshotLinkInner {
        mailbox: Mutex::new(SnapshotMailbox {
            dirty: None,
            lines: VecDeque::new(),
        }),
        cv: Condvar::new(),
        resync: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        finishing: AtomicBool::new(false),
        failed: AtomicBool::new(false),
        flush_done: AtomicBool::new(false),
        snapshots_sent: AtomicU64::new(0),
        tool_debug_generation_sent: AtomicU64::new(tool_debug_generation_sent),
    });
    let writer_inner = Arc::clone(&inner);
    let writer_id = id.to_string();
    thread::Builder::new()
        .name("smelt-acp-snapshot".to_string())
        .spawn(move || {
            acp_snapshot_writer(SnapshotWriter {
                stream,
                initial,
                mirror,
                encode,
                detach,
                inner: writer_inner,
                id: writer_id,
                kind,
                fd,
            });
        })
        .map_err(std::io::Error::other)?;
    Ok(AcpSnapshotLink {
        fd,
        inner,
        shutdown,
    })
}

/// 这条快照连接要哪些部分。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SnapshotProjection {
    /// false 时只带进行中的回合，历史由连接方自己分页读。
    pub history: bool,
    /// 工具参数表和 runtime debug sidecar。只给桌面调试面板用。
    pub debug: bool,
}

#[cfg(test)]
impl SnapshotProjection {
    pub(crate) const FULL: Self = Self {
        history: true,
        debug: true,
    };
}

/// `initial` 是首帧（整段帧），`mirror` 和它一起建：见 [`full_frame`]。
#[allow(clippy::too_many_arguments)]
pub(crate) fn acp_snapshot_link_for_slot(
    slot: &Arc<AcpSlot<AcpSession>>,
    stream: UnixStream,
    initial: Vec<u8>,
    mirror: FrameMirror,
    tool_debug_generation_sent: u64,
    id: &str,
    kind: &'static str,
    projection: SnapshotProjection,
) -> std::io::Result<AcpSnapshotLink> {
    let encode_slot = Arc::downgrade(slot);
    let detach_slot = Arc::downgrade(slot);
    spawn_acp_snapshot_link(
        stream,
        initial,
        mirror,
        tool_debug_generation_sent,
        id,
        kind,
        Arc::new(
            move |frame_kind,
                  from,
                  should_persist,
                  include_envelope,
                  include_runtime_debug,
                  tool_debug_generation_sent,
                  runtime_debug| {
                let slot = encode_slot.upgrade()?;
                Some(encode_acp_snapshot(
                    &slot.value,
                    frame_kind,
                    from,
                    should_persist,
                    include_envelope,
                    include_runtime_debug,
                    tool_debug_generation_sent,
                    projection,
                    runtime_debug,
                ))
            },
        ),
        Arc::new(move |fd| {
            let Some(slot) = detach_slot.upgrade() else {
                return;
            };
            detach_failed_snapshot_link(&slot.value, fd);
        }),
    )
}

struct SnapshotWriter {
    stream: UnixStream,
    initial: Vec<u8>,
    /// 对方手里有哪些条目。只有这条写线程读写它。
    mirror: FrameMirror,
    encode: SnapshotEncode,
    detach: SnapshotDetach,
    inner: Arc<SnapshotLinkInner>,
    id: String,
    kind: &'static str,
    fd: RawFd,
}

fn acp_snapshot_writer(writer: SnapshotWriter) {
    let SnapshotWriter {
        mut stream,
        initial,
        mut mirror,
        encode,
        detach,
        inner,
        id,
        kind,
        fd,
    } = writer;
    if !initial.is_empty()
        && let Err(error) = stream.write_all(&initial)
    {
        note_snapshot_writer_stopped(&inner, &detach, &id, kind, fd, &error);
        return;
    }
    loop {
        let (lines, job) = {
            let mut mailbox = inner.mailbox.lock().unwrap();
            loop {
                if mailbox.lines.is_empty() && mailbox.dirty.is_none() {
                    if inner.closed.load(Ordering::SeqCst) || inner.finishing.load(Ordering::SeqCst)
                    {
                        inner.flush_done.store(true, Ordering::SeqCst);
                        inner.cv.notify_all();
                        let _ = stream.shutdown(Shutdown::Both);
                        return;
                    }
                    mailbox = inner.cv.wait(mailbox).unwrap();
                    continue;
                }
                break take_coalesced_snapshot(&inner, mailbox);
            }
        };
        if inner.closed.load(Ordering::SeqCst) && !inner.finishing.load(Ordering::SeqCst) {
            inner.flush_done.store(true, Ordering::SeqCst);
            inner.cv.notify_all();
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        for line in &lines {
            if let Err(error) = stream.write_all(line) {
                note_snapshot_writer_stopped(&inner, &detach, &id, kind, fd, &error);
                return;
            }
        }
        let Some(job) = job else {
            continue;
        };
        let sent_generation = inner.tool_debug_generation_sent.load(Ordering::SeqCst);
        let frame_kind = if inner.resync.swap(false, Ordering::SeqCst) {
            SnapshotFrameKind::Full(&mut mirror)
        } else {
            SnapshotFrameKind::Delta(&mut mirror)
        };
        let Some(encoded) = encode(
            frame_kind,
            job.from,
            job.should_persist,
            job.include_envelope,
            job.include_runtime_debug,
            sent_generation,
            job.runtime_debug,
        ) else {
            inner.flush_done.store(true, Ordering::SeqCst);
            inner.cv.notify_all();
            let _ = stream.shutdown(Shutdown::Both);
            return;
        };
        if let Err(error) = stream.write_all(&encoded.bytes) {
            note_snapshot_writer_stopped(&inner, &detach, &id, kind, fd, &error);
            return;
        }
        if encoded.included_tool_debug {
            inner
                .tool_debug_generation_sent
                .store(encoded.tool_debug_generation, Ordering::SeqCst);
        }
        inner.snapshots_sent.fetch_add(1, Ordering::SeqCst);
    }
}

/// 非紧急脏标记再等一个很短的窗口，让随后到达的 token 并进同一帧。
/// 等待期间若变成落盘/调试帧，或连接正在关闭，立刻取出。
fn take_coalesced_snapshot(
    inner: &SnapshotLinkInner,
    mut mailbox: std::sync::MutexGuard<'_, SnapshotMailbox>,
) -> (VecDeque<Vec<u8>>, Option<SnapshotDirty>) {
    let urgent = mailbox.dirty.as_ref().is_some_and(snapshot_dirty_is_urgent);
    if !urgent && mailbox.lines.is_empty() && mailbox.dirty.is_some() {
        let deadline = std::time::Instant::now() + ACP_SNAPSHOT_COALESCE;
        while std::time::Instant::now() < deadline {
            if mailbox.dirty.as_ref().is_some_and(snapshot_dirty_is_urgent)
                || inner.closed.load(Ordering::SeqCst)
                || inner.finishing.load(Ordering::SeqCst)
            {
                break;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (guard, result) = inner.cv.wait_timeout(mailbox, remaining).unwrap();
            mailbox = guard;
            if result.timed_out() {
                break;
            }
        }
    }
    let lines = std::mem::take(&mut mailbox.lines);
    let job = mailbox.dirty.take();
    drop(mailbox);
    (lines, job)
}

fn note_snapshot_writer_stopped(
    inner: &SnapshotLinkInner,
    detach: &SnapshotDetach,
    id: &str,
    kind: &'static str,
    fd: RawFd,
    error: &std::io::Error,
) {
    inner.failed.store(true, Ordering::SeqCst);
    inner.flush_done.store(true, Ordering::SeqCst);
    inner.cv.notify_all();
    if !inner.closed.load(Ordering::SeqCst) {
        crate::dlog(&format!(
            "{kind} snapshot writer stopped id={id} fd={fd} error={error}"
        ));
        detach(fd);
    }
}

pub(crate) fn detach_failed_snapshot_link(sess: &AcpSession, fd: RawFd) {
    let _output_gate = sess.output_gate.lock().unwrap();
    let mut out = sess.out.lock().unwrap();
    if out.client.as_ref().is_some_and(|link| link.fd == fd) {
        out.client.take();
    }
    out.watchers.retain(|link| link.fd != fd);
}

/// 编码一帧。增量帧只带相对 `frame_kind` 里镜像的条目变化；整段帧带从 `from` 起的
/// 条目，镜像按它重建。
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_acp_snapshot(
    sess: &AcpSession,
    frame_kind: SnapshotFrameKind<'_>,
    from: usize,
    should_persist: bool,
    include_envelope: bool,
    include_runtime_debug: bool,
    tool_debug_generation_sent: u64,
    projection: SnapshotProjection,
    runtime_debug: Option<Arc<smelt_core::acp_session::RuntimeDebug>>,
) -> EncodedAcpSnapshot {
    let SnapshotProjection {
        history: include_history,
        debug: include_debug,
    } = projection;
    let (mirror, full) = match frame_kind {
        SnapshotFrameKind::Full(mirror) => (mirror, true),
        SnapshotFrameKind::Delta(mirror) => (mirror, false),
    };
    let (mut snap, entries_delta, live, tool_debug_generation, included_tool_debug) = {
        let reduced = sess.reduced.lock().unwrap();
        let tool_debug_generation = reduced.tool_debug_generation;
        let included_tool_debug =
            include_debug && tool_debug_generation != tool_debug_generation_sent;
        let include_runtime_body =
            include_runtime_debug && include_debug && runtime_debug.is_none();
        let (history_len, open_end) = reduced.open_turn_range();
        let live = if include_history {
            smelt_core::acp_session::live_header_from_state(&reduced)
        } else {
            smelt_core::acp_session::live_from_state(&reduced)
        };
        let window_end = if include_history {
            reduced.entries_total()
        } else {
            open_end
        };
        let range = |start| {
            reduced.to_snapshot_range_selective(
                should_persist,
                start,
                window_end,
                included_tool_debug,
                include_runtime_body,
                include_envelope,
            )
        };
        let (mut snapshot, entries_delta) = if full {
            // 窗口只订阅当前一轮，但收尾那一步改到的条目已经划进历史。从记脏的
            // `from` 起一起带上，窗口才接得上，不会当成漏帧把对话清空。
            let start = if include_history {
                from
            } else {
                from.min(history_len)
            };
            (range(start), None)
        } else {
            // 条目不进快照，只发相对对方镜像的变化。已收尾的回合不会再长，
            // 双方都不必再为它留镜像。
            let delta = mirror.encode_entries(
                reduced.entries_base(),
                reduced.entries(),
                from,
                window_end,
                history_len.min(from),
            );
            (range(window_end), Some(delta))
        };
        if let Some(runtime_debug) =
            runtime_debug.filter(|_| include_runtime_debug && include_debug)
        {
            // 父镜像上的是账本。这一帧要带的是宿主刚发来的完整请求体。
            snapshot.runtime_debug = Some(runtime_debug.as_ref().clone());
        } else if !(include_runtime_debug && include_debug) {
            snapshot.runtime_debug = None;
        }
        if !included_tool_debug {
            snapshot.tool_debug = None;
        }
        snapshot.snapshot_revision = sess.snapshot_revision.fetch_add(1, Ordering::SeqCst) + 1;
        (
            snapshot,
            entries_delta,
            live,
            tool_debug_generation,
            included_tool_debug,
        )
    };
    if include_envelope {
        set_conversation_snapshot(sess, &mut snap);
    }
    let provider_pid = sess
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|handle| handle.stdio.lock().unwrap().map(|stdio| stdio.pid));
    let frame = serde_json::to_value(smelt_core::acp_session::AcpSnapshotFrame {
        live: &live,
        snapshot: &snap,
        provider_pid,
    })
    .expect("ACP snapshot frame contains only serializable data");
    let payload = match entries_delta {
        None => {
            let (bytes, rebuilt) = full_frame(&frame);
            *mirror = rebuilt;
            bytes
        }
        Some(entries) => {
            let delta = smelt_core::acp_wire::FrameDelta {
                fields: mirror.encode_header(&frame),
                entries,
            };
            let mut bytes = serde_json::json!({ "delta": delta }).to_string().into_bytes();
            bytes.push(b'\n');
            bytes
        }
    };
    EncodedAcpSnapshot {
        bytes: payload,
        tool_debug_generation,
        included_tool_debug,
    }
}

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
    push_acp_snapshot_since_with_runtime_debug(
        sess,
        should_persist,
        entries_offset,
        false,
        true,
        None,
    );
}

fn push_acp_snapshot_since_with_runtime_debug(
    sess: &AcpSession,
    should_persist: bool,
    entries_offset: Option<usize>,
    include_runtime_debug: bool,
    include_envelope: bool,
    runtime_debug: Option<Arc<smelt_core::acp_session::RuntimeDebug>>,
) {
    let _output_gate = sess.output_gate.lock().unwrap();
    let from = {
        let reduced = sess.reduced.lock().unwrap();
        entries_offset.unwrap_or(reduced.entries_total())
    };
    let out = sess.out.lock().unwrap();
    if let Some(client) = out.client.as_ref() {
        client.mark(
            from,
            should_persist,
            include_runtime_debug,
            include_envelope,
            runtime_debug.clone(),
        );
    }
    for watcher in &out.watchers {
        watcher.mark(
            from,
            should_persist,
            include_runtime_debug,
            include_envelope,
            runtime_debug.clone(),
        );
    }
}

/// 对方要了完整快照：各连接下一帧改发整段帧，发送方镜像跟着重建。
fn request_full_snapshot_frames(sess: &AcpSession) {
    let out = sess.out.lock().unwrap();
    for link in out.client.iter().chain(out.watchers.iter()) {
        link.request_full_frame();
    }
}

#[cfg(test)]
pub(crate) fn push_acp_snapshot_with_runtime_debug_for_test(sess: &AcpSession) {
    push_acp_snapshot_since_with_runtime_debug(sess, false, None, true, true, None);
}

pub(crate) fn push_acp_snapshot(sess: &AcpSession, should_persist: bool) {
    let offset = sess
        .reduced
        .lock()
        .unwrap()
        .entries_total()
        .saturating_sub(1);
    push_acp_snapshot_since(sess, should_persist, Some(offset));
}

pub(crate) fn set_conversation_snapshot(
    sess: &AcpSession,
    snapshot: &mut smelt_core::acp_session::ConversationSnapshot,
) {
    snapshot.conversation_state = Some(smelt_core::conversation::ConversationStateSnapshot {
        binding: sess.conversation_binding.lock().unwrap().clone(),
        pending_agent_preset: sess.pending_agent_preset.lock().unwrap().clone(),
    });
}

/// 只在 daemon 自己还不知道路由时采用客户端值。已确认的绑定不能被
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

fn session_uses_pi_admission(sess: &AcpSession) -> bool {
    sess.launch_spec
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(smelt_core::pi_rpc::is_smelt_pi_launch)
}

/// 在持有 [`AcpSession::turn_completion`] 时，根据最新状态释放刚结束回合的
/// prompt 闸门并最多派发一条排队消息。
///
/// 界面回合结束和 Pi 能不能再收一条 prompt 是两件事。其它 agent 仍在回合结束时放行。
/// Pi 只有 `provider_idle` 才放行；已经写出、还没被收下的那条会占住闸门。
/// 未完成工具只影响展示：迟到的 `ToolFinished` 仍可改写旧 tool id。
pub(crate) fn settle_acp_turn_locked(
    sess: &AcpSession,
    turn_ended: bool,
    ready: bool,
    provider_idle: bool,
    subscribers: &EventHubHandle,
) {
    let idle = matches!(
        sess.reduced.lock().unwrap().phase,
        smelt_core::daemon_state::DaemonPhase::Idle
    );
    if !idle {
        return;
    }
    let pi = session_uses_pi_admission(sess);
    if pi && sess.unaccepted_prompt.lock().unwrap().is_some() {
        return;
    }
    if pi && sess.prompt_in_flight.load(Ordering::SeqCst) && !provider_idle {
        return;
    }
    let gate_was_held = sess.prompt_in_flight.swap(false, Ordering::SeqCst);
    if gate_was_held {
        note_automation_watch(sess);
    }
    if gate_was_held || turn_ended || ready || provider_idle {
        // 用户点的是这一条，要比停在旁边的普通 prompt 先走。
        try_flush_immediate_delivery(sess, subscribers);
        flush_pending_acp_prompt_locked(sess, subscribers);
    }
}

/// drain 日志用的事件名。只取判别式名字，不带负载——日志不该把整段历史或图片
/// base64 打进去。
fn acp_event_name(event: &smelt_core::acp_conn::ConversationEvent) -> &'static str {
    use smelt_core::acp_conn::ConversationEvent as E;
    match event {
        E::Ready { .. } => "Ready",
        E::Status(_) => "Status",
        E::HistoryReplayStarted => "HistoryReplayStarted",
        E::HistoryReplayFinished => "HistoryReplayFinished",
        E::RestoreFailed(_) => "RestoreFailed",
        E::Fatal(_) => "Fatal",
        E::TurnEnded(_) => "TurnEnded",
        _ => "其它",
    }
}

/// 把已经堆在通道里的一批事件收成一次快照游标。
///
/// 重放会在几毫秒里把几十条历史都 `try_send` 进来。若每条都立刻序列化一份
/// 快照，写线程还堵在第一行上，邮箱就会顶满 8MB 并拆掉整条连接；主进程读到
/// 半行 JSON，只能当成宿主挂了再重连，于是同一份历史永远恢复失败。
/// `Some(0)` 表示这一批里清空过或要求全量重发。否则取最早的增量游标。
/// 全都没动 entries 时返回 `None`，调用方按当前长度发空增量。
pub(crate) fn coalesce_snapshot_offset<I>(offsets: I) -> Option<usize>
where
    I: IntoIterator<Item = Option<usize>>,
{
    let mut earliest = None;
    for offset in offsets {
        match offset {
            Some(0) => return Some(0),
            Some(offset) => {
                earliest = Some(earliest.map_or(offset, |earliest: usize| earliest.min(offset)));
            }
            None => {}
        }
    }
    earliest
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
            while let Ok(first) = event_rx.recv().await {
                let _turn_completion = sess.turn_completion.lock().unwrap();
                // 重放把整段历史一次性塞进通道。先收干已经到达的事件，再发一份
                // 快照；实时流通常一次只有一条，行为与逐条推送相同。
                let mut batch = vec![first];
                while let Ok(next) = event_rx.try_recv() {
                    batch.push(next);
                }
                let mut offsets = Vec::with_capacity(batch.len());
                let mut should_persist = false;
                let mut runtime_debug_changed = false;
                let mut envelope_changed = false;
                let mut turn_ended = false;
                let mut ready = false;
                let mut provider_idle = false;
                let mut prompt_refused = false;
                let mut applied = false;
                let mut stop = false;
                for mut ev in batch {
                    if sess.connection_generation.load(Ordering::SeqCst) != generation {
                        // 这条连接已被换掉，剩下的事件不能再改新连接的状态。但被丢掉的
                        // 若是 Ready，会话就会一直停在 Connecting——必须留痕，否则现场
                        // 只看得到「卡着」。
                        crate::dlog(&format!(
                            "acp drain 代数失配退出 id={session_id} gen={generation} \
                             current={} 丢弃事件={}",
                            sess.connection_generation.load(Ordering::SeqCst),
                            acp_event_name(&ev),
                        ));
                        stop = true;
                        break;
                    }
                    if matches!(&ev, smelt_core::acp_conn::ConversationEvent::PromptAccepted) {
                        // 气泡在提交时就写过了，这里不再新增条目，只是状态变了（provider
                        // 排队提示要收），所以只标 applied，不推 entries 偏移。
                        if accept_unaccepted_prompt(sess) {
                            applied = true;
                        }
                        continue;
                    }
                    if matches!(
                        &ev,
                        smelt_core::acp_conn::ConversationEvent::PromptNotAccepted
                    ) {
                        requeue_unaccepted_prompt(sess);
                        prompt_refused = true;
                        applied = true;
                        continue;
                    }
                    if matches!(&ev, smelt_core::acp_conn::ConversationEvent::ProviderIdle) {
                        provider_idle = true;
                        applied = true;
                        continue;
                    }
                    if matches!(
                        &ev,
                        smelt_core::acp_conn::ConversationEvent::TurnEnded(
                            agent_client_protocol::schema::v1::StopReason::Cancelled
                        )
                    ) {
                        park_unaccepted_after_cancel(sess);
                    } else if matches!(&ev, smelt_core::acp_conn::ConversationEvent::TurnFailed(_))
                    {
                        restore_unaccepted_to_composer(sess);
                    }
                    turn_ended |= ev.ends_turn();
                    ready |= matches!(&ev, smelt_core::acp_conn::ConversationEvent::Ready { .. });
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
                        crate::dlog(&format!(
                            "acp drain 收到恢复 Ready id={session_id} gen={generation}"
                        ));
                        let history_id = sess.reduced.lock().unwrap().history_session_id.clone();
                        sess.restore_state
                            .lock()
                            .unwrap()
                            .mark_restored(history_id.as_deref());
                    }
                    if let smelt_core::acp_conn::ConversationEvent::ComposerRestore {
                        texts, ..
                    } = &mut ev
                    {
                        note_immediate_queue_restore(sess, texts);
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
                        offsets.push(outcome.entries_offset);
                        push_acp_snapshot_since(
                            sess,
                            true,
                            coalesce_snapshot_offset(offsets.iter().copied()),
                        );
                        update_acp_daemon_state(sess, &subscribers);
                        stop = true;
                        applied = false;
                        break;
                    }
                    offsets.push(outcome.entries_offset);
                    should_persist |= outcome.should_persist;
                    runtime_debug_changed |= outcome.runtime_debug_changed;
                    envelope_changed |= outcome.envelope_changed;
                    applied = true;
                }
                if prompt_refused && !provider_idle {
                    // 同一批里如果已经空闲，不能先把相位改成思考中，否则这条空闲信号放不出队。
                    remember_provider_hold(sess);
                    applied = true;
                }
                if provider_idle {
                    release_provider_hold(sess);
                }
                if prompt_refused || provider_idle || turn_ended || ready {
                    envelope_changed = true;
                }
                if applied {
                    push_acp_snapshot_since_with_runtime_debug(
                        sess,
                        should_persist,
                        coalesce_snapshot_offset(offsets.iter().copied()),
                        runtime_debug_changed,
                        envelope_changed,
                        None,
                    );
                    update_acp_daemon_state(sess, &subscribers);
                    // 其它 agent 在回合结束时放闸。Pi 还要等它自己声明空闲。
                    settle_acp_turn_locked(sess, turn_ended, ready, provider_idle, &subscribers);
                    // 闸门本来就开着时 settle 不进 flush 分支。还原刚落地的立即发送仍要发出去。
                    try_flush_immediate_delivery(sess, &subscribers);
                }
                if stop {
                    break;
                }
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
            // 放弃写进会话状态，和清掉的 id 同一把锁。普通重启不带这个请求，
            // 库里的旧 id 留到新 id 确认。
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
                true,
            );
            // 直连进程这里就能清库。生产上的会话宿主不打开 SQLite，
            // 主 daemon 合并到 `history_abandoned` 再清。
            clear_durable_history_id(sess);
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
/// 交接前旧读端会停在安全边界，已预取的半行交给继任者当前缀。版本缺口或
/// entry 前缀缺失时仍要求 host 发 offset=0 的全量快照。
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
                    crate::dlog(&format!(
                        "acp 镜像 drain 代数失配退出 id={session_id} gen={generation} current={}",
                        sess.connection_generation.load(Ordering::SeqCst)
                    ));
                    break;
                }
                let _provider_pid = envelope.provider_pid;
                let snapshot = envelope.snapshot;
                let revision = snapshot.snapshot_revision;
                let seen = sess.host_snapshot_revision.load(Ordering::SeqCst);
                if revision != 0 && revision <= seen {
                    crate::dlog(&format!(
                        "acp 镜像丢弃旧快照 id={session_id} rev={revision} seen={seen}"
                    ));
                    continue;
                }

                let incremental_gap = snapshot.entries_offset > 0
                    && seen != 0
                    && revision != 0
                    && revision > seen.saturating_add(1);
                if (awaiting_full_snapshot || incremental_gap) && snapshot.entries_offset != 0 {
                    // 进入等全量状态后，只有 offset=0 的快照能再把镜像推动。全量那份若
                    // 没如约到达，镜像就永久冻在这里，而 host 内部一切正常——必须留痕。
                    crate::dlog(&format!(
                        "acp 镜像等全量快照 id={session_id} rev={revision} seen={seen} \
                         offset={} gap={incremental_gap} awaiting={awaiting_full_snapshot}",
                        snapshot.entries_offset,
                    ));
                    if !awaiting_full_snapshot {
                        awaiting_full_snapshot = true;
                        let request_ok = sess
                            .hosted_handle
                            .lock()
                            .unwrap()
                            .as_ref()
                            .is_some_and(|host| host.request_full_snapshot().is_ok());
                        if !request_ok {
                            crate::dlog(&format!(
                                "acp 镜像请求全量快照失败，drain 退出 id={session_id}"
                            ));
                            break;
                        }
                    }
                    continue;
                }

                let runtime_debug_changed = snapshot.runtime_debug.is_some();
                let should_persist = snapshot.should_persist;
                let requested_offset = snapshot.entries_offset;
                let (owner_ids, runtime_debug, history_id, history_abandoned) = {
                    let mut reduced = sess.reduced.lock().unwrap();
                    match reduced.merge_hosted_snapshot(snapshot) {
                        Ok(merged) => (
                            live_acp_owner_ids(&reduced),
                            merged.runtime_debug.map(Arc::new),
                            normalize_resume_id(reduced.history_session_id.clone()),
                            reduced.history_abandoned,
                        ),
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
                // 宿主在干活就算活动；安静下来之后才开始计闲置。
                if crate::acp_idle::reduced_suspend_blocker(&sess.reduced.lock().unwrap()).is_some()
                {
                    sess.idle.touch();
                }
                apply_durable_history_from_host(sess, history_id, history_abandoned);
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
                push_acp_snapshot_since_with_runtime_debug(
                    sess,
                    should_persist,
                    Some(requested_offset),
                    runtime_debug_changed,
                    true,
                    runtime_debug,
                );
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
    abandon_history: bool,
) {
    let sess = &slot.value;
    sess.idle.mark_running();
    let (generation, restore_policy) = {
        let _turn_completion = sess.turn_completion.lock().unwrap();
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        // 每次换连接都推进代数。旧 drain 即使随后才看到 event_rx 关闭，也不能
        // 把新连接的 handle/Starting 状态清掉。
        let generation = sess.connection_generation.fetch_add(1, Ordering::SeqCst) + 1;
        crate::dlog(&format!(
            "acp relaunch id={id} gen={generation} resume={resume_id:?} fork={fork_id:?}"
        ));
        let restore_policy = {
            let mut reduced = sess.reduced.lock().unwrap();
            let previous_history_session_id = reduced.history_session_id.clone();
            let restore_policy = sess.restore_state.lock().unwrap().begin(
                if fork_id.is_some() {
                    None
                } else {
                    resume_id.as_deref()
                },
                !reduced.entries().is_empty(),
            );
            smelt_core::acp_session::reset_for_restart(&mut reduced);
            // `resume_id` 只是这次 session/load 的候选身份，不能在 load 成功前
            // 写进 canonical history，也不能改库里已经确认的 id。否则 GUI 收到
            // spawn 后的第一份 Starting 快照会把一个可能不存在的 id 当成已确认
            // 历史。真正成功时由 Ready 登记；session/new 的首条 prompt 由
            // note_prompt_sent 登记，再写入 SQLite。同一 id 的重启保留内存里的
            // 身份和放弃标志。切换候选时只清内存，同时放下放弃标志，崩溃后仍
            // 回到库里的旧对话。历史文件确认读不出来时，id 和放弃标志在这把锁里
            // 一起写成「已放弃」。
            if abandon_history {
                reduced.history_session_id = None;
                reduced.history_abandoned = true;
            } else if previous_history_session_id.as_deref() != resume_id.as_deref() {
                reduced.history_session_id = None;
                reduced.history_abandoned = false;
            }
            restore_policy
        };
        (generation, restore_policy)
    };
    // 无头恢复不会经过 GUI 的 AgentHostState 迁移；daemon 自己也要把 Smelt
    // 发布过的旧默认值升到当前适配器，否则历史会话 relaunch 仍会拉旧包。
    upgrade_released_adapter_launch(&mut launch);
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
            "resume_id": resume_id,
            "fork_id": fork_id,
            "fork_cut": &fork_cut,
            "conversation_binding": sess.conversation_binding.lock().unwrap().clone(),
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
    // unreaped 重试只观察、不收尸、也不重杀。上次的 wait 还占着这个 pid，
    // 这里再 waitpid 会和它抢；此 pid 若已被复用，重杀会误伤无辜进程组。
    // 探得仍在就继续留着不 spawn（fail closed）。
    // 先 take 掉再判：`if let` 的 scrutinee guard 会活过整个 body，里面再
    // lock 同一把就是自死锁（之前 instant-true 从不进 body，雷没爆过）。
    let unreaped = sess.unreaped_pid.lock().unwrap().take();
    if let Some(pid) = unreaped
        && !smelt_core::acp_conn::prove_process_group_dead_without_reaping(pid, ACP_SHUTDOWN_GRACE)
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
    conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
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
        automation_watch: Mutex::new(None),
        pending_prompts: Mutex::new(VecDeque::new()),
        immediate_delivery: Mutex::new(None),
        unaccepted_prompt: Mutex::new(None),
        provider_hold: AtomicBool::new(false),
        hosted_handle: Mutex::new(None),
        host_snapshot_revision: AtomicU64::new(0),
        handle: Mutex::new(None),
        unreaped_pid: Mutex::new(None),
        cwd: cwd.clone(),
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
        conversation_submit: Mutex::new(()),
        pending_agent_preset: Mutex::new(pending_agent_preset),
        durable_history_session_id: Mutex::new(None),
        idle: Default::default(),
    }
}

/// Pi 还在跑。空闲界面拉回转圈，好接住迟到的输出；这不是一条新回合。
pub(crate) fn remember_provider_hold(sess: &AcpSession) {
    let mut state = sess.reduced.lock().unwrap();
    let was_idle = matches!(state.phase, smelt_core::daemon_state::DaemonPhase::Idle)
        && state.turn_started_at_ms.is_none();
    smelt_core::acp_session::note_provider_still_running(&mut state);
    drop(state);
    if was_idle {
        sess.provider_hold.store(true, Ordering::SeqCst);
    }
}

/// 占位用的转圈随着 Pi 空闲一起收回。真正被收下的回合不走这里。
pub(crate) fn release_provider_hold(sess: &AcpSession) {
    if !sess.provider_hold.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut state = sess.reduced.lock().unwrap();
    smelt_core::acp_session::clear_provider_queue_status(&mut state);
    if matches!(state.phase, smelt_core::daemon_state::DaemonPhase::Thinking) {
        state.phase = smelt_core::daemon_state::DaemonPhase::Idle;
        state.turn_started_at_ms = None;
    }
}

fn take_unaccepted_prompt(sess: &AcpSession) -> Option<QueuedAcpPrompt> {
    sess.unaccepted_prompt.lock().unwrap().take()
}

pub(crate) fn requeue_unaccepted_prompt(sess: &AcpSession) {
    let Some(mut prompt) = take_unaccepted_prompt(sess) else {
        return;
    };
    // 「Pi 还在跑」不是用户没说过这句话，只是还没送进去：气泡留在原地，
    // provider 空下来重发同一条，不能再写第二个气泡。
    prompt.echoed = true;
    sess.prompt_in_flight.store(true, Ordering::SeqCst);
    sess.pending_prompts.lock().unwrap().push_front(prompt);
}

/// 还草稿：正文回输入框的同时把消息流里的气泡撤掉，不能两处都留。
fn restore_prompt_to_composer(sess: &AcpSession, prompt: QueuedAcpPrompt) {
    let mut state = sess.reduced.lock().unwrap();
    if prompt.echoed {
        smelt_core::acp_session::undo_prompt_sent(&mut state);
    }
    smelt_core::acp_session::queue_composer_restore(&mut state, vec![prompt.text], prompt.images);
}

pub(crate) fn restore_unaccepted_to_composer(sess: &AcpSession) {
    let Some(prompt) = take_unaccepted_prompt(sess) else {
        return;
    };
    restore_prompt_to_composer(sess, prompt);
}

pub(crate) fn park_unaccepted_after_cancel(sess: &AcpSession) {
    let Some(prompt) = take_unaccepted_prompt(sess) else {
        return;
    };
    match prompt.origin {
        AcpPromptOrigin::User => restore_prompt_to_composer(sess, prompt),
    }
}

/// Pi 收下了这一条。气泡在提交时就已经写过，这里只收掉「未确认」标记
/// 并放掉 provider 排队状态。
pub(crate) fn accept_unaccepted_prompt(sess: &AcpSession) -> bool {
    if take_unaccepted_prompt(sess).is_none() {
        return false;
    }
    sess.provider_hold.store(false, Ordering::SeqCst);
    let mut state = sess.reduced.lock().unwrap();
    smelt_core::acp_session::clear_provider_queue_status(&mut state);
    true
}

/// 在调用方取得 daemon 回合槽位后发送 prompt。
///
/// 气泡一律在这里就落地，不等 agent 确认：用户确实提交了这条，这是 daemon 自己
/// 掌握的事实。Pi 额外记一笔 `unaccepted_prompt`，只用来决定被拒收时是重发还是
/// 还草稿；`echoed` 的重发不再写第二遍。会话命令（`/reload`）没有模型回合，不回显。
pub(crate) fn send_acp_prompt_reserved(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    delivery_id: Option<String>,
    origin: AcpPromptOrigin,
    echoed: bool,
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
        *sess.unaccepted_prompt.lock().unwrap() = None;
        return Err("ACP session is not running");
    };
    let shown_images = images.clone();
    let shown_text = text.clone();
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx
        .try_send(smelt_core::acp_conn::ConversationCommand::Prompt { text, images })
        .is_ok()
    {
        if remember_outgoing_prompt(sess, shown_text, shown_images, delivery_id, origin, echoed) {
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
        }
        Ok(())
    } else {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        sess.prompt_in_flight.store(false, Ordering::SeqCst);
        *sess.unaccepted_prompt.lock().unwrap() = None;
        Err("ACP command channel is busy or closed")
    }
}

/// 投影已经写入用户气泡时返回 true。会话命令不写气泡，重发也不重写。
fn remember_outgoing_prompt(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    delivery_id: Option<String>,
    origin: AcpPromptOrigin,
    echoed: bool,
) -> bool {
    let pi = session_uses_pi_admission(sess);
    if pi && smelt_core::pi_rpc::parse_session_slash(&text).is_some() {
        return false;
    }
    if pi {
        *sess.unaccepted_prompt.lock().unwrap() = Some(QueuedAcpPrompt {
            text: text.clone(),
            images: images.clone(),
            delivery_id: delivery_id.clone(),
            origin,
            echoed: true,
        });
    }
    if echoed {
        // 重发：气泡上一次就写过了，这一轮只是把它真正送进 provider。
        return false;
    }
    smelt_core::acp_session::note_prompt_sent_with_delivery(
        &mut sess.reduced.lock().unwrap(),
        text,
        images,
        delivery_id,
    );
    true
}

/// Pi 的会话命令在进入 Prompt / Steer / FollowUp 之前处理。
///
/// 其它 agent 的同名文本仍是普通输入。`/reload` 由驱动拒绝，不写给 Pi。
/// `/shake` 仍走 prompt，由扩展命令执行。
fn dispatch_pi_session_command(
    sess: &AcpSession,
    text: &str,
    images: Vec<smelt_core::acp_conn::PromptImage>,
) -> Result<bool, &'static str> {
    let is_pi = sess
        .launch_spec
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(smelt_core::pi_rpc::is_smelt_pi_launch);
    if !is_pi {
        return Ok(false);
    }
    let Some(slash) = smelt_core::pi_rpc::parse_session_slash(text) else {
        return Ok(false);
    };
    let in_turn = sess.prompt_in_flight.load(Ordering::SeqCst);
    match slash {
        smelt_core::pi_rpc::PiSessionSlash::Compact {
            custom_instructions,
        } => {
            send_driver_command(
                sess,
                smelt_core::acp_conn::ConversationCommand::Compact {
                    custom_instructions,
                },
            )?;
            Ok(true)
        }
        smelt_core::pi_rpc::PiSessionSlash::Reload | smelt_core::pi_rpc::PiSessionSlash::Shake
            if in_turn =>
        {
            // 驱动在活动回合上拒绝这两条，并发状态而不是模型消息。不能走
            // `note_prompt_sent`，否则会把正在跑的回合覆盖成一条新回合。
            send_driver_command(
                sess,
                smelt_core::acp_conn::ConversationCommand::Prompt {
                    text: text.to_string(),
                    images,
                },
            )?;
            Ok(true)
        }
        smelt_core::pi_rpc::PiSessionSlash::Reload | smelt_core::pi_rpc::PiSessionSlash::Shake => {
            Ok(false)
        }
    }
}

fn send_driver_command(
    sess: &AcpSession,
    command: smelt_core::acp_conn::ConversationCommand,
) -> Result<(), &'static str> {
    let handle = sess
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .map(|h| (h.cmd_tx.clone(), Arc::clone(&h.in_flight_rpc)));
    let Some((cmd_tx, in_flight_rpc)) = handle else {
        return Err("ACP session is not running");
    };
    in_flight_rpc.fetch_add(1, Ordering::SeqCst);
    if cmd_tx.try_send(command).is_err() {
        in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
        return Err("ACP command channel is busy or closed");
    }
    Ok(())
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

fn request_acp_cancel(sess: &AcpSession, subscribers: &EventHubHandle) -> Result<(), &'static str> {
    let result = {
        let handle = sess.handle.lock().unwrap();
        let Some(h) = handle.as_ref() else {
            return Err("ACP session is not running");
        };
        h.cmd_tx
            .try_send(smelt_core::acp_conn::ConversationCommand::Cancel)
            .map_err(|_| "ACP command channel is busy or closed")
    };
    if result.is_ok() {
        smelt_core::acp_session::note_cancel_requested(&mut sess.reduced.lock().unwrap());
        push_acp_snapshot(sess, false);
        update_acp_daemon_state(sess, subscribers);
    }
    result
}

/// 原生排队「立即发送」。认领这一条，停掉当前回答或只清队列，等 clear_queue 把它还回来之后只发一次。
/// 同一文本再来一次是同一个动作。调用方必须能拿到 `turn_completion`；这里自己拿，避免和还原处理交错。
fn deliver_queued_now(
    sess: &AcpSession,
    text: String,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let _turn = sess.turn_completion.lock().unwrap();
    if sess.handle.lock().unwrap().is_none() {
        return Err("ACP session is not running");
    }
    {
        let slot = sess.immediate_delivery.lock().unwrap();
        if let Some(existing) = slot.as_ref() {
            return if existing.text == text {
                Ok(())
            } else {
                Err("another immediate send is in progress")
            };
        }
    }
    let (images, stop_turn) = {
        let state = sess.reduced.lock().unwrap();
        if !state.supports_native_queue {
            return Err("agent does not support native queue");
        }
        if matches!(
            state.phase,
            smelt_core::daemon_state::DaemonPhase::Connecting
                | smelt_core::daemon_state::DaemonPhase::Dead
        ) {
            return Err("ACP session is not running");
        }
        let Some((images, _)) = smelt_core::acp_session::queued_item_images(&state, &text) else {
            return Err("queued item is gone");
        };
        // 界面已经空闲就只清队列。还在跑（含取消还没落成 Idle）才停当前回答。
        let stop_turn = !matches!(state.phase, smelt_core::daemon_state::DaemonPhase::Idle);
        (images, stop_turn)
    };
    *sess.immediate_delivery.lock().unwrap() = Some(ImmediateDelivery {
        text,
        images,
        awaiting_clear: true,
        send: false,
    });
    let result = if stop_turn {
        request_acp_cancel(sess, subscribers)
    } else {
        send_acp_clear_queue(sess)
    };
    if result.is_err() {
        *sess.immediate_delivery.lock().unwrap() = None;
    }
    result
}

/// clear_queue 还回来的原文里拿掉已经认领的那一条。没还回来就丢掉这次发送，不能再补一次。
pub(crate) fn note_immediate_queue_restore(sess: &AcpSession, texts: &mut Vec<String>) {
    let mut slot = sess.immediate_delivery.lock().unwrap();
    let Some(delivery) = slot.as_mut() else {
        return;
    };
    if !delivery.awaiting_clear {
        return;
    }
    delivery.awaiting_clear = false;
    delivery.send = smelt_core::acp_session::take_claimed_queue_text(texts, &delivery.text);
    if !delivery.send {
        *slot = None;
    }
}

/// 闸门开着、界面空闲、还原已经证明这一条还在队列里，才发一次。再调用不会再发。
pub(crate) fn try_flush_immediate_delivery(sess: &AcpSession, subscribers: &EventHubHandle) {
    let idle = matches!(
        sess.reduced.lock().unwrap().phase,
        smelt_core::daemon_state::DaemonPhase::Idle
    );
    if !idle || sess.unaccepted_prompt.lock().unwrap().is_some() {
        return;
    }
    if sess.prompt_in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    let delivery = {
        let mut slot = sess.immediate_delivery.lock().unwrap();
        let ready = slot
            .as_ref()
            .is_some_and(|delivery| delivery.send && !delivery.awaiting_clear);
        if !ready {
            sess.prompt_in_flight.store(false, Ordering::SeqCst);
            return;
        }
        slot.take().expect("ready immediate delivery")
    };
    if send_acp_prompt_reserved(
        sess,
        delivery.text.clone(),
        delivery.images.clone(),
        None,
        AcpPromptOrigin::User,
        false,
        subscribers,
    )
    .is_err()
    {
        *sess.immediate_delivery.lock().unwrap() = Some(delivery);
    }
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

/// 编辑并重发最后一条用户消息。入口是 GUI 传来的 entry 下标，但下标背后的一切
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
    let (phase, target, latest_user_index) = {
        let reduced = sess.reduced.lock().unwrap();
        // 重放期间工作缓冲是草稿，GUI 的下标按已发布正文算，两边对不上；
        // 截断也不能落在半成品上。等重放收尾再回退。
        if reduced.replaying_history {
            return Err("rewind requires history replay to finish");
        }
        let latest_user_index = reduced.entries().iter().rposition(|entry| match entry {
            smelt_core::acp_chat::AcpEntry::User(text) => {
                !smelt_core::acp_chat::is_interrupt_marker(text)
            }
            smelt_core::acp_chat::AcpEntry::UserWithImages { text, .. } => {
                !smelt_core::acp_chat::is_interrupt_marker(text)
            }
            _ => false,
        });
        (
            reduced.phase,
            reduced.entries().get(entry_index).map(|entry| match entry {
                smelt_core::acp_chat::AcpEntry::User(text) => Some(text.clone()),
                smelt_core::acp_chat::AcpEntry::UserWithImages { text, .. } => Some(text.clone()),
                _ => None,
            }),
            latest_user_index,
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
    if latest_user_index != Some(entry_index) {
        return Err("rewind target is not the latest user message");
    }
    // 同文本消息靠序号区分：agent 的可分叉列表里取第 N 条同文本消息，N 必须与
    // 本地同文本计数一致。中断标记不是真消息，不参与计数。
    let occurrence = {
        let reduced = sess.reduced.lock().unwrap();
        reduced
            .entries()
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

/// 回合结束后只释放一条 daemon 队列中的 prompt。调用方必须持有 `turn_completion`；
/// 剩余消息等下一次 TurnEnded，保证 provider 永远不会看到并发回合。
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
        prompt.origin,
        prompt.echoed,
        subscribers,
    )
    .is_err()
    {
        sess.pending_prompts.lock().unwrap().push_front(prompt);
    }
}

pub(crate) fn try_rename_running_acp_session(sessions: &AcpSessions, id: &str, title: &str) {
    let Some(slot) = sessions.get(id) else {
        return;
    };
    let _ = sessions.with_current(id, &slot, |sess| {
        let handle = sess.handle.lock().unwrap();
        let Some(handle) = handle.as_ref() else {
            return;
        };
        let _ = handle
            .cmd_tx
            .try_send(smelt_core::acp_conn::ConversationCommand::SetSessionTitle(
                title.to_string(),
            ));
    });
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
    // Refresh 要的是这个守护手里的完整快照：发给连着它的客户端，宿主模式下也由这里答，
    // 不转给宿主。父进程要宿主重发时直接调 `request_full_snapshot`。
    if !matches!(action, AcpUserAction::Refresh)
        && let Some(hosted) = sess.hosted_handle.lock().unwrap().as_ref()
    {
        // 历史页由父进程直接问宿主，不能被 GUI 动作转发过去。
        if matches!(action, AcpUserAction::HistoryPage { .. }) {
            return Err("history page is internal");
        }
        return hosted.send_action(&action).map_err(|error| {
            eprintln!("[acp] 转发用户动作到独立宿主失败：{error}");
            "ACP session host is unavailable"
        });
    }
    match action {
        AcpUserAction::Refresh => {
            request_full_snapshot_frames(sess);
            push_acp_snapshot_since_with_runtime_debug(sess, false, Some(0), true, true, None);
            Ok(())
        }
        AcpUserAction::HistoryPage { .. } => Err("history page is internal"),
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
            // 会话命令不是模型输入。必须在 steer / 本地队列之前认出来，否则
            // `/reload` 会进 steering 队列，驱动也收不到可解析的 Prompt。
            match dispatch_pi_session_command(sess, &text, images.clone()) {
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
            if sess.prompt_in_flight.swap(true, Ordering::SeqCst) {
                // 只有回合还在跑才 steer。取消已经把界面收成 Idle、闸门却还握着等 Pi 空闲时，
                // 再插进当前回合会把「立即发送」的消息退回队列；再点一次就会连发两遍。
                let steer =
                    smelt_core::acp_session::accepts_mid_turn_steer(&sess.reduced.lock().unwrap());
                if steer {
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
                }
                sess.pending_prompts
                    .lock()
                    .unwrap()
                    .push_back(QueuedAcpPrompt {
                        text,
                        images,
                        delivery_id,
                        origin: AcpPromptOrigin::User,
                        echoed: false,
                    });
                push_acp_snapshot_since(sess, false, None);
                return Ok(());
            }
            let result = send_acp_prompt_reserved(
                sess,
                text,
                images,
                delivery_id.clone(),
                AcpPromptOrigin::User,
                false,
                subscribers,
            );
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
        } => {
            match dispatch_pi_session_command(sess, &text, images.clone()) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => return Err(error),
            }
            send_acp_follow_up(sess, text, images, delivery_id, subscribers)
        }
        AcpUserAction::Compact => send_acp_compact(sess),
        AcpUserAction::AcknowledgeComposerRestore { revision } => {
            if smelt_core::acp_session::acknowledge_composer_restore(
                &mut sess.reduced.lock().unwrap(),
                revision,
            ) {
                push_acp_snapshot_since(sess, false, None);
            }
            Ok(())
        }
        AcpUserAction::ClearQueue => send_acp_clear_queue(sess),
        AcpUserAction::DeliverQueuedNow { text } => deliver_queued_now(sess, text, subscribers),
        AcpUserAction::RewindToMessage { entry_index } => send_acp_rewind(sess, entry_index),
        AcpUserAction::Cancel => request_acp_cancel(sess, subscribers),
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
        AcpUserAction::RefreshModels => {
            let handle = sess.handle.lock().unwrap();
            let Some(h) = handle.as_ref() else {
                return Err("ACP session is not running");
            };
            // 不计 in_flight_rpc：这是背景刷新，不应让输入栏显示成「正在处理」。
            h.cmd_tx
                .try_send(ConversationCommand::RefreshModels)
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

/// 用户输入与 Task/Peer/Automation delivery 的分界。Direct 绑定进入 ACP；
/// 自动化 Run 拒绝额外 composer 输入。
pub(crate) fn dispatch_conversation_input(
    sess: &AcpSession,
    mut input: smelt_core::conversation::ConversationInput,
    subscribers: &EventHubHandle,
) -> Result<(), smelt_core::conversation::ConversationSubmitError> {
    use smelt_core::conversation::ConversationBinding;

    let _submit = sess.conversation_submit.lock().unwrap();
    let pending_preset = sess.pending_agent_preset.lock().unwrap().clone();
    let binding = sess.conversation_binding.lock().unwrap().clone();
    match binding.unwrap_or_default() {
        ConversationBinding::Direct => {}
        ConversationBinding::Automation { .. } => {
            return Err(smelt_core::conversation::ConversationSubmitError::rejected(
                "automation Run input is daemon-owned",
            ));
        }
    }
    input.text =
        smelt_core::conversation::merge_agent_preset(pending_preset.as_deref(), &input.text);
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
    if pending_preset.is_some() {
        *sess.pending_agent_preset.lock().unwrap() = None;
        // 显式发布消费状态，确保所有客户端同步，并让 workspace 在进程退出前清掉
        // 已使用的预设。
        push_acp_snapshot_since(sess, true, None);
    }
    Ok(())
}

pub(crate) fn conversation_submit_response(
    result: Result<(), smelt_core::conversation::ConversationSubmitError>,
) -> serde_json::Value {
    match result {
        Ok(()) => serde_json::json!({"ok": true}),
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
    let submit_started = std::time::Instant::now();
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
        crate::acp_idle::wake_suspended_acp_session(id, &slot, acp_sessions, subscribers)
            .map_err(smelt_core::conversation::ConversationSubmitError::rejected)?;
        acp_sessions
            .with_current(id, &slot, |session| {
                apply_submit_config_values(session, &config_values, subscribers)?;
                dispatch_conversation_input(session, input, subscribers)
            })
            .ok_or_else(|| {
                smelt_core::conversation::ConversationSubmitError::rejected(
                    "ACP session was replaced",
                )
            })?
    });
    let response = conversation_submit_response(result);
    // 用户按下回车到这里是 Smelt 自己的开销；气泡落地还要等 Pi 的 admission
    // （见 pi_rpc 里「被 Pi 收下用时」那行）。两条日志的时间差就是分界点。
    crate::dlog(&format!(
        "acp submit id={id} 派发用时 {:?}",
        submit_started.elapsed()
    ));
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
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
    let action = match smelt_core::acp_session::parse_user_action(action_value) {
        Ok(action) => action,
        Err(unknown) => {
            // 只让这一次动作失败，连接照旧。对端比本端新时把话说清楚，否则
            // 只能看到一句【无效动作】，分不出是守护进程该升级还是消息真坏了。
            let message = unknown.message();
            if unknown.newer_peer {
                eprintln!("[acp] 会话 {id} 收到本版不认识的动作：{message}");
            }
            let _ = smelt_runtime_client::protocol::write_json_line(
                &mut conn,
                &serde_json::json!({ "ok": false, "error": message }),
            );
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
                            | smelt_core::acp_session::AcpUserAction::DeliverQueuedNow { .. }
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
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({
                "ok": false,
                "error": error
            }),
        );
        return;
    }

    if crate::acp_idle::acp_action_needs_runtime(&action)
        && let Err(error) =
            crate::acp_idle::wake_suspended_acp_session(id, &slot, acp_sessions, subscribers)
    {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": false, "error": error }),
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
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
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
            let omit_history = req.omit_history;
            let (history_len, open_end) = reduced.open_turn_range();
            let live = smelt_core::acp_session::live_from_state(&reduced);
            let tool_debug_generation = reduced.tool_debug_generation;
            let mut snapshot = if omit_history {
                reduced.to_snapshot_range(false, history_len, open_end)
            } else {
                reduced.to_snapshot(false)
            };
            snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
            drop(reduced);
            set_conversation_snapshot(sess, &mut snapshot);
            let provider_pid = sess
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|handle| handle.stdio.lock().unwrap().map(|stdio| stdio.pid));
            let (initial, mirror) = full_frame(
                &serde_json::to_value(smelt_core::acp_session::AcpSnapshotFrame {
                    live: &live,
                    snapshot: &snapshot,
                    provider_pid,
                })
                .expect("ACP snapshot frame contains only serializable data"),
            );
            let Ok(attachment) = acp_snapshot_link_for_slot(
                &slot,
                c,
                initial,
                mirror,
                tool_debug_generation,
                &id,
                "acp-client",
                SnapshotProjection {
                    history: !req.omit_history,
                    debug: true,
                },
            ) else {
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
    sess.idle.touch();
    // ACP 控制连接活着：headless 自升级让路。对话可以连着看很久却不再走
    // desktop 鉴权 / 终端 open，不能靠过期时间戳判断「没人看」。
    let _viewer = crate::protocol::ViewerLease::acquire();

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
        if let smelt_core::acp_session::AcpUserAction::HistoryPage {
            request_id,
            before,
            limit,
        } = action
        {
            // 父进程的控制连接上不回答：正文在宿主。宿主进程没有 hosted_handle，就地切一页。
            // 挂起的会话也在父进程里，镜像只剩当前回合，同样不答。
            if sess.hosted_handle.lock().unwrap().is_some() || sess.idle.is_suspended() {
                continue;
            }
            let bytes = history_page_bytes(sess, request_id, before, limit);
            let _output_gate = sess.output_gate.lock().unwrap();
            if let Some(client) = sess.out.lock().unwrap().client.as_ref() {
                client.enqueue_line(bytes);
            }
            continue;
        }
        if crate::acp_idle::acp_action_needs_runtime(&action)
            && let Err(error) =
                crate::acp_idle::wake_suspended_acp_session(&id, &slot, &acp_sessions, &subscribers)
        {
            eprintln!("[acp] 会话 {id} 唤醒失败，丢弃这次动作：{error}");
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

/// 宿主按绝对下标切一页，经快照写线程回给父进程。调试正文不进历史页。
fn history_page_bytes(sess: &AcpSession, request_id: u64, before: usize, limit: usize) -> Vec<u8> {
    let limit = limit.clamp(1, 500);
    let mut snapshot = {
        let reduced = sess.reduced.lock().unwrap();
        let end = before.min(reduced.entries_total());
        let start = end.saturating_sub(limit);
        let mut snapshot = reduced.to_snapshot_range(false, start, end);
        snapshot.runtime_debug = None;
        snapshot
    };
    snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
    let line = serde_json::json!({
        "history_page": acp_runtime_host::HistoryPageBody {
            request_id,
            snapshot,
        }
    });
    let mut bytes = line.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

fn write_rejected_acp_snapshot(mut conn: UnixStream, reason: &str) {
    let mut state = smelt_core::acp_session::AcpSessionState::default();
    state.phase = smelt_core::daemon_state::DaemonPhase::Dead;
    state.end_reason = reason.to_string();
    state.end_kind = smelt_core::acp_session::AcpEndKind::ProviderFailed;
    let snapshot = state.to_snapshot(true);
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut conn,
        &serde_json::json!({ "snapshot": snapshot }),
    );
}

/// 父镜像裁过前缀时，向还活着的宿主要回全文，供下一次 `to_snapshot` 当种子。
/// 要不到就留下当前这一截。种子仍带着绝对下标，新宿主重放会话文件前不会把历史长度缩短。
fn reassemble_hosted_transcript(sess: &AcpSession) {
    let trimmed = sess.reduced.lock().unwrap().entries_base() > 0;
    if !trimmed {
        return;
    }
    let Some(host) = sess.hosted_handle.lock().unwrap().clone() else {
        return;
    };
    match host.read_snapshot_covering(0, usize::MAX) {
        Ok(snapshot) if snapshot.entries_offset == 0 && !snapshot.entries.is_empty() => {
            sess.reduced
                .lock()
                .unwrap()
                .install_resident_entries(snapshot.entries);
        }
        Ok(snapshot) => {
            eprintln!(
                "[acp] 重启前宿主全文不完整（offset={}，{} 条），新宿主只能靠会话文件重放",
                snapshot.entries_offset,
                snapshot.entries.len()
            );
        }
        Err(error) => {
            eprintln!("[acp] 重启前读不回宿主全文：{error}");
        }
    }
}

/// Ensure an ACP runtime exists without attaching a controlling client. Both desktop
/// `acp_open` and mobile `acp_create` use this path, so launch/resume semantics cannot drift.
pub(crate) fn ensure_acp_session(
    req: &AcpOpenRequest,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<Arc<AcpSlot<AcpSession>>, String> {
    loop {
        let (slot, created) = acp_sessions.reserve_with(&req.id, || {
            make_acp_session(
                &req.id,
                req.cwd.clone(),
                req.conversation_binding.clone(),
                req.pending_agent_preset.clone(),
            )
        });
        if !created {
            adopt_unknown_conversation_binding(&slot.value, req.conversation_binding.clone());
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
        let resume_id = resolve_resume_id(
            &req.id,
            req.resume_id.clone(),
            known_history,
            req.fork_id.is_none(),
        );
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
            if alive && !created {
                // 父镜像只留着当前回合。新宿主的种子要全文，先向还活着的旧宿主要回来。
                reassemble_hosted_transcript(&slot.value);
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
                false,
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
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
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

/// 只读旁观，不参与 client 顶替，可多个并存。
///
/// 会话没在跑时 daemon 自己起不来它：启动规格归桌面端。如果它是桌面菜单里的
/// 对话（daemon 冷重启后桌面只放了占位），就请桌面端拉起来再接上；拉不起来写一行
/// `{"unavailable": {"reason": ..}}` 说清楚，绝不静默断开——旁观方分不清「没有」
/// 和「还没到」，只会一直转圈。
pub(crate) fn handle_acp_watch(
    mut conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    acp_sessions: AcpSessions,
    workspace_menu: &WorkspaceMenuStore,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let include_debug = request_includes_debug(v);
    let slot = match acp_sessions.get(&id) {
        Some(slot) => slot,
        None => match wake_dormant_conversation(
            &id,
            &acp_sessions,
            workspace_menu,
            subscribers,
            ACP_WAKE_TIMEOUT,
        ) {
            Ok(slot) => slot,
            Err(reason) => {
                let _ = smelt_runtime_client::protocol::write_json_line(
                    &mut conn,
                    &serde_json::json!({ "unavailable": { "reason": reason } }),
                );
                return;
            }
        },
    };
    // 先拿输出闸门再读归约状态，和快照推送同一把锁。向宿主要历史时必须先把
    // 闸门和生命周期锁放开，否则会卡住正在写的那一帧。
    let mut host_page: Option<smelt_core::acp_session::ConversationSnapshot> = None;
    let mut asked_host = false;
    let attached_fd = loop {
        let lifecycle = slot.lifecycle.lock().unwrap();
        if !acp_sessions.is_current(&id, &slot) {
            return;
        }
        let sess = &slot.value;
        let _output_gate = sess.output_gate.lock().unwrap();
        let reduced = sess.reduced.lock().unwrap();
        let entries_total = reduced.entries_total();
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
        if offset < reduced.entries_base() && !asked_host {
            let host = sess.hosted_handle.lock().unwrap().clone();
            drop(reduced);
            drop(_output_gate);
            drop(lifecycle);
            asked_host = true;
            let end = entries_total.max(offset.saturating_add(1));
            host_page = host.and_then(|host| host.read_snapshot_covering(offset, end).ok());
            continue;
        }
        let Ok(c) = conn.try_clone() else { return };
        let tool_debug_generation = reduced.tool_debug_generation;
        let mut snapshot = reduced.snapshot_with_older_page(offset, host_page.as_ref());
        drop(reduced);
        if !include_debug {
            omit_debug_sidecars(&mut snapshot);
        }
        snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
        set_conversation_snapshot(sess, &mut snapshot);
        let (initial, mirror) = full_frame(&serde_json::json!({ "snapshot": snapshot }));
        let Ok(attachment) = acp_snapshot_link_for_slot(
            &slot,
            c,
            initial,
            mirror,
            tool_debug_generation,
            &id,
            "acp-watcher",
            SnapshotProjection {
                history: true,
                debug: include_debug,
            },
        ) else {
            return;
        };
        let fd = attachment.fd;
        sess.out.lock().unwrap().watchers.push(attachment);
        drop(_output_gate);
        drop(lifecycle);
        break fd;
    };
    let _viewer = crate::protocol::ViewerLease::acquire();
    let mut scratch = [0u8; 64];
    let _ = reader.read(&mut scratch);
    let _output_gate = slot.value.output_gate.lock().unwrap();
    slot.value
        .out
        .lock()
        .unwrap()
        .watchers
        .retain(|w| w.fd != attached_fd);
}

/// 桌面端收到唤醒请求后立刻 `acp_open`，slot 在它接上时就已占住；等的只是事件
/// 投递和 GUI 主线程的一拍。宽裕到覆盖 GUI 刚起、对话占位还在恢复的窗口。
const ACP_WAKE_TIMEOUT: Duration = Duration::from_secs(15);
const ACP_WAKE_POLL: Duration = Duration::from_millis(100);

/// 把一段桌面端拥有、当前没在跑的对话拉起来，等到 runtime slot 出现。
/// 错误串是给手机用户看的理由。
fn wake_dormant_conversation(
    id: &str,
    acp_sessions: &AcpSessions,
    workspace_menu: &WorkspaceMenuStore,
    subscribers: &EventHubHandle,
    timeout: Duration,
) -> Result<Arc<AcpSlot<AcpSession>>, String> {
    if workspace_menu.lock().unwrap().acp_session(id).is_none() {
        return Err("This conversation is no longer running.".to_string());
    }
    match subscribers.request_conversation_wake(id) {
        Ok(true) => {}
        Ok(false) => {
            return Err(
                "Smelt is not open on the computer, so this conversation can't be resumed."
                    .to_string(),
            );
        }
        Err(error) => {
            eprintln!("[acp] 唤醒对话 {id} 失败：{error}");
            return Err("Could not ask the computer to resume this conversation.".to_string());
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(slot) = acp_sessions.get(id) {
            return Ok(slot);
        }
        if Instant::now() >= deadline {
            return Err("The computer did not resume this conversation in time.".to_string());
        }
        thread::sleep(ACP_WAKE_POLL);
    }
}

/// 请求没写就照旧带调试 sidecar：桌面的调试面板靠它。
fn request_includes_debug(request: &serde_json::Value) -> bool {
    request["include_debug"].as_bool().unwrap_or(true)
}

fn omit_debug_sidecars(snapshot: &mut smelt_core::acp_session::ConversationSnapshot) {
    snapshot.tool_debug = None;
    snapshot.runtime_debug = None;
}

/// One-shot bounded history read used by mobile upward pagination. Unlike `acp_watch`,
/// this does not register a watcher and therefore cannot leak a long-lived connection.
pub(crate) fn handle_acp_snapshot(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let Some(slot) = acp_sessions.get(id) else {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": false, "error": "ACP 会话不存在" }),
        );
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
    let include_debug = request_includes_debug(v);
    // 挂起的会话镜像只剩当前回合。往上翻到镜像之前，就得先拉起宿主问它要。
    let needs_host = acp_sessions
        .with_current(id, &slot, |session| {
            let reduced = session.reduced.lock().unwrap();
            let end = before.min(reduced.entries_total());
            end.saturating_sub(limit) < reduced.entries_base() && session.idle.is_suspended()
        })
        .unwrap_or(false);
    if needs_host
        && let Err(error) =
            crate::acp_idle::wake_suspended_acp_session(id, &slot, acp_sessions, subscribers)
    {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": false, "error": error }),
        );
        return;
    }
    let page = acp_sessions.with_current(id, &slot, |session| {
        let _output_gate = session.output_gate.lock().unwrap();
        let reduced = session.reduced.lock().unwrap();
        let end = before.min(reduced.entries_total());
        let start = end.saturating_sub(limit);
        if start < reduced.entries_base()
            && let Some(host) = session.hosted_handle.lock().unwrap().clone()
        {
            return HistoryRead::AskHost { host, end };
        }
        let mut snapshot = reduced.to_snapshot_range(false, start, end);
        snapshot.runtime_debug = None;
        snapshot.snapshot_revision = session.snapshot_revision.load(Ordering::SeqCst);
        drop(reduced);
        if !include_debug {
            omit_debug_sidecars(&mut snapshot);
        }
        set_conversation_snapshot(session, &mut snapshot);
        HistoryRead::Ready(Box::new(snapshot))
    });
    let Some(page) = page else {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": false, "error": "ACP 会话不存在" }),
        );
        return;
    };
    let snapshot = match page {
        HistoryRead::Ready(snapshot) => *snapshot,
        HistoryRead::AskHost { host, end } => match host.request_history_page(end, limit) {
            Ok(mut snapshot) => {
                snapshot.runtime_debug = None;
                snapshot.snapshot_revision = slot.value.snapshot_revision.load(Ordering::SeqCst);
                set_conversation_snapshot(&slot.value, &mut snapshot);
                snapshot
            }
            Err(error) => {
                let _ = smelt_runtime_client::protocol::write_json_line(
                    &mut conn,
                    &serde_json::json!({ "ok": false, "error": error }),
                );
                return;
            }
        },
    };
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut conn,
        &serde_json::json!({ "snapshot": snapshot }),
    );
}

enum HistoryRead {
    Ready(Box<smelt_core::acp_session::ConversationSnapshot>),
    AskHost {
        host: acp_runtime_host::HostedConversationHandle,
        end: usize,
    },
}

/// 按需列出这场对话实际加载的技能。
///
/// 故意不塞进会话快照：快照每次状态变化都要推给所有观察者，而技能得扫一遍
/// 磁盘；两者的频率和代价对不上。手机打开对话时问一次即可，和桌面每次渲染
/// 输入栏重算是同一份来源（`loaded_skills_for_launch`）。
pub(crate) fn handle_acp_skills(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let launch = acp_sessions.get(id).and_then(|slot| {
        acp_sessions.with_current(id, &slot, |session| {
            (
                session.launch_spec.lock().unwrap().clone(),
                session.cwd.clone(),
            )
        })
    });
    let Some((Some(launch), cwd)) = launch else {
        // 会话没在运行（已结束 / 仅历史）就没有「这场对话加载了什么」可言。这不是错误，
        // 回 supported=false 让客户端收起入口，而不是弹一个用户无法处理的报错。
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": true, "supported": false, "skills": [] }),
        );
        return;
    };
    // 技能是 Pi 的概念。别家 agent 如实回 `supported: false`，让客户端不画这个入口，
    // 而不是画一个永远空的列表。
    let agent = smelt_core::agent_kind::ConversationAgentKind::from_command_loose(&launch.command);
    let supported = agent == Some(smelt_core::agent_kind::ConversationAgentKind::Pi);
    let skills = if supported {
        smelt_core::pi_plugin_catalog::loaded_skills_for_launch(
            &launch,
            cwd.as_deref().map(std::path::Path::new),
        )
    } else {
        Vec::new()
    };
    let skills: Vec<_> = skills
        .into_iter()
        .map(|skill| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
            })
        })
        .collect();
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut conn,
        &serde_json::json!({
            "ok": true,
            "agent": agent.map(|kind| kind.id()),
            "supported": supported,
            "skills": skills,
        }),
    );
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
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
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
    let _ = smelt_runtime_client::protocol::write_json_line(&mut c, &response);
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
    relaunch_recorded_acp_runtime(id, &slot, acp_sessions, subscribers)
}

/// 按槽位里记录的启动规格重新拉起 runtime，带上自己的历史身份。调用方必须持有
/// 生命周期锁并确认槽位仍是当前的。重启和闲置唤醒共用。
pub(crate) fn relaunch_recorded_acp_runtime(
    id: &str,
    slot: &Arc<AcpSlot<AcpSession>>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    let sess = &slot.value;
    let Some(launch) = sess.launch_spec.lock().unwrap().clone() else {
        return Err("no launch spec recorded for this session yet");
    };
    let resume_id = {
        let reduced = sess.reduced.lock().unwrap();
        resolve_resume_id(id, None, known_acp_resume_id(&reduced), true)
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
        slot,
        id,
        launch,
        resume_id,
        None,
        None,
        Arc::clone(acp_sessions),
        subscribers,
        false,
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

/// 交接不带走已提交的消息正文。那些在 agent 自己的会话文件里，新进程重读。
/// 这里只留相位、身份，以及还没写完的这一轮。
fn conversation_snapshot_for_handoff(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> smelt_core::acp_session::ConversationSnapshot {
    let (history_len, end) = reduced.open_turn_range();
    let mut snapshot = reduced.to_snapshot_range(false, history_len, end);
    snapshot.entries_offset = 0;
    snapshot.entries_total = snapshot.entries.len();
    snapshot.history_omitted = true;
    snapshot.runtime_debug = None;
    snapshot.tool_debug = None;
    snapshot
}

/// 交接 v2 typed 收集：与旧版同锁、同过滤、同 retire 语义，只换 typed 输出。
/// 旧 JSON 版已删：v2 写端不再产文件，legacy 文件读端只认旧二进制写的文件。
/// 交接回滚：[`collect_acp_handoff_typed`] 停下的宿主读端从停手处接着读。
/// 控制连接全程没动，宿主和镜像都不用重开，界面也就不会重连。
pub(crate) fn resume_acp_after_handoff_rollback(acp_sessions: &AcpSessions) {
    for (_, slot) in &acp_sessions.snapshot() {
        if let Some(host) = slot.value.hosted_handle.lock().unwrap().as_ref() {
            host.resume_after_handoff_rollback();
        }
    }
}

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
        // 先停读端再取 fd。读线程还在跑时把 fd 交出去，两个进程会从一行中间撕开。
        let host = sess.hosted_handle.lock().unwrap().clone();
        if let Some(host) = host {
            let pending_host_bytes = match host.take_for_handoff() {
                Ok(bytes) => bytes,
                Err(error) => {
                    crate::dlog(&format!("acp 交接停不下宿主读端 {id}：{error}"));
                    continue;
                }
            };
            let Some(host_fd) = host.control_fd() else {
                crate::dlog(&format!("acp 交接拿不到宿主控制连接 {id}"));
                continue;
            };
            let host_pid = host.pid();
            let provider_pid = host.provider_pid();
            let host_snapshot_revision = sess.host_snapshot_revision.load(Ordering::SeqCst);
            let launch = recorded_handoff_launch(sess);
            let (agent_mcp, agent_token) = {
                let state = sess.state.lock().unwrap();
                (state.agent_mcp, state.agent_token.clone())
            };
            let mut snapshot = conversation_snapshot_for_handoff(&sess.reduced.lock().unwrap());
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
                runtime_spec_fingerprint: sess.runtime_spec_fingerprint.lock().unwrap().clone(),
                conversation_binding: sess.conversation_binding.lock().unwrap().clone(),
                snapshot,
                pending_host_bytes,
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
                conversation_snapshot_for_handoff(&reduced),
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
            // hosted 与 direct 共用同一回合门槛。独立宿主让 mid-turn handoff 可恢复，
            // 但恢复能力不能成为正常升级主动打断活跃回合的理由。
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
                    && smelt_core::acp_chat::has_unfinished_tool_call(reduced.entries());
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

#[cfg(test)]
mod snapshot_batch_tests {
    use super::coalesce_snapshot_offset;

    #[test]
    fn replay_burst_collapses_to_one_full_snapshot() {
        // HistoryReplayStarted 是 0，后面每条历史都是递增游标。必须整批从 0 重发，
        // 不能为每一条各占一份快照。
        let offsets = std::iter::once(Some(0)).chain((1..78).map(Some));
        assert_eq!(coalesce_snapshot_offset(offsets), Some(0));
    }

    #[test]
    fn incremental_burst_starts_at_the_earliest_change() {
        assert_eq!(
            coalesce_snapshot_offset([Some(12), None, Some(4), Some(9)]),
            Some(4)
        );
        assert_eq!(coalesce_snapshot_offset([None, None]), None);
    }
}

#[cfg(test)]
mod snapshot_link_tests {
    use super::*;
    use std::io::Read;
    /// 历史重放会在写线程还堵在 socket 上时连续 mark。旧邮箱把这些快照排成字节
    /// 队列，超限就摘连接；现在必须还连着，并且只序列化一份覆盖全部 mark 的快照。
    #[test]
    fn direct_handoff_snapshot_omits_committed_history() {
        let mut state = smelt_core::acp_session::AcpSessionState::default();
        state.phase = smelt_core::daemon_state::DaemonPhase::Idle;
        *state.entries_mut() = vec![smelt_core::acp_chat::AcpEntry::User("以前".into())];
        let snapshot = conversation_snapshot_for_handoff(&state);
        assert!(
            snapshot.entries.is_empty(),
            "空闲会话的已提交消息不能进交接文件"
        );

        state.phase = smelt_core::daemon_state::DaemonPhase::Thinking;
        state.turn_started_at_ms = Some(1);
        state
            .entries_mut()
            .push(smelt_core::acp_chat::AcpEntry::User("现在".into()));
        let snapshot = conversation_snapshot_for_handoff(&state);
        assert!(snapshot.history_omitted);
        assert_eq!(snapshot.entries.len(), 1);
        assert!(matches!(
            &snapshot.entries[0],
            smelt_core::acp_chat::AcpEntry::User(text) if text == "现在"
        ));
    }

    #[test]
    fn burst_while_writer_is_blocked_coalesces_and_stays_connected() {
        let slot = Arc::new(AcpSlot {
            lifecycle: Mutex::new(()),
            value: make_acp_session("acp-burst", None, None, None),
        });
        let (mut server, mut client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let chunk = vec![b'x'; 8 * 1024];
        loop {
            match server.write(&chunk) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("填满 socket 失败: {error}"),
            }
        }
        server.set_nonblocking(false).unwrap();
        server.set_write_timeout(None).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let (_, mirror) = full_frame(&serde_json::json!({ "snapshot": { "entries": [] } }));
        let link = acp_snapshot_link_for_slot(
            &slot,
            server,
            b"{\"snapshot\":\"initial\"}\n".to_vec(),
            mirror,
            0,
            "acp-burst",
            "acp-client",
            SnapshotProjection::FULL,
        )
        .unwrap();
        slot.value.out.lock().unwrap().client = Some(link);

        // 写线程卡在首包上。这段时间里的 mark 必须并成一份，而不是把连接摘掉。
        thread::sleep(Duration::from_millis(50));
        for index in 0..40 {
            slot.value.reduced.lock().unwrap().entries_mut().push(
                smelt_core::acp_chat::AcpEntry::User(format!("marker-{index}")),
            );
            push_acp_snapshot_since(&slot.value, true, Some(0));
        }
        {
            let out = slot.value.out.lock().unwrap();
            let link = out.client.as_ref().unwrap();
            assert!(!link.has_failed(), "快照堆积不应摘掉 ACP 连接");
            assert_eq!(link.snapshots_sent(), 0, "写线程堵住时不应提前序列化");
        }

        let mut received = Vec::new();
        let mut buf = [0u8; 8 * 1024];
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !received.windows(9).any(|w| w == b"marker-39") {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&buf[..n]),
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    if received.windows(9).any(|w| w == b"marker-39") {
                        break;
                    }
                }
                Err(error) => panic!("读快照失败: {error}"),
            }
        }
        let text = String::from_utf8_lossy(&received);
        assert!(
            text.contains("marker-39"),
            "合并后的快照应包含最后一条，实际读到 {} 字节",
            received.len()
        );
        assert!(
            text.contains("marker-0"),
            "合并快照应从最早的 offset 覆盖，不能只剩尾巴"
        );
        let snapshots = text.matches("\"snapshot\"").count();
        assert!(
            snapshots <= 2,
            "堵住期间的 40 次 mark 应并成一份更新，实际 snapshot 行约 {snapshots} 份:\n{text}"
        );
        let sent = slot
            .value
            .out
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .snapshots_sent();
        assert_eq!(sent, 1, "写线程恢复后只应序列化一次");
    }
}
