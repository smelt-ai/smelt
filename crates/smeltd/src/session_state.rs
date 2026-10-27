//! 会话状态通道与 hook/OSC 相位归约。
//!
//! `SessionState` / `Phase` 是守护里几乎所有模块的扇出中心。从 `main.rs` 整块搬出，
//! 让 ACP / 目录 / 总线依赖这一层而不是 crate 根，避免后续再拆时形成 sibling 循环。

use std::sync::atomic::{AtomicU64, Ordering};

use smelt_core::agent_event::{
    AGENT_EVENT_MIN_VERSION, AGENT_EVENT_VERSION, AgentEvent, AgentEventKind, Occupancy,
};
use smelt_core::agent_kind::TerminalAgentKind;

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 会话状态通道（见 docs/notification-architecture.md）。只有 hook 事件归约
/// （`agent_event` op）和 ACP 投影能改变 phase。OSC 不是会话名，BEL 只作信息
/// 通知。schema 定死，字段不够用再加，不删不改类型——远程端/GUI 都按这份 schema 解码。
#[derive(Clone, serde::Serialize)]
pub(crate) struct SessionState {
    pub(crate) id: String,
    /// 仅 daemon 内部使用的 runtime 代际；序列化时不暴露给旧客户端。revision 只能
    /// 解决同一实例的乱序，实例被删除或同 ID 重建后还需要它拒绝迟到事件。
    #[serde(skip)]
    pub(crate) instance: u64,
    pub(crate) cwd: Option<String>,
    /// claude / codex / copilot（来自 spawn 时的 launch 命令）。
    pub(crate) launch: Option<String>,
    /// 结构化 hook 实际上报并经终端 agent 注册表确认的 provider。它描述当前在
    /// shell 中运行的 agent，与不可变的启动命令 `launch` 分开。
    pub(crate) provider: Option<String>,
    /// 会话里的 agent 进程向守护证明身份用的令牌（智能体记忆等 daemon op 要它）。只进
    /// 受控子进程环境和 handoff，不进 list/subscribe，其它 agent 知道 session id 也冒充不了。
    #[serde(skip)]
    pub(crate) agent_token: String,
    /// 会话名。非空的 provider 标题（`SessionTitleChanged`，或 ACP 的
    /// `session_title`）直接替换。还没有名字时，首条能抽出标题的 prompt 写一次。
    /// 已有名字不被后来的 prompt 改掉。换对话时清空。OSC 0/2 不是会话名。
    pub(crate) title: Option<String>,
    /// 该终端绑定的 provider 会话 id（Copilot `sessionId` 等）。由每条 hook 携带，
    /// 因此 TUI 内部 `/resume`、退出重开都会自动改写，不需要另外的握手或代次推断。
    /// None = 这家 provider 的 hook 不带 id，或还没有事件到达。
    ///
    /// 它是「侧栏这条会话对应哪段历史存档」的唯一可信来源：重命名据此写入
    /// 历史标题覆盖层，标题解析、复制会话 id 也据此跟随。CLI 进程退出
    /// （`SessionEnded`）之后 id 仍留着，桌面退出后还要对上这段对话。
    /// 能不能续接不看这里，看 [`Self::cli_exited`]。
    #[serde(default)]
    pub(crate) conversation_id: Option<String>,
    /// CLI 进程已经退出、终端回到 shell。`SessionEnded` 置位；之后又有
    /// `SessionStarted`，或相位门控放行的活事件，再清掉。对话 id 不受影响。
    /// 为 true 时这条终端不能 `--resume`（见 `resume_of`）。不发 SessionEnd 的
    /// provider 一直是 false。
    #[serde(default)]
    pub(crate) cli_exited: bool,
    pub(crate) phase: Phase,
    /// unix 秒。「空转多久了」靠它算——作战地图用。
    pub(crate) phase_since: u64,
    /// 在问什么——远程遥控的命脉，Phase 6 的 action 门闩靠它判断能不能安全写入。
    pub(crate) pending_question: Option<String>,
    /// 累计花费口径（各轮 cache_read 都加了），**不是**上下文占用，不能当余量分母。
    /// 见 session_history::SessionSummary.total_tokens；目前没接，先占位。
    pub(crate) tokens_used: Option<u64>,
    /// 撞车预警要用；目前没接（见 smelt_git::GitStatusData），先占位。
    pub(crate) branch: Option<String>,
    pub(crate) dirty_files: Vec<String>,
    /// 本 daemon 进程内全局单调递增的状态版本。状态锁只保证修改互斥，修改后的
    /// 广播仍可能被线程调度倒序；订阅端用它拒绝旧快照覆盖新事实。
    pub(crate) revision: u64,
    pub(crate) updated_at: u64,
    /// 已收到 smelt-notify 的结构化 hook 事件。SessionStart 就会置位，用来关掉
    /// OSC/BEL 双通知；单独还不表示相位已经权威。
    pub(crate) structured_events: bool,
    /// 已收到回合级 hook / ACP 生命周期事件。
    #[serde(default)]
    pub(crate) turn_events: bool,
    /// 最近收到的归一化 hook 协议版本；尚未收到结构化事件时为 None。
    pub(crate) agent_event_version: Option<u32>,
    /// 等待状态的来源身份，仅供 reducer 防止无关子任务/工具完成误清等待。
    #[serde(skip)]
    pub(crate) active_blocker: Option<AgentBlocker>,
    /// 本进程是否仍持有 PTY/ACP 运行时。与客户端是否 attach（list 里的 connected）
    /// 正交。崩溃恢复的幽灵会话为 false；活会话缺省 true。
    pub(crate) runtime: bool,
}

impl SessionState {
    /// ACP 会话的状态。相位全由 ACP 投影给出（`update_acp_daemon_state`），从创建那一刻就是权威的。
    /// 新建和升级交接接管都走这里：用 `..Default::default()` 拼会丢掉 `turn_events`，
    /// `effective_phase()` 随之把运行中 / 已完成一律降成 Idle。
    pub(crate) fn acp(
        id: String,
        instance: u64,
        cwd: Option<String>,
        launch: Option<String>,
        agent_token: String,
    ) -> Self {
        Self {
            id,
            instance,
            cwd,
            launch,
            agent_token,
            structured_events: true,
            turn_events: true,
            ..Self::default()
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            id: String::new(),
            instance: 0,
            cwd: None,
            launch: None,
            provider: None,
            agent_token: String::new(),
            title: None,
            conversation_id: None,
            cli_exited: false,
            phase: Phase::Idle,
            phase_since: 0,
            pending_question: None,
            tokens_used: None,
            branch: None,
            dirty_files: Vec::new(),
            revision: 0,
            updated_at: 0,
            structured_events: false,
            turn_events: false,
            agent_event_version: None,
            active_blocker: None,
            runtime: true,
        }
    }
}

static NEXT_STATE_REVISION: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_INSTANCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn bump_state_revision(state: &mut SessionState) {
    state.revision = NEXT_STATE_REVISION.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn next_session_instance() -> u64 {
    NEXT_SESSION_INSTANCE.fetch_add(1, Ordering::SeqCst)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AgentBlocker {
    pub(crate) tool_use_id: Option<String>,
    pub(crate) agent_id: Option<String>,
    pub(crate) tool_name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
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

impl Phase {
    fn is_dead(self) -> bool {
        matches!(self, Self::Dead)
    }

    fn is_finished(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }

    /// 回合已关闭：迟到的完成不能重开。新工作仍可重开（Dead 除外）。
    fn turn_closed(self) -> bool {
        self.is_finished() || self.is_dead()
    }

    fn is_waiting(self) -> bool {
        matches!(self, Self::AwaitingApproval | Self::WaitingForUser)
    }
}

/// 谁在改 [`SessionState::phase`]。生产路径不得直接赋值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhaseSource {
    /// Claude / Codex / Copilot / Antigravity 的归一化 hook。
    Hook,
    /// ACP 归约结果投影到会话相位。
    AcpProjection,
}

/// 会话相位的唯一写入入口。返回是否真的改了 phase。
/// 来源仅做归因，不做门控：门控已收进 [`Occupancy::allows_phase_change`]。
pub(crate) fn commit_session_phase(
    state: &mut SessionState,
    _source: PhaseSource,
    next: Phase,
) -> bool {
    if state.phase == next {
        return false;
    }
    state.phase = next;
    state.phase_since = now_unix();
    true
}

fn event_matches_blocker(blocker: &AgentBlocker, event: &AgentEvent) -> bool {
    if let Some(expected) = blocker.tool_use_id.as_deref() {
        return event.tool_use_id.as_deref() == Some(expected);
    }
    if let Some(expected) = blocker.agent_id.as_deref() {
        return event.agent_id.as_deref() == Some(expected);
    }
    if let Some(expected) = blocker.tool_name.as_deref() {
        return event.tool_name.as_deref() == Some(expected);
    }
    true
}

/// 对话绑定的切换结果。标题清空只发生在 `Switched` 一处。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingTransition {
    Ignored,
    Same,
    Switched,
}

/// 归约当前对话身份。子任务事件不参与：它们可能携带子会话自己的 id，会把
/// 主对话的绑定冲掉。
///
/// 换对话时（id 变了）清掉上一段对话的会话名。
/// 留着就会让侧栏顶着上一段对话的名字。
///
/// 还没有对话 id 时的第一次绑定不清名字：`None` 变成 `Some` 不是换对话。
/// CLI 退出只把 `cli_exited` 置上，id 留着，所以退出后再起一段新对话仍然走
/// 「id 变了」，名字会让位；再 `--resume` 同一段则是 `Same`，名字留着。
fn switch_conversation(state: &mut SessionState, event: &AgentEvent) -> BindingTransition {
    if matches!(
        event.kind,
        AgentEventKind::SubagentStarted | AgentEventKind::SubagentStopped
    ) {
        return BindingTransition::Ignored;
    }
    let Some(id) = event
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return BindingTransition::Ignored;
    };
    if state.conversation_id.as_deref() == Some(id) {
        return BindingTransition::Same;
    }
    if state.conversation_id.is_some() {
        state.title = None;
    }
    state.conversation_id = Some(id.to_string());
    BindingTransition::Switched
}

/// 还没有会话名时，用这条 prompt 兜底。已有名字保持不变。
/// 显式改名只走 `SessionTitleChanged`。
fn note_prompt_fallback(state: &mut SessionState, event: &AgentEvent) {
    if state.title.is_some() {
        return;
    }
    if let Some(title) = event
        .conversation_title
        .as_deref()
        .and_then(smelt_core::session_title::prompt_title)
    {
        state.title = Some(title);
    }
}

/// 将 provider 适配器产出的统一事件归约进会话。
///
/// 相位只按 [`Occupancy`] 改：元数据不动，OpenTurn 打开回合，ResumeWork 在
/// 非 Dead 时重开，Progress/Wait/Finish 不能重开已关闭回合。等待粘性由
/// `follow_matched_wait` 表达，不再按 kind 写例外。
pub(crate) fn apply_agent_event(state: &mut SessionState, event: &AgentEvent) -> bool {
    if !(AGENT_EVENT_MIN_VERSION..=AGENT_EVENT_VERSION).contains(&event.version) {
        return false;
    }

    let previous_phase = state.phase;
    let occupancy = event.kind.occupancy();
    let waiting = state.phase.is_waiting();
    let matching_blocker = state
        .active_blocker
        .as_ref()
        .is_none_or(|blocker| event_matches_blocker(blocker, event));
    let allow = occupancy.allows_phase_change(
        state.phase.is_dead(),
        state.phase.turn_closed(),
        state.phase.is_finished(),
        waiting,
        matching_blocker,
    );

    if let Some(provider) = TerminalAgentKind::from_id(event.provider.trim()) {
        state.provider = Some(provider.id().to_string());
    }
    let _ = switch_conversation(state, event);
    // 进程退出和对话身份分开。SessionEnded 无条件记下退出，写在相位门控之前：
    // 回合已经 Finished 时 EndProcess 不改相位，退出标记一样要落下。之后
    // `SessionStarted`，或相位门控放行的活事件，说明进程又在跑，再清掉。
    // 回合已关闭之后的迟到进度不放行，也不把退出标记抹掉。
    if occupancy == Occupancy::EndProcess {
        state.cli_exited = true;
    } else if event.kind == AgentEventKind::SessionStarted || allow {
        state.cli_exited = false;
    }

    match occupancy {
        Occupancy::Metadata => {
            if event.kind == AgentEventKind::SessionTitleChanged
                && let Some(title) = smelt_core::session_title::provider_session_title(
                    event.conversation_title.as_deref(),
                )
            {
                state.title = Some(title.to_string());
            }
        }
        Occupancy::OpenTurn => {
            let _ = commit_session_phase(state, PhaseSource::Hook, Phase::Thinking);
            state.pending_question = None;
            state.active_blocker = None;
            note_prompt_fallback(state, event);
        }
        Occupancy::ResumeWork {
            follow_matched_wait,
        } => {
            if allow {
                let _ = commit_session_phase(state, PhaseSource::Hook, Phase::ExecutingTool);
                state.pending_question = event.message.clone();
                if follow_matched_wait {
                    state.active_blocker = None;
                }
            }
        }
        Occupancy::Progress {
            follow_matched_wait,
        } => {
            if allow {
                let _ = commit_session_phase(state, PhaseSource::Hook, Phase::Thinking);
                if follow_matched_wait {
                    state.pending_question = event.message.clone();
                    state.active_blocker = None;
                } else {
                    state.pending_question = None;
                }
            }
        }
        Occupancy::Wait { approval } => {
            if allow {
                let next = if approval {
                    Phase::AwaitingApproval
                } else {
                    Phase::WaitingForUser
                };
                let _ = commit_session_phase(state, PhaseSource::Hook, next);
                state.pending_question = event.message.clone().or_else(|| event.tool_name.clone());
                state.active_blocker = Some(AgentBlocker {
                    tool_use_id: event.tool_use_id.clone(),
                    agent_id: event.agent_id.clone(),
                    tool_name: event.tool_name.clone(),
                });
            }
        }
        Occupancy::Finish { failed } => {
            if allow {
                let next = if failed {
                    Phase::Failed
                } else {
                    Phase::Succeeded
                };
                let _ = commit_session_phase(state, PhaseSource::Hook, next);
                state.pending_question = event.message.clone();
                state.active_blocker = None;
            }
        }
        Occupancy::EndProcess => {
            // 退出标记在上面已经落下，对话 id 留着。能不能续接看 `cli_exited`
            // （`resume_of`），不靠把身份清掉。provider 和 title 同样留着。
            // 不发 SessionEnd 的 provider 走不到这里，见 `terminal_resume`。
            if allow {
                let _ = commit_session_phase(state, PhaseSource::Hook, Phase::Dead);
                state.pending_question = None;
                state.active_blocker = None;
            }
            state.agent_token = uuid::Uuid::new_v4().simple().to_string();
        }
    }

    state.structured_events = true;
    if occupancy.records_turn() {
        state.turn_events = true;
    }
    state.agent_event_version = Some(event.version);
    if state.phase != previous_phase {
        state.phase_since = now_unix();
    }
    state.updated_at = now_unix();
    bump_state_revision(state);
    true
}

#[cfg(test)]
mod agent_event_reducer_tests {
    use super::*;

    fn event(kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("codex", kind)
    }

    /// 对话身份跟着 hook 走：同一个 PTY 换一段对话，id 变了就要连带丢掉上一段
    /// 对话的标题，否则侧栏会顶着旧名字。
    #[test]
    fn switching_conversation_replaces_the_binding_and_drops_the_old_title() {
        let mut state = SessionState::default();
        let mut started = event(AgentEventKind::SessionStarted);
        started.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &started);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));

        let mut prompt = event(AgentEventKind::PromptSubmitted);
        prompt.conversation_id = Some("conv-1".into());
        prompt.conversation_title = Some("先问一句".into());
        apply_agent_event(&mut state, &prompt);

        let mut named = event(AgentEventKind::SessionTitleChanged);
        named.conversation_id = Some("conv-1".into());
        named.conversation_title = Some("Compare Pi and Codex".into());
        apply_agent_event(&mut state, &named);

        let mut same = event(AgentEventKind::ToolFinished);
        same.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &same);
        assert_eq!(
            state.title.as_deref(),
            Some("Compare Pi and Codex"),
            "同一段对话的后续事件不该清掉标题"
        );

        let mut switched = event(AgentEventKind::SessionStarted);
        switched.conversation_id = Some("conv-2".into());
        apply_agent_event(&mut state, &switched);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-2"));
        assert_eq!(state.title, None, "上一段对话的标题必须让位给新对话");
    }

    /// 子任务事件可能带子会话自己的 id；让它改写绑定会把主对话冲掉。
    #[test]
    fn subagent_events_never_rebind_the_conversation() {
        let mut state = SessionState::default();
        let mut started = event(AgentEventKind::SessionStarted);
        started.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &started);

        let mut subagent = event(AgentEventKind::SubagentStarted);
        subagent.conversation_id = Some("sub-1".into());
        apply_agent_event(&mut state, &subagent);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));
    }

    /// 不带 id 的 provider 不能把已有绑定清掉——认不出来就维持现状。
    #[test]
    fn events_without_an_id_keep_the_existing_binding() {
        let mut state = SessionState::default();
        let mut started = event(AgentEventKind::SessionStarted);
        started.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &started);

        apply_agent_event(&mut state, &event(AgentEventKind::ToolFinished));
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));
    }

    #[test]
    fn normalized_lifecycle_reaches_waiting_then_success() {
        let mut state = SessionState::default();
        assert!(apply_agent_event(
            &mut state,
            &event(AgentEventKind::PromptSubmitted)
        ));
        assert_eq!(state.phase, Phase::Thinking);

        let mut waiting = event(AgentEventKind::InputRequested);
        waiting.tool_use_id = Some("question-1".into());
        waiting.message = Some("选一个".into());
        apply_agent_event(&mut state, &waiting);
        assert_eq!(state.phase, Phase::WaitingForUser);
        assert_eq!(state.pending_question.as_deref(), Some("选一个"));

        let mut answered = event(AgentEventKind::ToolFinished);
        answered.tool_use_id = Some("question-1".into());
        apply_agent_event(&mut state, &answered);
        assert_eq!(state.phase, Phase::Thinking);

        apply_agent_event(&mut state, &event(AgentEventKind::TurnSucceeded));
        assert_eq!(state.phase, Phase::Succeeded);
    }

    #[test]
    fn tool_finished_keeps_optional_progress_detail() {
        let mut state = SessionState::default();
        apply_agent_event(&mut state, &event(AgentEventKind::PromptSubmitted));
        let mut finished = event(AgentEventKind::ToolFinished);
        finished.message = Some("第 5 步 · Bash: cargo test".into());
        apply_agent_event(&mut state, &finished);
        assert_eq!(state.phase, Phase::Thinking);
        assert_eq!(
            state.pending_question.as_deref(),
            Some("第 5 步 · Bash: cargo test")
        );
    }

    #[test]
    fn unrelated_child_events_do_not_clear_a_sticky_wait() {
        let mut state = SessionState::default();
        let mut waiting = event(AgentEventKind::ApprovalRequested);
        waiting.tool_use_id = Some("approval-1".into());
        waiting.agent_id = Some("lead".into());
        apply_agent_event(&mut state, &waiting);

        let mut child_finished = event(AgentEventKind::ToolFinished);
        child_finished.tool_use_id = Some("child-tool".into());
        child_finished.agent_id = Some("child".into());
        apply_agent_event(&mut state, &child_finished);
        assert_eq!(state.phase, Phase::AwaitingApproval);

        let mut child_stopped = event(AgentEventKind::SubagentStopped);
        child_stopped.agent_id = Some("child".into());
        apply_agent_event(&mut state, &child_stopped);
        assert_eq!(state.phase, Phase::AwaitingApproval);
    }

    #[test]
    fn tool_name_keeps_wait_sticky_when_provider_has_no_call_id() {
        let mut state = SessionState::default();
        let mut waiting = event(AgentEventKind::InputRequested);
        waiting.tool_name = Some("request_user_input".into());
        apply_agent_event(&mut state, &waiting);

        let mut unrelated = event(AgentEventKind::ToolFinished);
        unrelated.tool_name = Some("shell".into());
        apply_agent_event(&mut state, &unrelated);
        assert_eq!(state.phase, Phase::WaitingForUser);

        let mut answered = event(AgentEventKind::ToolFinished);
        answered.tool_name = Some("request_user_input".into());
        apply_agent_event(&mut state, &answered);
        assert_eq!(state.phase, Phase::Thinking);
    }

    #[test]
    fn prompt_and_terminal_events_always_clear_a_wait() {
        for kind in [
            AgentEventKind::PromptSubmitted,
            AgentEventKind::TurnSucceeded,
            AgentEventKind::TurnFailed,
            AgentEventKind::SessionEnded,
        ] {
            let mut state = SessionState::default();
            let mut waiting = event(AgentEventKind::InputRequested);
            waiting.tool_use_id = Some("question-1".into());
            apply_agent_event(&mut state, &waiting);
            apply_agent_event(&mut state, &event(kind));
            assert!(!matches!(
                state.phase,
                Phase::AwaitingApproval | Phase::WaitingForUser
            ));
            assert!(state.active_blocker.is_none());
        }
    }

    #[test]
    fn late_completions_do_not_reopen_a_finished_turn() {
        for terminal in [AgentEventKind::TurnSucceeded, AgentEventKind::TurnFailed] {
            for late in [
                AgentEventKind::ToolFinished,
                AgentEventKind::ToolFailed,
                AgentEventKind::ApprovalRequested,
                AgentEventKind::InputRequested,
                AgentEventKind::SubagentStopped,
                AgentEventKind::TurnSucceeded,
                AgentEventKind::TurnFailed,
            ] {
                let mut state = SessionState::default();
                apply_agent_event(&mut state, &event(terminal));
                let expected = state.phase;
                apply_agent_event(&mut state, &event(late));
                assert_eq!(state.phase, expected, "late event: {late:?}");

                apply_agent_event(&mut state, &event(AgentEventKind::PromptSubmitted));
                assert_eq!(state.phase, Phase::Thinking);
            }
        }
    }

    #[test]
    fn new_work_reopens_a_turn_after_a_premature_stop() {
        // 过早的 Stop / idle 不能把之后真正开始的工具钉死在空闲。
        // 迟到的「完成」仍不能重开；只有新工作边沿可以。
        for terminal in [AgentEventKind::TurnSucceeded, AgentEventKind::TurnFailed] {
            let mut by_tool = SessionState::default();
            apply_agent_event(&mut by_tool, &event(terminal));
            apply_agent_event(&mut by_tool, &event(AgentEventKind::ToolStarted));
            assert_eq!(
                by_tool.phase,
                Phase::ExecutingTool,
                "终态后的 ToolStarted 是新工作，必须重开回合"
            );

            let mut by_subagent = SessionState::default();
            apply_agent_event(&mut by_subagent, &event(terminal));
            apply_agent_event(&mut by_subagent, &event(AgentEventKind::SubagentStarted));
            assert_eq!(
                by_subagent.phase,
                Phase::ExecutingTool,
                "终态后的 SubagentStarted 同样是新工作"
            );
        }
    }

    #[test]
    fn session_start_never_changes_phase() {
        for phase in [
            Phase::Connecting,
            Phase::Idle,
            Phase::Thinking,
            Phase::ExecutingTool,
            Phase::AwaitingApproval,
            Phase::WaitingForUser,
            Phase::Succeeded,
            Phase::Failed,
            Phase::Dead,
        ] {
            let mut state = SessionState {
                phase,
                turn_events: !matches!(phase, Phase::Idle | Phase::Connecting),
                ..Default::default()
            };
            let turn_events = state.turn_events;
            apply_agent_event(&mut state, &event(AgentEventKind::SessionStarted));
            assert_eq!(state.phase, phase, "SessionStart 不是回合边沿：{phase:?}");
            assert_eq!(state.turn_events, turn_events);
            assert!(state.structured_events);
        }
    }

    #[test]
    fn session_end_preserves_a_finished_turn_and_dead_rejects_late_noise() {
        for terminal in [AgentEventKind::TurnSucceeded, AgentEventKind::TurnFailed] {
            let mut state = SessionState::default();
            apply_agent_event(&mut state, &event(terminal));
            let expected = state.phase;
            apply_agent_event(&mut state, &event(AgentEventKind::SessionEnded));
            assert_eq!(state.phase, expected);
        }

        let mut dead = SessionState::default();
        apply_agent_event(&mut dead, &event(AgentEventKind::SessionEnded));
        assert_eq!(dead.phase, Phase::Dead);
        apply_agent_event(&mut dead, &event(AgentEventKind::ToolFinished));
        assert_eq!(dead.phase, Phase::Dead);
        apply_agent_event(&mut dead, &event(AgentEventKind::ToolStarted));
        assert_eq!(dead.phase, Phase::Dead, "已结束的运行时不能被工具开始复活");
        apply_agent_event(&mut dead, &event(AgentEventKind::TurnSucceeded));
        assert_eq!(dead.phase, Phase::Dead);
        apply_agent_event(&mut dead, &event(AgentEventKind::PromptSubmitted));
        assert_eq!(dead.phase, Phase::Thinking);
    }

    /// CLI `/exit` 回到 shell：完成过的回合相位和对话身份都留着，只是进程已经退出，
    /// 停机不续接。零回合退出后再起另一段对话，身份换了，自定义标题让位。
    #[test]
    fn session_end_keeps_the_conversation_and_a_later_start_rebinds() {
        let mut state = SessionState {
            launch: Some("claude".into()),
            title: Some("这段对话".into()),
            ..Default::default()
        };
        let mut started = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        started.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &started);

        let mut done = AgentEvent::new("claude", AgentEventKind::TurnSucceeded);
        done.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &done);
        assert_eq!(state.phase, Phase::Succeeded);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));

        let mut ended = AgentEvent::new("claude", AgentEventKind::SessionEnded);
        ended.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &ended);
        assert_eq!(state.phase, Phase::Succeeded);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));
        assert!(state.cli_exited);
        assert_eq!(state.provider.as_deref(), Some("claude"));
        assert_eq!(state.title.as_deref(), Some("这段对话"));
        assert_eq!(state.launch.as_deref(), Some("claude"));

        // 回合已关闭，迟到的进度不放行，也不能把退出标记抹掉。
        let mut late = AgentEvent::new("claude", AgentEventKind::ToolFinished);
        late.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut state, &late);
        assert!(state.cli_exited);

        let mut again = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        again.conversation_id = Some("conv-2".into());
        apply_agent_event(&mut state, &again);
        assert_eq!(state.conversation_id.as_deref(), Some("conv-2"));
        assert!(!state.cli_exited);
        assert_eq!(state.phase, Phase::Succeeded);
        assert_eq!(state.title, None, "新对话不能顶着上一段的名字");

        let mut fresh = SessionState {
            launch: Some("claude".into()),
            title: Some("自定义标题".into()),
            ..Default::default()
        };
        let mut first = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        first.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut fresh, &first);
        assert_eq!(fresh.conversation_id.as_deref(), Some("conv-1"));
        assert!(!fresh.cli_exited);
        assert!(!fresh.turn_events);
        assert_eq!(fresh.title.as_deref(), Some("自定义标题"));

        let mut ended = AgentEvent::new("claude", AgentEventKind::SessionEnded);
        ended.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut fresh, &ended);
        assert_eq!(fresh.conversation_id.as_deref(), Some("conv-1"));
        assert!(fresh.cli_exited);
        assert_eq!(fresh.title.as_deref(), Some("自定义标题"));

        let mut resumed = fresh.clone();
        let mut same = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        same.conversation_id = Some("conv-1".into());
        apply_agent_event(&mut resumed, &same);
        assert_eq!(resumed.conversation_id.as_deref(), Some("conv-1"));
        assert!(!resumed.cli_exited);
        assert_eq!(resumed.title.as_deref(), Some("自定义标题"));

        let mut next = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        next.conversation_id = Some("conv-2".into());
        apply_agent_event(&mut fresh, &next);
        assert_eq!(fresh.conversation_id.as_deref(), Some("conv-2"));
        assert!(!fresh.cli_exited);
        assert_eq!(fresh.title, None);
    }

    #[test]
    fn session_start_marks_hooks_live_but_not_turn_authority() {
        let mut state = SessionState::default();
        apply_agent_event(&mut state, &event(AgentEventKind::SessionStarted));
        assert!(state.structured_events);
        assert!(!state.turn_events);
        assert_eq!(state.phase, Phase::Idle);

        apply_agent_event(&mut state, &event(AgentEventKind::PromptSubmitted));
        assert!(state.turn_events);
        assert_eq!(state.phase, Phase::Thinking);
    }

    #[test]
    fn prompt_sets_the_session_name_and_opens_a_turn() {
        for provider in ["codex", "claude", "copilot", "grok", "opencode"] {
            let mut state = SessionState::default();
            let mut prompt = AgentEvent::new(provider, AgentEventKind::PromptSubmitted);
            prompt.conversation_title = Some("修一下".into());
            assert!(apply_agent_event(&mut state, &prompt));
            assert_eq!(state.phase, Phase::Thinking);
            assert!(state.turn_events);
            assert_eq!(state.title.as_deref(), Some("修一下"));
        }
    }

    #[test]
    fn title_event_updates_metadata_without_touching_the_turn() {
        let mut state = SessionState {
            phase: Phase::ExecutingTool,
            pending_question: Some("Bash: cargo test".into()),
            ..Default::default()
        };
        let phase_since = state.phase_since;
        let mut title = AgentEvent::new("opencode", AgentEventKind::SessionTitleChanged);
        title.conversation_title = Some("修复 OpenCode 标题".into());

        assert!(apply_agent_event(&mut state, &title));
        assert_eq!(state.provider.as_deref(), Some("opencode"));
        assert_eq!(state.title.as_deref(), Some("修复 OpenCode 标题"));
        assert_eq!(state.phase, Phase::ExecutingTool);
        assert_eq!(state.phase_since, phase_since);
        assert_eq!(state.pending_question.as_deref(), Some("Bash: cargo test"));
        assert!(state.structured_events);
        assert!(!state.turn_events);

        let mut blank = AgentEvent::new("opencode", AgentEventKind::SessionTitleChanged);
        blank.conversation_title = Some("   ".into());
        assert!(apply_agent_event(&mut state, &blank));
        assert_eq!(state.title.as_deref(), Some("修复 OpenCode 标题"));
        assert_eq!(state.phase, Phase::ExecutingTool);
    }

    #[test]
    fn every_registered_terminal_provider_is_detected_from_hooks() {
        for provider in TerminalAgentKind::ALL {
            let mut state = SessionState::default();
            apply_agent_event(
                &mut state,
                &AgentEvent::new(provider.id(), AgentEventKind::SessionStarted),
            );
            assert_eq!(state.provider.as_deref(), Some(provider.id()));
        }

        let mut state = SessionState::default();
        apply_agent_event(
            &mut state,
            &AgentEvent::new("unknown-provider", AgentEventKind::SessionStarted),
        );
        assert_eq!(state.provider, None);
    }

    #[test]
    fn unsupported_event_version_is_ignored() {
        let mut state = SessionState::default();
        let mut future = event(AgentEventKind::TurnFailed);
        future.version = AGENT_EVENT_VERSION + 1;
        assert!(!apply_agent_event(&mut state, &future));
        assert_eq!(state.phase, Phase::Idle);
        assert!(!state.structured_events);
    }

    fn prompt_with_title(title: &str) -> AgentEvent {
        let mut event = event(AgentEventKind::PromptSubmitted);
        event.conversation_title = Some(title.to_string());
        event
    }

    #[test]
    fn provider_title_replaces_prompt_and_later_prompts_do_not_rename() {
        let mut state = SessionState {
            cwd: Some("/Users/me/code/smelt".into()),
            launch: Some("codex".into()),
            ..Default::default()
        };
        apply_agent_event(&mut state, &prompt_with_title("第一条需求"));
        assert_eq!(state.title.as_deref(), Some("第一条需求"));

        let mut named = event(AgentEventKind::SessionTitleChanged);
        named.conversation_title = Some("用户显式命名".into());
        apply_agent_event(&mut state, &named);
        assert_eq!(state.title.as_deref(), Some("用户显式命名"));

        apply_agent_event(&mut state, &prompt_with_title("第二条需求"));
        assert_eq!(state.title.as_deref(), Some("用户显式命名"));
    }

    #[test]
    fn later_prompt_does_not_replace_an_existing_title() {
        let mut state = SessionState {
            title: Some("已恢复的名字".into()),
            ..Default::default()
        };
        apply_agent_event(&mut state, &prompt_with_title("新的第一条"));
        assert_eq!(state.title.as_deref(), Some("已恢复的名字"));
    }

    #[test]
    fn established_provider_title_is_not_replaced_by_prompt_fallback() {
        let mut state = SessionState {
            cwd: Some("/Users/me/code/smelt".into()),
            launch: Some("codex".into()),
            ..Default::default()
        };
        let mut named = event(AgentEventKind::SessionTitleChanged);
        named.conversation_title = Some("已有会话名".into());
        apply_agent_event(&mut state, &named);
        apply_agent_event(&mut state, &prompt_with_title("恢复后的新问题"));
        assert_eq!(state.title.as_deref(), Some("已有会话名"));
    }

    #[test]
    fn hook_reopens_after_dead_via_new_prompt() {
        let mut hook = SessionState {
            phase: Phase::Dead,
            ..Default::default()
        };
        assert!(commit_session_phase(
            &mut hook,
            PhaseSource::Hook,
            Phase::Thinking
        ));
        assert_eq!(hook.phase, Phase::Thinking);
    }

    #[test]
    fn conversation_binding_transition_matrix() {
        // 空 id 维持现状，不清标题。
        let mut state = SessionState {
            conversation_id: Some("conv-1".into()),
            title: Some("旧标题".into()),
            ..Default::default()
        };
        let mut empty = event(AgentEventKind::ToolFinished);
        empty.conversation_id = None;
        assert_eq!(
            switch_conversation(&mut state, &empty),
            BindingTransition::Ignored
        );
        assert_eq!(state.conversation_id.as_deref(), Some("conv-1"));
        assert_eq!(state.title.as_deref(), Some("旧标题"));

        // 同 id 不清标题。
        let mut same = event(AgentEventKind::ToolFinished);
        same.conversation_id = Some("conv-1".into());
        assert_eq!(
            switch_conversation(&mut state, &same),
            BindingTransition::Same
        );
        assert_eq!(state.title.as_deref(), Some("旧标题"));

        // 换 id 清标题，且只清一次。
        let mut switched = event(AgentEventKind::SessionStarted);
        switched.conversation_id = Some("conv-2".into());
        assert_eq!(
            switch_conversation(&mut state, &switched),
            BindingTransition::Switched
        );
        assert_eq!(state.conversation_id.as_deref(), Some("conv-2"));
        assert_eq!(state.title, None);
    }
}
