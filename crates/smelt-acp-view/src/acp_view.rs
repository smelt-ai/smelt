//! ACP 会话的消息流视图：第二种会话类型的 GPUI 皮肤。
//!
//! **薄客户端**：agent 子进程由 smeltd 托管（`smelt_core::acp_session` 里的
//! `apply_event` 归约、`AcpEvent::Permission`/`Elicitation` 的 responder 也
//! 都在那边——responder 绑在连接线程上没法跨进程传，这里没有资格直接持有
//! 它们）。这层只做两件事：把 `smelt_core::acp_client` 收到的 `AcpSnapshot`
//! 摊平进本地字段渲染出来，把用户操作打包成 `AcpUserAction` 发回去。四档
//! 着色 / Dock 角标 / 应用内待处理通知现在都由 smeltd 的集中状态订阅驱动
//! （跟终端会话共用 subscribe 通道），这层只镜像视图态，不再自己判相位跳变。

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, App, AppContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FollowMode, InteractiveElement, IntoElement, ListAlignment, ListState,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Window, div,
    list as virtual_list, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::clipboard::Clipboard;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, IconName, RopeExt, Sizable, StyledExt, h_flex, v_flex};

use agent_client_protocol::schema::v1::SessionId;

use smelt_core::acp_client::{AcpClientHandle, AcpClientLaunch, spawn_acp_client};
use smelt_core::acp_conn::{ModelState, PromptImage, SessionConfigState};
use smelt_core::acp_session::{
    AcpPhase, AcpSnapshot, AcpUserAction, ApprovalDetailsView, ElicitFieldKindView,
    PendingElicitation, PendingPermission, PermissionOptionKindView, PlanEntryStatusView, PlanView,
};
use smelt_core::agent_kind::{AcpAgentKind, AcpLaunchSpec};
use smelt_ui::daemon_states_global::{AttentionGlobal, AttentionKind};
use smelt_ui::ui_theme;

/// 消息流数据模型（AcpEntry/ToolOutputPart/ToolKind/ToolCallStatus）与 diff/
/// markdown 围栏这批纯逻辑现在都活在 `smelt_core::acp_chat`——不依赖 GPUI 也不
/// 依赖 agent_client_protocol，未来 web/mobile 端渲染同一份对话时不用重新实现
/// 一遍「怎么把协议事件变成可展示内容」。这里整段 re-export，文件里大量既有的
/// 裸 `AcpEntry::...` 用法不用逐处改路径。
pub use smelt_core::acp_chat::{
    AcpEntry, AcpImage, DiffLine, DiffLineTag, ToolCallStatus, ToolKind, ToolOutputPart,
    compact_diff_lines, diff_lines, is_interrupt_marker, strip_code_fence,
};

#[derive(Clone)]
struct CachedDiff {
    old_fingerprint: (usize, u64),
    new_fingerprint: (usize, u64),
    lines: std::rc::Rc<Vec<DiffLine>>,
    added: usize,
    removed: usize,
}

/// `@` / `/` 补全弹层的状态。回合态，不落盘。
struct CompletionPopup {
    /// 触发 token 在输入框文本里的字节范围（含 `@`/`/`），接受候选时按它替换。
    start: usize,
    end: usize,
    items: Vec<smelt_ui::acp_completion::Candidate>,
    selected: usize,
}

pub enum AcpViewEvent {
    Changed,
    PreviewImage(std::sync::Arc<gpui::Image>),
    ContinueInNewSession(AcpHandoffRequest),
    NavigateToSession(String),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AcpForkOrigin {
    pub session_id: String,
    pub title: String,
}

#[derive(Clone)]
pub struct AcpHandoffRequest {
    pub source: AcpForkOrigin,
    pub cwd: Option<String>,
    pub agent: AcpAgentKind,
    pub launch: AcpLaunchSpec,
    pub refresh_launch_from_settings: bool,
    pub profile_id: Option<String>,
    pub config_values: Vec<(String, String)>,
    pub prompt: String,
}

impl EventEmitter<AcpViewEvent> for AcpView {}

pub struct AcpView {
    sid: String,
    cwd: Option<String>,
    entries: Vec<AcpEntry>,
    permissions: Vec<PendingPermission>,
    /// 已发出回执、等待 smeltd 快照确认的队首审批。期间按钮替换成处理中状态，
    /// 防止网络往返时重复点击；队首变化后清空。
    permission_submitting: Option<(String, String)>,
    elicitation: Option<PendingElicitation>,
    /// 自由文本 elicitation 的本地编辑器；协议状态只保存字符串，不持有 GPUI 实体。
    elicitation_inputs: std::collections::HashMap<usize, Entity<InputState>>,
    phase: AcpPhase,
    /// 启动阶段的进度文案（下载运行时等），Starting 横幅显示。
    status_line: Option<String>,
    /// None = 已结束的占位视图（重开后才建；Ended 态没有输入框）。
    input: Option<Entity<InputState>>,
    /// smeltd 连接句柄——`None` 只在真正的冷恢复占位（没连过）出现；只要连过
    /// 一次就一直持有到视图销毁，Drop 时只会断开 socket（见 `AcpClientHandle`
    /// 文件头注释），不影响 smeltd 那边的会话存活。
    handle: Option<AcpClientHandle>,
    /// 重启用的启动规格（placeholder / restart 共用）。
    launch: AcpLaunchSpec,
    /// true = 普通会话重启时按当前设置刷新命令；false = 保留持久化下来的 launch。
    refresh_launch_from_settings: bool,
    /// workspace profile 的稳定 id；普通 agent 会话为 None。
    profile_id: Option<String>,
    /// 这条会话接的是哪个 agent（Claude / Copilot / Codex）：决定显示名，也决定
    /// 「重新开始」时该去全局配置的哪一条命令上取最新值。
    agent: AcpAgentKind,
    /// 已粘进来、等着随下一条 prompt 发出去的图片（缩略图条显示，发完清空）。
    /// 只在内存里待到发送为止：图片体积大，不进 workspace.json。
    pending_images: Vec<std::sync::Arc<gpui::Image>>,
    /// ACP 规定同一 session 一次只能有一个在跑的 turn；运行中点发送不能裸并发
    /// 塞第二个 prompt（协议没设计支持，行为全凭 agent 自己兜底）。这里改成
    /// 排队：Running 时点发送只入队，输入框上方给一条可见的「已排队」条，
    /// 相位回 Idle（apply_snapshot 里那个分支）再按顺序真正发出去。只在内存里
    /// 待到发送为止，不落盘。
    queued_prompts: std::collections::VecDeque<(String, Vec<std::sync::Arc<gpui::Image>>)>,
    /// 已发送消息的解码图片缓存，避免流式输出或 spinner 重绘时反复解码 base64。
    rendered_images: std::collections::HashMap<(usize, usize), std::sync::Arc<gpui::Image>>,
    /// Edit diff 的算法结果与紧凑预览，避免卡片展开后的每次重绘都重新计算。
    rendered_diffs: std::collections::HashMap<String, Vec<Option<CachedDiff>>>,
    /// 本会话的 agent 是否收图（握手 Ready 带来）。握手前默认 true——那时还没
    /// 粘图的机会，先假设支持，Ready 到了再按实际能力修正（Grok = false）。
    supports_image: bool,
    /// 「这个 agent 不收图」的一次性提示：粘图被拦时置上，输入框上方显示一行，
    /// 用户下次一打字（Change）就清掉，不占定时器。
    paste_hint: Option<String>,
    /// `@` / `/` 补全弹层的当前状态；None = 没在补全。
    completion: Option<CompletionPopup>,
    /// 补全候选列表的滚动位置；键盘移动选中项时同步保证其可见。
    completion_scroll: ScrollHandle,
    /// cwd 下的文件清单缓存（`@` 的候选源）。每敲一个字符跑一次 git ls-files
    /// 会明显卡手，所以一次会话只列一次。
    file_cache: Option<std::rc::Rc<Vec<String>>>,
    /// 当前 ACP 连接使用的运行时 session id，仅用于展示连接状态。
    acp_session_id: Option<SessionId>,
    /// agent 历史存储中的 canonical id。持久化和 `session/load` 只使用它；
    /// runtime 连接重新建立后返回的新 id 不能覆盖它。
    history_session_id: Option<SessionId>,
    /// 会话当前可用的斜杠命令 (名字, 说明)；空 = agent 没发过这个更新。
    /// 胶囊点开列出来、点一条填进输入框——只显示数量没有任何用处。
    available_commands: Vec<(String, String)>,
    /// 上下文用量：(已用 token, 窗口大小)。None = agent 没上报过，不显示。
    usage: Option<(u64, u64)>,
    /// 本次启动/续接的起点，用来在横幅上报「已等了几秒」。
    /// 实测 `session/new` 里 Claude Code 自身要约 10 秒（跟下载无关，同一适配器
    /// 进程建第二个会话一样慢），没有进度反馈会让人以为卡死了。
    starting_since: Option<std::time::Instant>,
    /// agent 最近一次上报的任务计划（每次全量覆盖）。回合态：不落盘，
    /// TurnEnded 保留最后一份供回看，「重新开始」清空。
    plan: Option<PlanView>,
    /// PLAN 条折叠态（默认展开，跟设计稿一致）。
    plan_collapsed: bool,
    /// 用户手动展开的推理摘要（key = entries 索引）。思考默认折叠，且此状态
    /// 只属于当前视图，不写入会话记录。
    expanded_thoughts: std::collections::HashSet<usize>,
    /// 手动展开的回合执行过程（key = 该组第一条 entry 的索引）。默认整组收起，
    /// 避免 Read/Edit/Bash 与思考摘要交替铺满消息流。
    expanded_process_groups: std::collections::HashSet<usize>,
    /// 模型状态：当前名 + 可切换的候选（协议给什么显示什么）；None = agent
    /// 没上报过，UI 就不显示模型胶囊，不拿适配器包名冒充。
    model: Option<ModelState>,
    /// 除模型以外的 ACP 会话配置。agent 未上报则不显示。
    config_options: Vec<SessionConfigState>,
    /// 当前回合开始时间与最近完成耗时，由 smeltd 计时后随快照同步。
    turn_started_at_ms: Option<u64>,
    last_turn_duration_ms: Option<u64>,
    /// 手动展开了完整输出的工具调用（key = tool_call_id）。长输出默认折叠成
    /// 前几行 + 「展开」，回合态不落盘。
    expanded_tools: std::collections::HashSet<String>,
    /// 手动展开 / 收起了整个工具卡片（key = tool_call_id）。默认规则：
    /// completed 收起，pending/in-progress/failed/等权限展开；用户点过后按这两组
    /// 覆盖默认值。只属于本地浏览状态，不落盘。
    expanded_tool_cards: std::collections::HashSet<String>,
    collapsed_tool_cards: std::collections::HashSet<String>,
    /// 可变高度消息虚拟列表：只测量和构建视口附近的 Markdown/工具卡。
    list_state: ListState,
    /// 用户是否已滚离消息尾部；由 ListScrollEvent 更新。
    viewing_history: bool,
    /// 「强制重启」请求正在路上：用于阻止侧栏菜单重复触发。跟
    /// `AcpPhase::Starting` 不是一回事——那是新进程握手阶段，这个是"旧进程
    /// 还没确认死透"的过渡态，两者可能重叠（发出 acp_restart 到收到新一份
    /// Starting 快照之间有个网络往返）。
    restarting: bool,
    /// 「强制重启」失败时的提示文案（连不上 smeltd、会话已不存在等）；下次
    /// 操作前一直显示，成功后清空。只属于本地展示状态，不落盘。
    restart_error: Option<String>,
    /// 冷恢复占位待自动启动：GUI 重启后第一次切到这个会话时自动 restart，
    /// 有旧 session id 则协议级续接，没有则新建一轮但保留本地历史。只消费一次——
    /// 自动启动失败（Fatal → Ended）后回到手动，错误得让人看见，不能循环重试。
    auto_resume_pending: bool,
    /// ACP 没有 fork 原语。新会话握手进入 Idle 后，自动发送一次精简交接提示。
    pending_initial_prompt: Option<String>,
    /// 来源会话当前选择的模型、权限等 ACP 配置。必须先于交接提示写入新会话。
    pending_initial_config: Vec<(String, String)>,
    /// “在新会话中继续”的来源，只用于导航和解释会话关系。
    fork_origin: Option<AcpForkOrigin>,
    focus_handle: FocusHandle,
    _input_sub: Option<gpui::Subscription>,
}

pub(crate) fn resolve_restart_launch(
    current_launch: &AcpLaunchSpec,
    profile_id: Option<&str>,
    config: &smelt_ui::agent_ui_config::AgentUiConfig,
    agent: AcpAgentKind,
    refresh_launch_from_settings: bool,
) -> AcpLaunchSpec {
    if let Some(profile_id) = profile_id {
        return config
            .find_profile(profile_id)
            .map(|profile| config.profile_launch_spec(profile))
            .unwrap_or_else(|| current_launch.clone());
    }
    if refresh_launch_from_settings {
        return AcpLaunchSpec::from_command(config.acp_cmd_for(agent));
    }
    current_launch.clone()
}

impl AcpView {
    /// 建视图并立即向 smeltd 发起 `acp_open`（非阻塞，握手结果以快照回来）。
    pub fn start(
        window: &mut Window,
        cx: &mut Context<Self>,
        agent: AcpAgentKind,
        launch: AcpLaunchSpec,
        profile_id: Option<String>,
        cwd: Option<String>,
    ) -> Self {
        let mut this = Self::placeholder(
            cx,
            agent,
            launch,
            profile_id.is_none(),
            profile_id,
            cwd,
            String::new(),
            Vec::new(),
            None,
            None,
        );
        this.phase = AcpPhase::Starting;
        this.starting_since = Some(std::time::Instant::now());
        this.init_input(window, cx);
        let handle = spawn_acp_client(AcpClientLaunch {
            id: this.sid.clone(),
            cwd: this.cwd.clone(),
            launch: this.launch.clone(),
            agent_id: agent.id().to_string(),
            resume_id: None, // 第一次开，没有旧会话可续
            retained_entries_len: this.entries.len(),
        });
        this.attach_handle(handle, cx);
        this
    }

    /// 新建独立 ACP 会话，并在握手完成后发送来源会话的精简交接上下文。
    pub fn start_with_handoff(
        window: &mut Window,
        cx: &mut Context<Self>,
        request: AcpHandoffRequest,
    ) -> Self {
        let mut this = Self::start(
            window,
            cx,
            request.agent,
            request.launch,
            request.profile_id,
            request.cwd,
        );
        this.refresh_launch_from_settings = request.refresh_launch_from_settings;
        this.pending_initial_prompt = Some(request.prompt);
        this.pending_initial_config = request.config_values;
        this.fork_origin = Some(request.source);
        this
    }

    /// 冷启动恢复用的占位：首次显示时自动启动。`entries` 只用于读取旧版存档的
    /// 迁移兼容；当前版本以 agent 的 `session/load` 重放作为历史唯一来源。
    /// `resume_session_id` 是上次握手成功后 agent 分配的 session id。
    ///
    /// `saved_sid`：**这是让 GUI 重开后能真正"接上还活着的 smeltd 会话"而不是
    /// 每次都当新会话重新 spawn 子进程的关键**——smeltd 用 id 判断"这是不是同
    /// 一个会话"，`Some(id)` 时沿用上次持久化的 id（GUI 冷启动恢复走这条，
    /// `main.rs` 的 `AcpSaved.sid`），id 对上了 smeltd 那边只要还没退出/没被
    /// kill，`restart()` 发起的 `acp_open` 就是一次廉价 attach，不是重新 spawn
    /// 子进程。`None` 生成一个全新 id——「从历史会话页继续」和真正的新会话都
    /// 走这条：前者本质是"起一条新的 smeltd 托管连接，靠 `resume_id` 对 agent
    /// 自己的持久化做 session/load"，不是"接上 smeltd 里已经在跑的那个会话"，
    /// 没有理由假装是同一个 id。
    pub fn placeholder(
        cx: &mut Context<Self>,
        agent: AcpAgentKind,
        launch: AcpLaunchSpec,
        refresh_launch_from_settings: bool,
        profile_id: Option<String>,
        cwd: Option<String>,
        reason: String,
        entries: Vec<AcpEntry>,
        resume_session_id: Option<SessionId>,
        saved_sid: Option<String>,
    ) -> Self {
        // 冷恢复会话首次显示就直接进入可用的对话页：有旧 session id 时续接，
        // 没有时启动新一轮。历史仍先留在本地，守护端若能 attach 会用其快照覆盖。
        let auto_resume_pending = true;
        let initial_entry_count = entries.len();
        let rendered_images = decode_entry_images(&entries, 0);
        let rendered_diffs = build_diff_cache(&entries);
        let list_state = ListState::new(initial_entry_count, ListAlignment::Top, px(800.));
        list_state.set_follow_mode(FollowMode::Tail);
        let view = cx.entity().downgrade();
        list_state.set_scroll_handler(move |event, _window, cx| {
            let view = view.clone();
            let viewing_history = event.is_scrolled && !event.is_following_tail;
            // list 正在可变借用自己的状态，延后通知外层重新判断 sticky 提问。
            cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.viewing_history != viewing_history {
                        this.viewing_history = viewing_history;
                        cx.notify();
                    }
                });
            });
        });
        Self {
            auto_resume_pending,
            sid: saved_sid.unwrap_or_else(|| format!("acp-{}", uuid::Uuid::new_v4())),
            cwd,
            entries,
            permissions: Vec::new(),
            permission_submitting: None,
            elicitation: None,
            elicitation_inputs: Default::default(),
            status_line: None,
            phase: AcpPhase::Ended(reason),
            input: None,
            handle: None,
            launch,
            refresh_launch_from_settings,
            profile_id,
            agent,
            pending_images: Vec::new(),
            queued_prompts: std::collections::VecDeque::new(),
            rendered_images,
            rendered_diffs,
            supports_image: true,
            paste_hint: None,
            completion: None,
            completion_scroll: ScrollHandle::new(),
            file_cache: None,
            acp_session_id: None,
            history_session_id: resume_session_id,
            available_commands: Vec::new(),
            usage: None,
            starting_since: None,
            plan: None,
            plan_collapsed: false,
            expanded_thoughts: std::collections::HashSet::new(),
            expanded_process_groups: std::collections::HashSet::new(),
            model: None,
            config_options: Vec::new(),
            turn_started_at_ms: None,
            last_turn_duration_ms: None,
            expanded_tools: std::collections::HashSet::new(),
            expanded_tool_cards: std::collections::HashSet::new(),
            collapsed_tool_cards: std::collections::HashSet::new(),
            list_state,
            viewing_history: false,
            restarting: false,
            restart_error: None,
            pending_initial_prompt: None,
            pending_initial_config: Vec::new(),
            fork_origin: None,
            focus_handle: cx.focus_handle(),
            _input_sub: None,
        }
    }

    /// 「重新开始」：带着上次的 session id（如果有）尝试真续接——smeltd 那边
    /// 如果这个会话还活着就是普通 attach，已经 Ended 才会真的重新 spawn
    /// 子进程（见 smeltd `acp_open` 的 attach-vs-relaunch 判断，这里不用关心
    /// 是哪一种，反正结果都会以快照回来：`ReadyKind::ResumedWithReplay` 时
    /// 服务端已经清空 entries 让 replay 重建，`Fresh` 且本地有历史时服务端
    /// 已经插好分割线——这层拿到的快照就是最终结果，不用再猜）。
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cfg) = cx.try_global::<smelt_ui::agent_ui_config::AgentUiConfig>() {
            self.launch = resolve_restart_launch(
                &self.launch,
                self.profile_id.as_deref(),
                cfg,
                self.agent,
                self.refresh_launch_from_settings,
            );
        }
        self.permissions.clear();
        self.permission_submitting = None;
        self.elicitation = None;
        self.elicitation_inputs.clear();
        self.plan = None; // 计划是回合态，新会话不该带着上一段的进度条
        self.model = None; // 模型等新会话握手后重新上报
        self.config_options.clear();
        self.usage = None; // 上下文用量属于旧会话，别带到新的上
        self.phase = AcpPhase::Starting;
        self.starting_since = Some(std::time::Instant::now());
        self.init_input(window, cx);
        let handle = spawn_acp_client(AcpClientLaunch {
            id: self.sid.clone(),
            cwd: self.cwd.clone(),
            launch: self.launch.clone(),
            agent_id: self.agent.id().to_string(),
            resume_id: self.history_session_id.as_ref().map(|s| s.to_string()),
            retained_entries_len: self.entries.len(),
        });
        self.attach_handle(handle, cx);
        cx.notify();
    }

    /// 历史页再次点“继续”时主动重新连接 smeltd。若 daemon 会话仍在，这只是
    /// attach 并立即返回完整快照；若 GUI 之前断线留下了旧 View，也能由此修复。
    pub fn reattach_to_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restart(window, cx);
    }

    /// 舞台头状态胶囊用的相位文案 + 颜色。
    ///
    /// ACP 有自己的相位机，不能经 DaemonStates 那套五态绕一圈拿——`Starting`
    /// 和 `Ended` 在映射里都会塌成「空闲」，于是「正在启动」的横幅底下顶着一个
    /// 「空闲」胶囊，自相矛盾。
    pub fn phase_label(&self) -> (&'static str, u32) {
        match &self.phase {
            AcpPhase::Starting => ("启动中", ui_theme::blue()),
            AcpPhase::Idle => ("空闲", ui_theme::text_faint()),
            AcpPhase::Running => ("运行中", ui_theme::blue()),
            AcpPhase::AwaitingApproval => ("等你批准", ui_theme::yellow()),
            AcpPhase::AwaitingChoice => ("等你选择", ui_theme::yellow()),
            AcpPhase::Ended(_) => ("已结束", ui_theme::text_faint()),
        }
    }

    /// 切到本会话时自动启动：冷恢复占位（Ended）第一次被激活就 restart，
    /// 像终端一样「点开就是活的」。只触发一次，见字段注释。
    pub fn maybe_auto_resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.auto_resume_pending {
            return;
        }
        self.auto_resume_pending = false;
        if matches!(self.phase, AcpPhase::Ended(_)) && self.handle.is_none() {
            self.restart(window, cx);
        }
    }

    fn init_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("给 agent 的指令：@ 引文件，/ 用命令，Enter 发送，Shift+Enter 换行")
                .multi_line(true)
                .auto_grow(3, 10)
        });
        self._input_sub = Some(cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _input, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter { shift, .. } => {
                        if !shift {
                            this.submit_input(window, cx);
                        }
                    }
                    // 每次文本变化重算补全 token（打 `@`/`/` 就弹，打空格就收）。
                    InputEvent::Change => this.refresh_completion(cx),
                    InputEvent::Blur => this.completion = None,
                    _ => {}
                }
            },
        ));
        self.input = Some(input);
    }

    /// 启动期每秒重绘一次，让横幅上的「已 N 秒」真的在走。
    /// 相位离开 Starting 就自然停（不占常驻定时器）。
    fn tick_starting(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |v, cx| {
                        let starting = matches!(v.phase, AcpPhase::Starting);
                        if starting {
                            cx.notify();
                        }
                        starting
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
            }
        })
        .detach();
    }

    /// 挂上连接句柄并起快照 drain（start / restart 共用）。
    fn attach_handle(&mut self, handle: AcpClientHandle, cx: &mut Context<Self>) {
        let snapshot_rx = handle.snapshot_rx.clone();
        self.handle = Some(handle);
        self.tick_starting(cx);
        cx.spawn(async move |this, cx| {
            while let Ok(snap) = snapshot_rx.recv().await {
                if this
                    .update(cx, |view, cx| view.apply_snapshot(snap, cx))
                    .is_err()
                {
                    return; // 视图已销毁
                }
            }
        })
        .detach();
    }

    /// smeltd 托管用的会话 id，也是持久化存档（`AcpSaved.sid`）的 key——
    /// GUI 重开后拿它原样传回 `placeholder` 的 `saved_sid`，才能接上 smeltd
    /// 里还活着的同一个会话，而不是每次都当新会话处理。
    pub fn session_id(&self) -> &str {
        &self.sid
    }

    /// 从首条用户消息生成稳定的会话标题。Codex app-server 不会主动把 thread name
    /// 推给客户端；左侧会话列表和完成通知至少应能说明这轮对话在做什么。
    pub fn auto_title(&self) -> Option<String> {
        smelt_core::acp_chat::auto_title(&self.entries)
    }

    /// 存档快照：写进 AcpSaved.history_session_id，GUI 重开后「重新开始」
    /// 才有旧 session id 可用来尝试真续接。
    pub fn history_session_id_for_save(&self) -> Option<SessionId> {
        self.history_session_id.clone()
    }

    /// 停止当前 turn（session/cancel）。agent 会以 Cancelled 收尾，相位随 TurnEnded 回 Idle。
    fn cancel_turn(&mut self) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::Cancel);
        }
    }

    /// 「停止」打不断（agent 卡在工具调用里对 cancel 不理不睬）时的兜底：
    /// 让 smeltd 直接杀掉整个 agent 进程组、换一个新的接着跑，带
    /// `history_session_id` 走 `session/load` 接回同一份历史——标签、这条视图
    /// 的 entries、GUI 这边的 acp_open 连接全部原地不动，只是底下的进程换了
    /// 一个。跟 `restart()`（给已 Ended 的占位视图用）不是一回事：那个要重新
    /// 建 GUI 自己的 socket 连接；这个不需要，smeltd 杀完重连内部子进程后会
    /// 照常沿着现有连接推新快照过来（`attach_handle` 起的 drain 循环还在跑）。
    pub fn force_restart(&mut self, cx: &mut Context<Self>) {
        if self.restarting {
            return;
        }
        self.restarting = true;
        self.restart_error = None;
        cx.notify();
        let sid = self.sid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { smelt_core::acp_client::restart_acp_session(&sid) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.restarting = false;
                if let Err(err) = result {
                    view.restart_error = Some(err);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cwd(&self) -> Option<String> {
        self.cwd.clone()
    }

    /// 启动规格（存档用：重开 GUI 后按它「重新开始」）。
    pub fn launch_spec(&self) -> AcpLaunchSpec {
        self.launch.clone()
    }

    pub fn refresh_launch_from_settings(&self) -> bool {
        self.refresh_launch_from_settings
    }

    pub fn profile_id(&self) -> Option<&str> {
        self.profile_id.as_deref()
    }

    /// 这条会话接的 agent 种类（存档 / 标题 / 舞台头胶囊用）。
    pub fn agent_kind(&self) -> AcpAgentKind {
        self.agent
    }

    pub fn fork_origin(&self) -> Option<AcpForkOrigin> {
        self.fork_origin.clone()
    }

    pub fn set_fork_origin(&mut self, origin: Option<AcpForkOrigin>) {
        self.fork_origin = origin;
    }

    /// cwd 下的文件清单（首次调用才真去列，之后走缓存）。
    fn file_list(&mut self) -> std::rc::Rc<Vec<String>> {
        if let Some(cached) = &self.file_cache {
            return cached.clone();
        }
        let list = self
            .cwd
            .as_deref()
            .map(smelt_ui::acp_completion::list_files)
            .unwrap_or_else(|| std::rc::Rc::new(Vec::new()));
        self.file_cache = Some(list.clone());
        list
    }

    /// 按输入框当前内容重算补全候选。
    fn refresh_completion(&mut self, cx: &mut Context<Self>) {
        // 一打字就把「不收图」提示撤了——它是针对上一次粘贴的，用户已经继续了。
        self.paste_hint = None;
        let Some(input) = self.input.clone() else {
            self.completion = None;
            return;
        };
        let (text, cursor) = {
            let s = input.read(cx);
            (s.value().to_string(), s.cursor())
        };
        // cursor 是字节偏移，可能落在多字节字符中间（中文输入过程中），
        // 切之前先确认是字符边界，否则 panic。
        let cursor = cursor.min(text.len());
        if !text.is_char_boundary(cursor) {
            return;
        }
        let Some(trigger) = smelt_ui::acp_completion::detect_trigger(&text[..cursor]) else {
            if self.completion.is_some() {
                self.completion = None;
                cx.notify();
            }
            return;
        };
        let files = match trigger.kind {
            smelt_ui::acp_completion::Kind::At => self.file_list(),
            smelt_ui::acp_completion::Kind::Slash => std::rc::Rc::new(Vec::new()),
        };
        let items =
            smelt_ui::acp_completion::candidates(&trigger, &files, &self.available_commands);
        self.completion = (!items.is_empty()).then(|| CompletionPopup {
            start: trigger.start,
            end: cursor,
            items,
            selected: 0,
        });
        self.completion_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// 上下移动补全选中项（返回 false = 当前没在补全，按键该交回输入框）。
    fn move_completion(&mut self, delta: i32, cx: &mut Context<Self>) -> bool {
        let Some(popup) = &mut self.completion else {
            return false;
        };
        let n = popup.items.len() as i32;
        popup.selected = (popup.selected as i32 + delta).rem_euclid(n) as usize;
        self.completion_scroll.scroll_to_item(popup.selected);
        cx.notify();
        true
    }

    /// 把选中的候选替换进输入框（返回 false = 没在补全）。
    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.completion.take() else {
            return false;
        };
        let Some(input) = self.input.clone() else {
            return false;
        };
        let Some(item) = popup.items.get(popup.selected) else {
            return false;
        };
        let insert = item.insert.clone();
        input.update(cx, |s, cx| {
            let text = s.value().to_string();
            // 只换掉触发 token 那一段，光标后面的内容原样留着。
            if popup.start <= popup.end
                && popup.end <= text.len()
                && text.is_char_boundary(popup.start)
                && text.is_char_boundary(popup.end)
            {
                let merged = format!("{}{}{}", &text[..popup.start], insert, &text[popup.end..]);
                let cursor_after_insert = popup.start + insert.len();
                s.set_value(merged, window, cx);
                let position = s.text().offset_to_position(cursor_after_insert);
                s.set_cursor_position(position, window, cx);
            }
            s.focus(window, cx);
        });
        cx.notify();
        true
    }

    /// 末几条消息的纯文本（总览卡片迷你预览，对齐终端的 last_lines）。
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.entries.iter().rev() {
            if out.len() >= n {
                break;
            }
            let line = match e {
                AcpEntry::User(t) => format!("> {}", t.lines().next().unwrap_or_default()),
                AcpEntry::UserWithImages { text, images } => {
                    let first = text.lines().next().unwrap_or_default();
                    if first.is_empty() {
                        format!("> {} 张图片", images.len())
                    } else {
                        format!("> {first}")
                    }
                }
                AcpEntry::Assistant {
                    text,
                    thought: false,
                } => text.lines().last().unwrap_or_default().to_string(),
                AcpEntry::Assistant { thought: true, .. } => continue,
                AcpEntry::ToolCall { title, .. } => format!("🔧 {title}"),
                AcpEntry::Divider(_) => continue,
            };
            if !line.trim().is_empty() {
                out.push(line);
            }
        }
        out.reverse();
        out
    }

    pub fn completed_unread(&self, cx: &App) -> bool {
        cx.try_global::<AttentionGlobal>().is_some_and(|store| {
            store
                .0
                .lock()
                .unwrap()
                .unread(&self.sid)
                .is_some_and(|item| item.kind == AttentionKind::Success)
        })
    }

    /// 会话被激活查看后清「有结果可看」。
    pub fn mark_read(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = cx.try_global::<AttentionGlobal>() {
            store.0.lock().unwrap().mark_read(&self.sid);
        }
    }

    pub fn is_awaiting_approval(&self) -> bool {
        matches!(self.phase, AcpPhase::AwaitingApproval)
    }

    pub fn is_running(&self) -> bool {
        matches!(self.phase, AcpPhase::Running)
    }

    /// 出了选择题等用户点（四档色里归「需要处理」橙档）。
    pub fn is_awaiting_choice(&self) -> bool {
        matches!(self.phase, AcpPhase::AwaitingChoice)
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        if let Some(input) = &self.input {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// 把一段文本塞进输入框并聚焦（SKILLS 面板点一条 skill 用）。
    /// 不自动发送——skill 后面常还要补一句话，发不发由人定。
    pub fn insert_prompt_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        input.update(cx, |s, cx| {
            let cur = s.value().to_string();
            let merged = if cur.trim().is_empty() {
                format!("{text} ")
            } else if cur.ends_with(' ') {
                format!("{cur}{text} ")
            } else {
                format!("{cur} {text} ")
            };
            s.set_value(merged, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// 总览快捷回复直达（对齐终端会话的 send_key_to_session 语义）。
    /// 连带把已粘贴的待发图片一起发出去并清空。发出去之后不再本地手动追加
    /// 用户消息/改相位——那是 smeltd 的事（见 `apply_acp_user_action` 里的
    /// `note_prompt_sent`），本地 socket 往返是毫秒级，等下一份快照回来就有。
    pub fn send_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        // 光有图没有字也算一条有效 prompt（「这截图什么意思」式的用法）。
        if text.trim().is_empty() && self.pending_images.is_empty() {
            return;
        }
        let images = std::mem::take(&mut self.pending_images);
        self.send_prompt_now(text, images, cx);
    }

    /// 真正把一条 prompt 打给 smeltd——不碰 `self.pending_images`，图片由调用方
    /// 传入。`send_prompt`（直发）和排队 flush（`flush_queued_prompt`）都走这里，
    /// 保证两条路径的编码/发送逻辑只有一份。
    fn send_prompt_now(
        &mut self,
        text: String,
        images: Vec<std::sync::Arc<gpui::Image>>,
        cx: &mut Context<Self>,
    ) {
        let encoded: Vec<PromptImage> = images
            .iter()
            .map(|im| PromptImage {
                mime: image_mime(im.format).to_string(),
                data_b64: base64_encode(&im.bytes),
            })
            .collect();
        if let Some(h) = &self.handle {
            if h.action_tx
                .try_send(AcpUserAction::Prompt {
                    text,
                    images: encoded,
                })
                .is_ok()
            {
                cx.notify();
            }
        }
    }

    /// 相位回 Idle 时按顺序取一条排队消息发出去（一次只发一条——发出去后相位
    /// 会变回 Running，下一次真正 Idle 再继续，不能一口气把整个队列打光，
    /// 否则又变回协议不支持的裸并发）。
    fn flush_queued_prompt(&mut self, cx: &mut Context<Self>) {
        if let Some((text, images)) = self.queued_prompts.pop_front() {
            self.send_prompt_now(text, images, cx);
        }
    }

    /// 剪贴板里是图就收进待发列表（返回 true 表示这次粘贴被图片消费掉了，
    /// 调用方据此拦下事件，别再让输入框按文本粘一遍）。
    fn take_clipboard_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let has_image = item
            .entries()
            .iter()
            .any(|e| matches!(e, gpui::ClipboardEntry::Image(_)));
        if !has_image {
            return false;
        }
        // 能力门：agent 不收图就别收进来（Grok = false）。返回 true 照样吞掉这次
        // 粘贴——图片剪贴板里没有文本，放行给输入框也是白搭，只会漏个空。
        if !self.supports_image {
            self.paste_hint = Some(format!(
                "{} 不支持图片，已忽略粘贴",
                self.agent.short_label()
            ));
            cx.notify();
            return true;
        }
        for entry in item.into_entries() {
            if let gpui::ClipboardEntry::Image(image) = entry {
                self.pending_images.push(std::sync::Arc::new(image));
            }
        }
        self.paste_hint = None;
        cx.notify();
        true
    }

    /// 关闭标签：只摘掉本地连接（`AcpClientHandle` Drop 会断开 socket），
    /// **不**终止 smeltd 里的会话——跟关一个终端标签不会杀掉底下的 shell 是
    /// 唯一调用方是 `main.rs::close_session`（用户点 × 主动关标签）——那条
    /// 路径本来就跟终端会话共用同一个"用户主动关 = 让守护杀掉底层进程"的
    /// 语气（挨着的 `terminal::kill_remote` 调用是同一个意图），不是"切标签/
    /// 退出 App 这种先不看了"，所以这里要真的终结 smeltd 里的会话，不能只是
    /// 摘本地连接。真正的"GUI 退出/切标签不该带走会话"体现在别处：没有任何
    /// 代码路径会在那些场景调用这个函数。
    pub fn shutdown(&mut self, _cx: &mut App) {
        smelt_core::acp_client::kill_acp_session(&self.sid);
        self.handle = None;
    }

    fn submit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        let text = input.read(cx).value().trim().to_string();
        // 只贴了图没打字也要能发。
        if text.is_empty() && self.pending_images.is_empty() {
            return;
        }
        input.update(cx, |s, cx| s.set_value("", window, cx));
        if self.is_running() {
            // 同 session 一次只能有一个在跑的 turn（ACP 约定），运行中点发送
            // 不裸并发塞第二个 prompt，先排队，等相位回 Idle 再按顺序真正发出。
            let images = std::mem::take(&mut self.pending_images);
            self.queued_prompts.push_back((text, images));
            cx.notify();
        } else {
            self.send_prompt(text, cx);
        }
    }

    /// 快照应用：整份状态从 smeltd 镜像过来。归约（entries 合并/phase 机/
    /// 回声去重）已经在服务端做完了，这里只做两件事：
    /// 1. 摊平快照字段进本地同名字段，渲染代码不用碰；
    /// 2. 持久化 / 重绘时机跟着快照走。四色状态、Dock 角标、待处理通知都由
    ///    外面的集中状态订阅维护，这里不再自己判相位跳变。
    fn apply_snapshot(&mut self, snap: AcpSnapshot, cx: &mut Context<Self>) {
        let should_persist = snap.should_persist;
        let old_entries_len = self.entries.len();
        let incremental_entries = snap.entries_offset <= self.entries.len();
        let entries_offset = if incremental_entries {
            snap.entries_offset
        } else {
            0
        };
        let previous_permission = self
            .permissions
            .first()
            .map(|card| (card.tool_call_id.clone(), card.question.clone()));
        if incremental_entries {
            self.entries.truncate(snap.entries_offset);
            self.entries.extend(snap.entries);
        } else {
            // 不应发生：Unix stream 有序且写失败会断开。保守清空，避免显示错位历史。
            self.entries = snap.entries;
        }
        self.rendered_images
            .retain(|(entry_ix, _), _| *entry_ix < entries_offset);
        self.rendered_images.extend(decode_entry_images(
            &self.entries[entries_offset..],
            entries_offset,
        ));
        refresh_diff_cache(&self.entries, entries_offset, &mut self.rendered_diffs);
        let new_entries_len = self.entries.len();
        if snap.entries_offset <= old_entries_len {
            // splice 会把落在被替换 item 内部的滚动锚点重置到该 item 顶部。
            // 流式正文持续替换最后一项时，用户若正在这项里向上浏览，就会每个
            // chunk 被拉回一次，形成明显抖动。离开尾随态后保留逻辑锚点；正文
            // 只在锚点下方增长，原 offset 仍然代表同一块可见内容。
            let scroll_anchor = (!self.list_state.is_following_tail())
                .then(|| self.list_state.logical_scroll_top());
            self.list_state.splice(
                snap.entries_offset..old_entries_len,
                new_entries_len - snap.entries_offset,
            );
            if let Some(anchor) = scroll_anchor {
                self.list_state.scroll_to(anchor);
            }
        } else {
            self.list_state.reset(new_entries_len);
        }
        self.phase = snap.phase;
        self.permissions = snap.pending_permissions;
        let current_permission = self
            .permissions
            .first()
            .map(|card| (card.tool_call_id.clone(), card.question.clone()));
        if previous_permission != current_permission {
            self.permission_submitting = None;
        }
        self.elicitation = snap.pending_elicitation;
        if self.elicitation.is_none() {
            self.elicitation_inputs.clear();
        }
        self.status_line = snap.status_line;
        let runtime_session_id = snap.acp_session_id.map(SessionId::new);
        self.acp_session_id = runtime_session_id.clone();
        if let Some(history_session_id) = snap.history_session_id {
            self.history_session_id = Some(SessionId::new(history_session_id));
        } else if self.history_session_id.is_none() {
            // 兼容尚未携带 history_session_id 的旧 daemon 快照。
            self.history_session_id = runtime_session_id;
        }
        self.supports_image = snap.supports_image;
        self.available_commands = snap.available_commands;
        self.usage = snap.usage;
        self.plan = snap.plan;
        self.model = snap.model;
        self.config_options = snap.config_options;
        self.turn_started_at_ms = snap.turn_started_at_ms;
        self.last_turn_duration_ms = snap.last_turn_duration_ms;
        let _ = snap.completed_unread;
        self.prune_tool_ui_state();

        if matches!(self.phase, AcpPhase::Ended(_)) {
            self.handle = None;
        }

        if matches!(self.phase, AcpPhase::Idle) {
            for (config_id, value_id) in std::mem::take(&mut self.pending_initial_config) {
                self.set_config_option(config_id, value_id);
            }
            if let Some(prompt) = self.pending_initial_prompt.take() {
                self.send_prompt(prompt, cx);
            } else if !self.queued_prompts.is_empty() {
                // 交接提示和排队消息不会同时出现（前者只在全新 fork 会话里用），
                // 分支互斥即可：这轮 Idle 只发队首一条，剩下的等下一次 Idle。
                self.flush_queued_prompt(cx);
            }
        }

        if should_persist {
            cx.emit(AcpViewEvent::Changed);
        }
        cx.notify();
    }

    /// 快照是全量覆盖，历史重放 / 新会话可能清空旧 entries；把只属于本地 UI
    /// 的工具展开状态同步裁剪掉，避免长会话来回续接后集合无限长。
    fn prune_tool_ui_state(&mut self) {
        let live_ids: std::collections::HashSet<String> = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                AcpEntry::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        self.expanded_tools.retain(|id| live_ids.contains(id));
        self.expanded_tool_cards.retain(|id| live_ids.contains(id));
        self.collapsed_tool_cards.retain(|id| live_ids.contains(id));
        self.expanded_thoughts.retain(|ix| *ix < self.entries.len());
        self.expanded_process_groups
            .retain(|ix| *ix < self.entries.len());
    }

    fn tool_card_is_expanded(
        &self,
        id: &str,
        status: ToolCallStatus,
        has_pending_permission: bool,
    ) -> bool {
        if self.expanded_tool_cards.contains(id) {
            return true;
        }
        if self.collapsed_tool_cards.contains(id) {
            return false;
        }
        tool_card_default_expanded(status, has_pending_permission)
    }

    fn toggle_tool_card(
        &mut self,
        id: String,
        status: ToolCallStatus,
        has_pending_permission: bool,
        cx: &mut Context<Self>,
    ) {
        if self.tool_card_is_expanded(&id, status, has_pending_permission) {
            self.expanded_tool_cards.remove(&id);
            self.collapsed_tool_cards.insert(id);
        } else {
            self.collapsed_tool_cards.remove(&id);
            self.expanded_tool_cards.insert(id);
        }
        cx.notify();
    }

    /// 选择题点选：动作发给 smeltd（真正的选中/提交状态机在服务端跑，见
    /// `smelt_core::acp_session::choose_elicitation`），下一份快照回来就带着
    /// 更新后的 `chosen`——本地 socket 往返够快，不用做乐观本地更新，免得
    /// 跟服务端真相分叉。单字段单选点了自动追发一条 Submit，跟服务端那边
    /// 「整卡只有一个单选字段，点了就是答案」的判断保持一致。
    fn pick_elicit_option(&mut self, field_ix: usize, opt_ix: usize, _cx: &mut Context<Self>) {
        let Some(h) = &self.handle else { return };
        let _ = h
            .action_tx
            .try_send(AcpUserAction::ElicitationChoose { field_ix, opt_ix });
        let single_select = self.elicitation.as_ref().is_some_and(|card| {
            card.fields.len() == 1 && matches!(card.fields[0].kind, ElicitFieldKindView::Select(_))
        });
        if single_select {
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationSubmit);
        }
    }

    /// 每个字段都有选择后才可提交（渲染侧按这个亮按钮）。
    fn elicit_ready(&self, cx: &App) -> bool {
        self.elicitation.as_ref().is_some_and(|card| {
            card.fields
                .iter()
                .enumerate()
                .all(|(ix, field)| match field.kind {
                    ElicitFieldKindView::Text { .. } => self
                        .elicitation_inputs
                        .get(&ix)
                        .is_some_and(|input| !input.read(cx).value().trim().is_empty()),
                    ElicitFieldKindView::ExternalUrl(_) => true,
                    _ => card.chosen.get(&ix).is_some_and(|sel| !sel.is_empty()),
                })
        })
    }

    fn submit_elicitation(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = &self.handle {
            for (&field_ix, input) in &self.elicitation_inputs {
                let _ = h.action_tx.try_send(AcpUserAction::ElicitationText {
                    field_ix,
                    value: input.read(cx).value().to_string(),
                });
            }
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationSubmit);
        }
    }

    /// 「跳过」：丢弃卡片（服务端那边的 responder Drop 自动回 Cancel），继续
    /// 文本对话。
    fn dismiss_elicitation(&mut self, _cx: &mut Context<Self>) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::ElicitationDismiss);
        }
    }

    /// 当前模型的人类可读名（舞台头显示用）；None = agent 没上报过。
    pub fn model_name(&self) -> Option<String> {
        self.model.as_ref().map(|m| m.current_name.clone())
    }

    /// 当前上下文已用 token 数（舞台头显示用）；None = agent 没上报过用量。
    /// 跟输入栏「上下文 %」胶囊同一个数据源，只是这里要精确数字而非百分比。
    pub fn context_tokens_used(&self) -> Option<u64> {
        self.usage.map(|(used, _)| used)
    }

    /// 写回 agent 上报的会话配置。四个 agent 共用 ACP 的标准接口。
    fn set_config_option(&mut self, config_id: String, value_id: String) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::SetConfigOption {
                config_id,
                value_id,
            });
        }
    }

    /// 输入栏 agent 胶囊的展示名：从启动命令里抠个可读的包名/程序名
    /// （`bunx @scope/claude-agent-acp@0.59.0` → `claude-agent-acp`，
    /// `copilot --acp` → `copilot`）。没有模型名数据源，不硬编。
    fn agent_label(&self) -> String {
        let tok = self
            .launch
            .command
            .split_whitespace()
            .rev()
            .find(|t| !t.starts_with('-'))
            .unwrap_or("agent");
        let name = tok.rsplit('/').next().unwrap_or(tok);
        name.split('@')
            .find(|s| !s.is_empty())
            .unwrap_or(name)
            .to_string()
    }

    /// PLAN 条：agent 上报的任务计划 → 消息流上方的可折叠进度条。
    /// 折叠 = 一行摘要 + 进度条；展开 = 三态步骤清单（对齐设计稿）。
    /// 只借 `&Context`（listener 不需要可变借用），render 里跟 theme 引用共存。
    fn render_plan_bar(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let plan = self.plan.as_ref()?;
        let total = plan.entries.len();
        if total == 0 {
            return None;
        }
        let done = plan
            .entries
            .iter()
            .filter(|e| matches!(e.status, PlanEntryStatusView::Completed))
            .count();
        let in_progress = plan
            .entries
            .iter()
            .filter(|e| matches!(e.status, PlanEntryStatusView::InProgress))
            .count();
        // 「第几步 of 总数」：正在跑的算当前步；全完成就是 n of n。
        let current = (done + in_progress).min(total);
        let (summary, summary_color) = if done == total {
            (
                format!("{total} of {total} · 完成"),
                gpui::rgb(ui_theme::green()),
            )
        } else if in_progress > 0 {
            (
                format!("{current} of {total} · 进行中"),
                gpui::rgb(ui_theme::accent()),
            )
        } else {
            (
                format!("{done} of {total}"),
                gpui::rgb(ui_theme::text_muted()),
            )
        };
        let progress = (done as f32 + in_progress as f32 * 0.5) / total as f32;

        let mut bar = gpui_component::v_flex()
            .border_b_1()
            .border_color(gpui::rgb(ui_theme::border_dim()))
            .bg(gpui::rgb(ui_theme::bg_status()))
            .child(
                h_flex()
                    .id("acp-plan-toggle")
                    .px_4()
                    .py_2()
                    .gap_2p5()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.plan_collapsed = !this.plan_collapsed;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(10.))
                            .text_xs()
                            .text_color(gpui::rgb(ui_theme::text_muted()))
                            .child(if self.plan_collapsed { "▸" } else { "▾" }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(gpui::rgb(ui_theme::text_muted()))
                            .child("PLAN"),
                    )
                    .child(div().text_xs().text_color(summary_color).child(summary))
                    .child(
                        div()
                            .flex_1()
                            .max_w(px(180.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(gpui::rgb(ui_theme::border_dim()))
                            .overflow_hidden()
                            .child(
                                div()
                                    .w(gpui::relative(progress.clamp(0., 1.)))
                                    .h_full()
                                    .bg(gpui::rgb(ui_theme::accent())),
                            ),
                    ),
            );
        if !self.plan_collapsed {
            let mut steps = gpui_component::v_flex().px_4().pb_3().gap_0p5();
            for entry in &plan.entries {
                let row = h_flex().gap_2p5().items_center().py_0p5();
                let row = match entry.status {
                    PlanEntryStatusView::Completed => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .bg(gpui::rgb(ui_theme::green()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::on_accent()))
                                .child("✓"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_faint()))
                                .line_through()
                                .child(entry.content.clone()),
                        ),
                    PlanEntryStatusView::InProgress => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .border_1()
                                .border_color(gpui::rgb(ui_theme::accent()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .size(px(7.))
                                        .rounded_xs()
                                        .bg(gpui::rgb(ui_theme::accent())),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(gpui::rgb(ui_theme::text_bright()))
                                        .child(entry.content.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(gpui::rgb(ui_theme::accent()))
                                        .child("· 进行中"),
                                ),
                        ),
                    // Pending 与协议未来的新状态都按「待做」渲染。
                    _ => row
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(15.))
                                .rounded_sm()
                                .border_1()
                                .border_color(gpui::rgb(ui_theme::border_focus())),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child(entry.content.clone()),
                        ),
                };
                steps = steps.child(row);
            }
            bar = bar.child(steps);
        }
        Some(bar.into_any_element())
    }

    /// ⌘⏎ 快捷批准：选第一个 allow 类选项（跟绿色主按钮同一目标）。
    fn pick_permission_primary(&mut self, cx: &mut Context<Self>) {
        let Some(card) = self.permissions.first() else {
            return;
        };
        let Some(pix) = card.options.iter().position(|o| {
            matches!(
                o.kind,
                PermissionOptionKindView::AllowOnce | PermissionOptionKindView::AllowAlways
            )
        }) else {
            return;
        };
        let option_id = card.options[pix].option_id.clone();
        let tool_call_id = card.tool_call_id.clone();
        self.pick_permission(&tool_call_id, &option_id, cx);
    }

    /// 审批按钮：把选中项发给 smeltd（真正消费 responder 回 RPC 是服务端的
    /// 事），卡片收起、相位回 Running 等下一份快照即可，不用本地抢跑。
    fn pick_permission(&mut self, tool_call_id: &str, option_id: &str, cx: &mut Context<Self>) {
        // ACP agent 可能一次发来多条请求，但实际执行仍按队列推进。只允许回应
        // 队首，避免上一帧残留的按钮或其它入口越过当前审批。
        if self.permission_submitting.is_some()
            || !is_active_permission_selection(&self.permissions, tool_call_id, option_id)
        {
            return;
        }
        let tool_call_id = tool_call_id.to_string();
        let option_id = option_id.to_string();
        if let Some(h) = &self.handle {
            if h.action_tx
                .try_send(AcpUserAction::PermissionSelect {
                    tool_call_id: tool_call_id.clone(),
                    option_id: option_id.clone(),
                })
                .is_ok()
            {
                self.permission_submitting = Some((tool_call_id, option_id));
                cx.notify();
            }
        }
    }
}

impl Focusable for AcpView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AcpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(card) = &self.elicitation {
            let text_fields: Vec<(usize, bool, String, String)> = card
                .fields
                .iter()
                .enumerate()
                .filter_map(|(ix, field)| match field.kind {
                    ElicitFieldKindView::Text { secret } => Some((
                        ix,
                        secret,
                        field.title.clone(),
                        card.text_values.get(&ix).cloned().unwrap_or_default(),
                    )),
                    _ => None,
                })
                .collect();
            self.elicitation_inputs
                .retain(|ix, _| text_fields.iter().any(|(field_ix, ..)| field_ix == ix));
            for (ix, secret, title, value) in text_fields {
                self.elicitation_inputs.entry(ix).or_insert_with(|| {
                    cx.new(|cx| {
                        let mut state = InputState::new(window, cx)
                            .placeholder(&title)
                            .default_value(value);
                        if secret {
                            state = state.masked(true);
                        }
                        state
                    })
                });
            }
        }
        let t = cx.theme();
        let muted = t.muted_foreground;
        let acp_surface: gpui::Hsla = gpui::transparent_black().into();

        // 相位横幅：启动中 / 已结束（含失败原因）时显示；正常运行不占空间。
        let banner: Option<gpui::AnyElement> = match &self.phase {
            AcpPhase::Starting => Some(
                div()
                    .p_2()
                    .text_sm()
                    .text_color(muted)
                    .child(self.status_line.clone().unwrap_or_else(|| {
                        // 说实话：慢的是 Claude Code 自己建会话（实测约 10 秒），
                        // 不是「首次下载适配器」——那句每次都显示，是假的。
                        // 报出已等秒数，免得看着像卡死。
                        let waited = self
                            .starting_since
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        let what = if self.history_session_id.is_some() {
                            "正在续接上次的会话".to_string()
                        } else {
                            format!("正在启动 {}", self.agent.label())
                        };
                        format!("{what}…（已 {waited} 秒，通常 10 秒左右）")
                    }))
                    .into_any_element(),
            ),
            AcpPhase::Ended(msg) => Some(
                v_flex()
                    .p_2()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(t.danger).child("会话已结束"))
                    .when(!msg.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .font_family("monospace")
                                .child(msg.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("acp-restart")
                            .w(px(120.))
                            .px_3()
                            .py_1p5()
                            .rounded_lg()
                            .border_1()
                            .border_color(t.border)
                            .text_sm()
                            .text_center()
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.8))
                            .child("重新开始")
                            .on_click(cx.listener(|this, _ev, window, cx| {
                                this.restart(window, cx);
                            })),
                    )
                    .into_any_element(),
            ),
            _ => None,
        };
        let fork_banner = self.fork_origin.clone().map(|origin| {
            let source_id = origin.session_id.clone();
            h_flex()
                .w_full()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(t.border)
                .text_xs()
                .text_color(muted)
                .child(Icon::new(IconName::SquareTerminal).xsmall())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("从「{}」继续", origin.title)),
                )
                .child(
                    div()
                        .id("acp-return-to-source")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(gpui::rgb(ui_theme::bg_hover())))
                        .child("返回原会话")
                        .on_click(cx.listener(move |_this, _ev, _window, cx| {
                            cx.emit(AcpViewEvent::NavigateToSession(source_id.clone()));
                        })),
                )
                .into_any_element()
        });

        // 底层保留全部 responder，但交互按队列串行：只显示并允许处理队首，
        // 回执后的下一份快照移除它，下一张卡才会出现（与 Codex App 一致）。
        let active_permission = self.permissions.first();
        let permission_is_submitting = self.permission_submitting.is_some();
        let permission_buttons = |card: &PendingPermission| {
            if permission_is_submitting {
                return h_flex()
                    .h(px(36.))
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(Spinner::new().xsmall().color(muted))
                    .child("处理中…")
                    .into_any_element();
            }
            let tool_call_id = card.tool_call_id.clone();
            let primary_ix = card.options.iter().position(|o| {
                matches!(
                    o.kind,
                    PermissionOptionKindView::AllowOnce | PermissionOptionKindView::AllowAlways
                )
            });
            let mut allow_buttons = h_flex().w_full().gap_2().items_center().flex_wrap();
            let mut reject_buttons = h_flex().w_full().gap_2().items_center().flex_wrap();
            if let Some(pix) = primary_ix {
                let name = card.options[pix].name.clone();
                let option_id = card.options[pix].option_id.clone();
                let tool_call_id = tool_call_id.clone();
                // 主按钮改胶囊 + hover 时轻微上浮带阴影——批准是这张卡最想让人点的
                // 动作，得比其余选项更有「弹一下」的手感，不只是纯色块换个透明度。
                allow_buttons = allow_buttons.child(
                    div()
                        .id(format!("acp-perm-primary-{option_id}"))
                        .relative()
                        .h(px(36.))
                        .px_4()
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(gpui::rgb(ui_theme::green()))
                        .text_color(gpui::rgb(ui_theme::on_accent()))
                        .text_sm()
                        .font_semibold()
                        .cursor_pointer()
                        .shadow_sm()
                        .hover(|d| d.opacity(0.9).shadow_md().top(px(-1.)))
                        .child(format!("{name} ⌘⏎"))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.pick_permission(&tool_call_id, &option_id, cx);
                        })),
                );
            }
            for (ix, opt) in card.options.iter().enumerate() {
                if Some(ix) == primary_ix {
                    continue;
                }
                let danger = matches!(
                    opt.kind,
                    PermissionOptionKindView::RejectOnce | PermissionOptionKindView::RejectAlways
                );
                let option_id = opt.option_id.clone();
                let tool_call_id = tool_call_id.clone();
                // 次级选项也改软底胶囊：danger 用红色调软底，其余用中性灰软底,
                // 不再是空心线框——跟主按钮的实心胶囊放一起才是同一套语言，
                // 而不是「一个填色一个描边」的两套风格拼在一起。
                let bg_u32 = if danger {
                    ui_theme::red()
                } else {
                    ui_theme::text_muted()
                };
                let button = div()
                    .id(format!("acp-perm-opt-{option_id}"))
                    .h(px(36.))
                    .px_3p5()
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(ui_theme::tint(bg_u32, 0x1c))
                    .text_sm()
                    .cursor_pointer()
                    .when(danger, |d| d.text_color(gpui::rgb(ui_theme::red())))
                    .hover(|d| d.bg(ui_theme::tint(bg_u32, 0x30)))
                    .child(opt.name.clone())
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.pick_permission(&tool_call_id, &option_id, cx);
                    }));
                if danger {
                    reject_buttons = reject_buttons.child(button);
                } else {
                    allow_buttons = allow_buttons.child(button);
                }
            }
            v_flex()
                .w_full()
                .gap_2()
                .child(allow_buttons)
                .child(reject_buttons)
                .into_any_element()
        };

        // GPUI 的可变高虚拟列表只构建视口与 overdraw 范围内的项。
        // 每项的渲染通过 Entity 回到视图，保留工具卡的展开/收起交互。
        let sticky_prompt = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .find(|(ix, entry)| {
                is_user_entry(entry)
                    && !matches!(entry, AcpEntry::User(text) if is_interrupt_marker(text))
                    && self.list_state.item_is_above_viewport(*ix) == Some(true)
            })
            .map(|(ix, entry)| {
                let summary = match entry {
                    AcpEntry::User(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
                    AcpEntry::UserWithImages { text, images } => {
                        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                        if text.is_empty() {
                            format!("{} 张图片", images.len())
                        } else {
                            format!("{text} · {} 张图片", images.len())
                        }
                    }
                    _ => unreachable!(),
                };
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .justify_center()
                    .px_4()
                    .child(
                        h_flex()
                            .id("acp-sticky-prompt")
                            .w_full()
                            .max_w(px(1040.))
                            .h(px(38.))
                            .px_3()
                            .gap_2()
                            .items_center()
                            .border_b_1()
                            .border_color(t.border)
                            .bg(ui_theme::glass_floating())
                            .cursor_pointer()
                            .hover(|row| row.bg(gpui::rgb(ui_theme::bg_hover())))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .text_color(gpui::rgb(ui_theme::text_mid()))
                                    .child(summary),
                            )
                            .child(div().flex_shrink_0().text_xs().text_color(muted).child("↑"))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.list_state.scroll_to_reveal_item(ix);
                                cx.notify();
                            })),
                    )
            });
        let jump_to_latest = self.viewing_history.then(|| {
            h_flex()
                .absolute()
                .bottom(px(14.))
                .left_0()
                .right_0()
                .justify_center()
                .child(
                    h_flex()
                        .id("acp-jump-to-latest")
                        .h(px(36.))
                        .px_3()
                        .gap_2()
                        .items_center()
                        .rounded_lg()
                        .border_1()
                        .border_color(t.border)
                        .bg(ui_theme::glass_floating())
                        .shadow_sm()
                        .cursor_pointer()
                        .hover(|button| button.bg(gpui::rgb(ui_theme::bg_hover())))
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child("回到最新"),
                        )
                        .child(div().text_sm().text_color(muted).child("↓"))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.viewing_history = false;
                            this.list_state.set_follow_mode(FollowMode::Tail);
                            cx.notify();
                        }))
                        .with_animation(
                            "acp-jump-to-latest-enter",
                            Animation::new(std::time::Duration::from_millis(160)),
                            |button, delta| button.opacity(delta),
                        ),
                )
        });
        let view = cx.entity();
        let list = virtual_list(self.list_state.clone(), move |i, _window, app| {
            view.update(app, |this, cx| {
                let t = cx.theme();
                let muted = t.muted_foreground;
                let active_permission_tool_id = this
                    .permissions
                    .first()
                    .map(|card| card.tool_call_id.as_str());
                let timed_answer_ix = this.last_turn_duration_ms.and_then(|_| {
                    this.entries.iter().rposition(|entry| {
                        matches!(entry, AcpEntry::Assistant { thought: false, .. })
                    })
                });
                let final_answer = is_turn_final_answer(&this.entries, i);
                let process_group = process_group_for_entry(&this.entries, i);
                let process_expanded = process_group
                    .is_some_and(|group| this.expanded_process_groups.contains(&group.first));
                if process_group.is_some_and(|group| group.first != i) && !process_expanded {
                    return div().into_any_element();
                }
                let entry = &this.entries[i];
                let mut el: gpui::AnyElement = match entry {
                    // agent 回显的「中断」标记不是用户说的话，别套成气泡——
                    // 那会读成「用户发了一条叫 [Request interrupted...] 的消息」。
                    AcpEntry::User(text) if is_interrupt_marker(text) => h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .my_1()
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .child(div().text_xs().text_color(muted).child("已中断"))
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .into_any_element(),
                    // 用户气泡右对齐限宽（对齐设计稿）：整行铺满时跟 agent 正文
                    // 混成一片，看不出谁在说话。
                    AcpEntry::User(text) => h_flex()
                        .w_full()
                        .justify_end()
                        .child(
                            div()
                                .max_w(gpui::relative(0.72))
                                .px_4()
                                .py_2p5()
                                .rounded_lg()
                                .border_1()
                                .border_color(ui_theme::tint(ui_theme::accent(), 0x2c))
                                .bg(ui_theme::tint(ui_theme::accent(), 0x14))
                                .hover(|bubble| {
                                    bubble
                                        .border_color(ui_theme::tint(ui_theme::accent(), 0x52))
                                        .bg(ui_theme::tint(ui_theme::accent(), 0x20))
                                })
                                .text_sm()
                                .child(smelt_ui::markdown_mermaid::markdown_view(
                                    ("acp-user-md", i),
                                    markdown_text_for_cwd(text, this.cwd.as_deref()),
                                )),
                        )
                        .into_any_element(),
                    AcpEntry::UserWithImages { text, images } => {
                        let mut content = v_flex().gap_2();
                        if !text.trim().is_empty() {
                            content = content.child(smelt_ui::markdown_mermaid::markdown_view(
                                ("acp-user-images-md", i),
                                markdown_text_for_cwd(text, this.cwd.as_deref()),
                            ));
                        }
                        let mut image_strip = h_flex().gap_2().flex_wrap();
                        for image_ix in 0..images.len() {
                            if let Some(image) = this.rendered_images.get(&(i, image_ix)).cloned() {
                                let preview_image = image.clone();
                                image_strip = image_strip.child(
                                    div()
                                        .id(("acp-sent-image", i * 1024 + image_ix))
                                        .overflow_hidden()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(t.border)
                                        .cursor_pointer()
                                        .hover(|image| {
                                            image.border_color(ui_theme::tint(
                                                ui_theme::accent(),
                                                0x72,
                                            ))
                                        })
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            let _ = this;
                                            cx.emit(AcpViewEvent::PreviewImage(
                                                preview_image.clone(),
                                            ));
                                        }))
                                        .child(gpui::img(image).h(px(160.)).max_w(px(280.))),
                                );
                            }
                        }
                        h_flex()
                            .w_full()
                            .justify_end()
                            .child(
                                div()
                                    .max_w(gpui::relative(0.8))
                                    .px_3()
                                    .py_3()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(ui_theme::tint(ui_theme::accent(), 0x2c))
                                    .bg(ui_theme::tint(ui_theme::accent(), 0x14))
                                    .hover(|bubble| {
                                        bubble
                                            .border_color(ui_theme::tint(ui_theme::accent(), 0x52))
                                            .bg(ui_theme::tint(ui_theme::accent(), 0x20))
                                    })
                                    .text_sm()
                                    .child(content.child(image_strip)),
                            )
                            .into_any_element()
                    }
                    AcpEntry::Assistant {
                        text,
                        thought: true,
                    } => {
                        let expanded = this.expanded_thoughts.contains(&i);
                        let preview = strip_thought_heading_markers(
                            text.lines()
                                .find(|line| !line.trim().is_empty())
                                .unwrap_or("正在思考…")
                                .trim(),
                        )
                        .to_string();
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .id(("acp-thought-toggle", i))
                                    .w_full()
                                    .min_w_0()
                                    .min_h(px(24.))
                                    .gap_2()
                                    .items_center()
                                    .rounded_md()
                                    .px_2()
                                    .cursor_pointer()
                                    .hover(|row| row.bg(gpui::rgb(ui_theme::bg_hover())))
                                    .child(
                                        div()
                                            .w(px(12.))
                                            .text_xs()
                                            .text_color(muted)
                                            .child(if expanded { "▾" } else { "▸" }),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .font_medium()
                                            .text_color(muted)
                                            .child("思考"),
                                    )
                                    .when(!expanded, |row| {
                                        row.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_color(muted)
                                                .text_xs()
                                                .truncate()
                                                .child(preview),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        if !this.expanded_thoughts.remove(&i) {
                                            this.expanded_thoughts.insert(i);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(expanded, |col| {
                                col.child(
                                    div()
                                        .min_w_0()
                                        .px_2()
                                        .pt_1()
                                        .pb_2()
                                        .text_sm()
                                        .text_color(muted)
                                        .italic()
                                        .child(smelt_ui::markdown_mermaid::markdown_view(
                                            ("acp-thought-md", i),
                                            markdown_text_for_cwd(text, this.cwd.as_deref()),
                                        )),
                                )
                            })
                            .into_any_element()
                    }
                    AcpEntry::Assistant {
                        text,
                        thought: false,
                    } => {
                        let answer = v_flex()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .text_color(t.foreground)
                            .child(smelt_ui::markdown_mermaid::markdown_view(
                                ("acp-md", i),
                                markdown_text_for_cwd(text, this.cwd.as_deref()),
                            ))
                            .when(final_answer, |col| {
                                col.child(
                                    h_flex()
                                        .pt_1()
                                        .gap_1()
                                        .child(
                                            Clipboard::new(("acp-copy-answer", i))
                                                .value(text.clone())
                                                .tooltip("复制回答"),
                                        )
                                        .child(
                                            Button::new(("acp-continue-new-session", i))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Network)
                                                .tooltip("在新会话中继续")
                                                .on_click(cx.listener(
                                                    move |this, _ev, _window, cx| {
                                                        let source = AcpForkOrigin {
                                                            session_id: this.sid.clone(),
                                                            title: this.auto_title().unwrap_or_else(
                                                                || this.agent.label().to_string(),
                                                            ),
                                                        };
                                                        let prompt = build_handoff_prompt(
                                                            &this.entries,
                                                            i,
                                                            &source.title,
                                                            this.cwd.as_deref(),
                                                        );
                                                        let mut config_values: Vec<_> = this
                                                            .config_options
                                                            .iter()
                                                            .filter_map(|config| {
                                                                config.options.iter().find_map(
                                                                    |(value, name)| {
                                                                        (name
                                                                            == &config.current_name)
                                                                            .then(|| {
                                                                                (
                                                                                    config
                                                                                        .config_id
                                                                                        .clone(),
                                                                                    value.clone(),
                                                                                )
                                                                            })
                                                                    },
                                                                )
                                                            })
                                                            .collect();
                                                        if let Some(model) = &this.model {
                                                            if let Some((value, _)) = model
                                                                .options
                                                                .iter()
                                                                .find(|(_, name)| {
                                                                    name == &model.current_name
                                                                })
                                                            {
                                                                config_values.push((
                                                                    model.config_id.clone(),
                                                                    value.clone(),
                                                                ));
                                                            }
                                                        }
                                                        cx.emit(
                                                            AcpViewEvent::ContinueInNewSession(
                                                                AcpHandoffRequest {
                                                                    source,
                                                                    cwd: this.cwd.clone(),
                                                                    agent: this.agent,
                                                                    launch: this.launch.clone(),
                                                                    refresh_launch_from_settings: this
                                                                        .refresh_launch_from_settings,
                                                                    profile_id: this.profile_id.clone(),
                                                                    config_values,
                                                                    prompt,
                                                                },
                                                            ),
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                            })
                            .when(!final_answer, |col| col.text_color(muted).text_xs());
                        if timed_answer_ix == Some(i) {
                            v_flex()
                                .w_full()
                                .gap_2()
                                .child(div().text_xs().text_color(muted).child(format!(
                                    "耗时 {}",
                                    format_duration(this.last_turn_duration_ms.unwrap_or_default())
                                )))
                                .child(answer)
                                .into_any_element()
                        } else {
                            answer.into_any_element()
                        }
                    }
                    AcpEntry::ToolCall {
                        id,
                        title,
                        kind,
                        status,
                        output,
                    } => {
                        let accent = tool_accent_color(kind);
                        let accent_u32 = tool_accent_u32(kind);
                        let failed = matches!(status, ToolCallStatus::Failed);
                        // 失败时左侧色条改红——出错是比「这是哪种工具」更急的信息，
                        // 状态色盖过身份色。hover 高亮边框也得用同一个 u32，否则
                        // 悬浮时四周描边是工具身份色（比如 Edit 的紫）、左侧色条
                        // 却还是红，两截颜色对不上，看着像描边套错了。
                        let bar_u32 = if failed { ui_theme::red() } else { accent_u32 };
                        let bar_color: gpui::Rgba = gpui::rgb(bar_u32);
                        let (status_dot, status_label): (gpui::Hsla, &str) = match status {
                            ToolCallStatus::Pending => (t.muted_foreground, "待执行"),
                            ToolCallStatus::InProgress => {
                                (gpui::rgb(ui_theme::blue()).into(), "执行中")
                            }
                            ToolCallStatus::Completed => {
                                (gpui::rgb(ui_theme::green()).into(), "完成")
                            }
                            ToolCallStatus::Failed => (gpui::rgb(ui_theme::red()).into(), "失败"),
                        };

                        // diff 汇总统计：头部摘要显示全部 diff 块加总的增删行数，
                        // 跟截图里 Edit 卡片右上角「+18 -4」的形态对齐。
                        let diff_totals: Vec<(usize, usize)> = this
                            .rendered_diffs
                            .get(id)
                            .into_iter()
                            .flatten()
                            .filter_map(|cached| {
                                cached.as_ref().map(|diff| (diff.added, diff.removed))
                            })
                            .collect();
                        let has_diff = !diff_totals.is_empty();
                        let (total_added, total_removed) = diff_totals
                            .iter()
                            .fold((0usize, 0usize), |(a, r), (da, dr)| (a + da, r + dr));
                        let has_pending_permission = active_permission_tool_id == Some(id.as_str());
                        let card_expanded =
                            this.tool_card_is_expanded(id, *status, has_pending_permission);

                        // diff / 状态角标都改成 Discord 那种圆角软底色小药丸，
                        // 而不是裸文字——同样的信息，胶囊比平铺文字更有「标签」的
                        // 活泼感，也跟下面的工具名药丸呼应成一套视觉语言。
                        let header_right: gpui::AnyElement = if has_diff {
                            h_flex()
                                .gap_1p5()
                                .child(
                                    div()
                                        .px_1p5()
                                        .rounded_full()
                                        .bg(ui_theme::tint(ui_theme::green(), 0x22))
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(gpui::rgb(ui_theme::green()))
                                        .child(format!("+{total_added}")),
                                )
                                .child(
                                    div()
                                        .px_1p5()
                                        .rounded_full()
                                        .bg(ui_theme::tint(ui_theme::red(), 0x22))
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(gpui::rgb(ui_theme::red()))
                                        .child(format!("-{total_removed}")),
                                )
                                .into_any_element()
                        } else if matches!(status, ToolCallStatus::Completed) {
                            // 完成是默认预期结果，一排卡片全打「完成」绿点纯噪音——
                            // 只在异常态（进行中/失败/待执行）才需要占用视觉注意力。
                            div().into_any_element()
                        } else {
                            let pill = h_flex()
                                .gap_1p5()
                                .items_center()
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .bg(ui_theme::tint(
                                    match status {
                                        ToolCallStatus::InProgress => ui_theme::blue(),
                                        ToolCallStatus::Failed => ui_theme::red(),
                                        _ => ui_theme::text_muted(),
                                    },
                                    0x22,
                                ))
                                .child(div().size_1p5().rounded_full().bg(status_dot))
                                .child(div().text_xs().text_color(status_dot).child(status_label));
                            if matches!(status, ToolCallStatus::InProgress) {
                                // 「执行中」呼吸一下——进行时的动作要有活的感觉，
                                // 完全静止的胶囊看着像卡死。
                                pill.with_animation(
                                    "acp-tool-status-breathe",
                                    Animation::new(std::time::Duration::from_millis(1400))
                                        .repeat(),
                                    |this, delta| {
                                        let wave = (delta * std::f32::consts::TAU).sin() * 0.5
                                            + 0.5;
                                        this.opacity(0.65 + wave * 0.35)
                                    },
                                )
                                .into_any_element()
                            } else {
                                pill.into_any_element()
                            }
                        };

                        let id_for_toggle = id.clone();
                        let status_for_toggle = *status;
                        let mut card = v_flex()
                            .relative()
                            .w_full()
                            .overflow_hidden()
                            .rounded_lg()
                            .border_1()
                            // 左边框跟下面的彩色色条挤在同一条边上：灰线会从色条
                            // 两端露出一小截，看着像镶了道多余的灰边。色条本身已经
                            // 把左边这条线的视觉职责接管了，这里直接去掉。
                            .border_l_0()
                            .border_color(t.border)
                            .bg(ui_theme::glass_card())
                            .hover(|card| {
                                card.border_color(ui_theme::tint(bar_u32, 0x66))
                                    .shadow_md()
                            })
                            .child(
                                // 工具卡左侧色条：跟标题/图标同一套 accent 色，一眼
                                // 就能在一长串卡片里扫出「这是执行/读/改/查」,不用
                                // 逐条读文字。失败态额外加粗一点，异常更抓眼。
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(px(if failed { 3. } else { 2. }))
                                    .bg(bar_color),
                            )
                            .child(
                                h_flex()
                                    .id(("acp-tool-card-toggle", i))
                                    .px_3()
                                    .py_1p5()
                                    .gap_2()
                                    .items_center()
                                    .cursor_pointer()
                                    .hover(|row| row.bg(gpui::rgb(ui_theme::bg_hover())))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(move |this, _ev, _window, cx| {
                                            this.toggle_tool_card(
                                                id_for_toggle.clone(),
                                                status_for_toggle,
                                                has_pending_permission,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child(
                                        div()
                                            .w(px(10.))
                                            .text_xs()
                                            .text_color(muted)
                                            .child(if card_expanded { "▾" } else { "▸" }),
                                    )
                                    .child(
                                        // 图标套一个同色软底的圆角徽章，而不是裸图标——
                                        // Discord 那种带色块的小 icon chip，比纯线框图标
                                        // 更有「彩色标签」的活泼感，扫描时也更抓眼。
                                        div()
                                            .flex_shrink_0()
                                            .size(px(20.))
                                            .rounded_md()
                                            .bg(ui_theme::tint(accent_u32, 0x24))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(Icon::new(tool_kind_icon(kind)).size(px(12.)).text_color(accent)),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(ui_theme::tint(accent_u32, 0x18))
                                            .text_xs()
                                            .font_semibold()
                                            .text_color(accent)
                                            .child(tool_kind_label(kind)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .font_family("monospace")
                                            .text_color(muted)
                                            .truncate()
                                            .child(strip_kind_prefix(title, kind).to_string()),
                                    )
                                    .child(header_right),
                            );
                        if card_expanded {
                            let mut rendered_output_part = false;
                            for (part_ix, part) in output.iter().enumerate() {
                                card = match part {
                                    ToolOutputPart::Diff { path, .. } => {
                                        rendered_output_part = true;
                                        let cached = this
                                            .rendered_diffs
                                            .get(id)
                                            .and_then(|parts| parts.get(part_ix))
                                            .and_then(Option::as_ref);
                                        card.child(
                                            v_flex()
                                                .px_4()
                                                .pb_3()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(path.clone()),
                                                )
                                                .children(cached.map(|diff| {
                                                    render_diff_lines(
                                                        &diff.lines,
                                                        (i, part_ix),
                                                        t.border,
                                                        t.muted_foreground,
                                                    )
                                                })),
                                        )
                                    }
                                    ToolOutputPart::Text(text) if !text.trim().is_empty() => {
                                        rendered_output_part = true;
                                        // adapter 把工具输出包在 markdown 围栏里（```console…```），
                                        // 当纯文本渲染会把 ``` 直接显示出来。剥掉再展示。
                                        let body = strip_code_fence(text);
                                        let lines: Vec<&str> = body.lines().collect();
                                        let total = lines.len();
                                        let key = id.to_string();
                                        let expanded = this.expanded_tools.contains(&key);
                                        // 默认只出前 8 行：以前是 max_h + overflow_hidden，
                                        // 内容被硬切掉且没有任何展开入口，等于看不到全部。
                                        let shown =
                                            if expanded || total <= TOOL_OUTPUT_PREVIEW_LINES {
                                                body.to_string()
                                            } else {
                                                lines[..TOOL_OUTPUT_PREVIEW_LINES].join("\n")
                                            };
                                        let need_toggle = total > TOOL_OUTPUT_PREVIEW_LINES;
                                        // 真正的控制台输出（bash stdout、文件内容……）保持等宽纯文本，
                                        // 星号、井号都是内容本身，不能被当 markdown 解析。但像
                                        // task_complete 这类没有专门 kind、靠 agent 自己写一段总结
                                        // 陈述的工具（落到 `ToolKind::Other`），内容本来就是按 markdown
                                        // 写的（`##`/`**`/列表），纯文本渲染只会把这些符号原样吐出来。
                                        let body_el: gpui::AnyElement =
                                            if matches!(kind, ToolKind::Other) {
                                                smelt_ui::markdown_mermaid::markdown_view(
                                                    ("acp-tool-output-md", i * 100 + part_ix),
                                                    shown,
                                                )
                                                .into_any_element()
                                            } else {
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .font_family("monospace")
                                                    .child(shown)
                                                    .into_any_element()
                                            };
                                        card.child(
                                            v_flex()
                                                .px_4()
                                                .pb_3()
                                                .gap_1()
                                                .child(body_el)
                                                .when(need_toggle, |d| {
                                                    let key = key.clone();
                                                    d.child(
                                                    div()
                                                        .id(("acp-tool-toggle", i * 100 + part_ix))
                                                        .text_xs()
                                                        .text_color(gpui::rgb(ui_theme::blue()))
                                                        .cursor_pointer()
                                                        .hover(|d| d.opacity(0.8))
                                                        .child(if expanded {
                                                            "收起".to_string()
                                                        } else {
                                                            format!("展开全部 {total} 行")
                                                        })
                                                        .on_mouse_down(
                                                            gpui::MouseButton::Left,
                                                            cx.listener(
                                                                move |this, _ev, _window, cx| {
                                                                    if !this
                                                                        .expanded_tools
                                                                        .remove(&key)
                                                                    {
                                                                        this.expanded_tools
                                                                            .insert(key.clone());
                                                                    }
                                                                    cx.stop_propagation();
                                                                    cx.notify();
                                                                },
                                                            ),
                                                        ),
                                                )
                                                }),
                                        )
                                    }
                                    ToolOutputPart::Text(_) => card,
                                };
                            }
                            if !rendered_output_part {
                                card = card.child(
                                    div()
                                        .px_4()
                                        .pb_3()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("无可展示输出"),
                                );
                            }
                        }
                        card.into_any_element()
                    }
                    AcpEntry::Divider(label) => h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .my_1()
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .child(div().text_xs().text_color(muted).child(label.clone()))
                        .child(div().flex_1().h(px(1.)).bg(t.border))
                        .into_any_element(),
                };
                if let Some(group) = process_group
                    && group.first == i
                {
                    let group_key = group.first;
                    let mut header = h_flex()
                        .id(("acp-process-group", group.first))
                        .w_full()
                        .min_h(px(32.))
                        .px_2p5()
                        .gap_2()
                        .items_center()
                        .rounded_full()
                        .bg(ui_theme::overlay(0x10))
                        .cursor_pointer()
                        .hover(|row| row.bg(gpui::rgb(ui_theme::bg_hover())))
                        .child(
                            div()
                                .w(px(12.))
                                .text_xs()
                                .text_color(muted)
                                .child(if process_expanded { "▾" } else { "▸" }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(gpui::rgb(ui_theme::text_mid()))
                                .child("执行过程"),
                        )
                        .child(div().text_xs().text_color(muted).child(if group.tools > 0 {
                            format!("{} 步 · {} 个工具调用", group.steps, group.tools)
                        } else {
                            format!("{} 步", group.steps)
                        }))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            if !this.expanded_process_groups.remove(&group_key) {
                                this.expanded_process_groups.insert(group_key);
                            }
                            cx.notify();
                        }));
                    if group.failed > 0 {
                        // 之前是裸红字飘在行尾；跟其余地方统一成软底小胶囊，
                        // 这一条折叠摘要本身也不再是没有任何底色的纯文字行。
                        header = header.child(div().flex_1()).child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .bg(ui_theme::tint(ui_theme::red(), 0x22))
                                .text_xs()
                                .font_semibold()
                                .text_color(gpui::rgb(ui_theme::red()))
                                .child(format!("{} 项失败", group.failed)),
                        );
                    }
                    el = if process_expanded {
                        v_flex()
                            .w_full()
                            .gap_2()
                            .child(header)
                            .child(el)
                            .into_any_element()
                    } else {
                        header.into_any_element()
                    };
                }
                // `gpui::list` 不像 flex 容器那样处理 `gap`；间距必须属于
                // 虚拟项本身，否则测得的高度不包含消息间的留白。
                let bottom = match entry {
                    AcpEntry::ToolCall { .. } => 8.,
                    AcpEntry::Assistant { thought: true, .. } => 4.,
                    _ => 16.,
                };
                h_flex()
                    .w_full()
                    .justify_center()
                    .px_4()
                    .pb(px(bottom))
                    .child(div().w_full().max_w(px(1040.)).child(el))
                    .into_any_element()
            })
        })
        .w_full()
        .flex_1()
        .min_h_0()
        .pt_4();

        // 「正在思考」占位：回合在跑、但 agent 还没吐出正文（最后一条不是
        // assistant 气泡）时，消息流末尾必须有活的东西。否则从按下发送到首字
        // 落地之间是一整屏纯黑——Copilot 这类首字延迟长的 agent 上看着像卡死。
        // 用 Spinner 而不是「已 N 秒」：GPUI 没有定时重绘，秒数会僵在原地，
        // 反而更像死了；spinner 自带动画帧，转着就说明进程还在。
        let show_thinking = matches!(self.phase, AcpPhase::Running)
            && !matches!(self.entries.last(), Some(AcpEntry::Assistant { .. }));

        // 审批是输入动作，不散落进历史工具卡；统一固定在 composer 上方，
        // 队首切换时卡片位置不动，用户也能看见还有多少项等待处理。
        let permission = active_permission.map(|pending| {
            let remaining = self.permissions.len();
            let details = match &pending.details {
                ApprovalDetailsView::Command {
                    command,
                    cwd,
                    reason,
                } => v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_normal()
                            .text_sm()
                            .font_family("monospace")
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child(command.clone()),
                    )
                    .children(
                        reason
                            .as_ref()
                            .map(|reason| div().text_xs().text_color(muted).child(reason.clone())),
                    )
                    .children(cwd.as_ref().map(|cwd| {
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(muted)
                            .child(format!("工作目录：{cwd}"))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::FileChange { reason, grant_root } => v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui::rgb(ui_theme::text_mid()))
                            .child(reason.clone().unwrap_or_else(|| pending.question.clone())),
                    )
                    .children(grant_root.as_ref().map(|root| {
                        div()
                            .text_xs()
                            .font_family("monospace")
                            .text_color(muted)
                            .child(format!("授权目录：{root}"))
                    }))
                    .into_any_element(),
                ApprovalDetailsView::Permissions { summary } => div()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(summary.clone())
                    .into_any_element(),
                ApprovalDetailsView::Generic => div()
                    .text_sm()
                    .text_color(gpui::rgb(ui_theme::text_mid()))
                    .child(pending.question.clone())
                    .into_any_element(),
            };
            v_flex()
                .w_full()
                .items_center()
                .px_4()
                .pt_3()
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .max_w(px(1040.))
                        .p_4()
                        .gap_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(gpui::rgb(ui_theme::yellow()))
                        .bg(ui_theme::tint(ui_theme::yellow(), 0x0c))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    // 需要批准的点缀一个扩散的 ping 环——跟系统级
                                    // 通知红点常见的那种脉冲一样，比静止圆点更有
                                    // 「这里正等你」的紧迫感，而不是容易被忽略的
                                    // 一个死圆点。
                                    div()
                                        .relative()
                                        .size_2()
                                        .child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .rounded_full()
                                                .bg(gpui::rgb(ui_theme::yellow()))
                                                .with_animation(
                                                    "acp-permission-ping",
                                                    Animation::new(
                                                        std::time::Duration::from_millis(1600),
                                                    )
                                                    .repeat(),
                                                    |this, delta| {
                                                        let scale = 1.0 + delta * 1.6;
                                                        this.opacity((1.0 - delta).max(0.0) * 0.7)
                                                            .size(px(8. * scale))
                                                    },
                                                ),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .size_2()
                                                .rounded_full()
                                                .bg(gpui::rgb(ui_theme::yellow())),
                                        ),
                                )
                                .child(div().text_sm().font_semibold().child("需要批准"))
                                .child(div().flex_1())
                                .when(remaining > 1, |row| {
                                    row.child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(ui_theme::overlay(0x18))
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!("{remaining} 项待处理")),
                                    )
                                }),
                        )
                        .child(details)
                        .child(permission_buttons(pending)),
                )
                .into_any_element()
        });

        // 选择题卡片：message + 逐字段按钮组；单字段单选点击即提交，
        // 其余选齐后亮「提交」；「跳过」丢卡（responder Drop 回 Cancel）。
        let elicitation = self.elicitation.as_ref().map(|card| {
            let ready = self.elicit_ready(cx);
            let mut body = v_flex()
                .mx_4()
                .mb_3()
                .p_4()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(gpui::rgb(ui_theme::yellow()))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().font_semibold().child("等你选择"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(card.message.clone()),
                        ),
                );
            let multi_field = card.fields.len() > 1
                || card
                    .fields
                    .first()
                    .is_some_and(|f| !matches!(f.kind, ElicitFieldKindView::Select(_)));
            let show_footer = multi_field
                && !matches!(
                    card.fields.as_slice(),
                    [smelt_core::acp_session::ElicitFieldView {
                        kind: ElicitFieldKindView::ExternalUrl(_),
                        ..
                    }]
                );
            for (fix, field) in card.fields.iter().enumerate() {
                if let ElicitFieldKindView::ExternalUrl(url) = &field.kind {
                    let url = url.clone();
                    body = body.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(field.title.clone()))
                            .child(
                                div()
                                    .id(("acp-elicit-url", fix))
                                    .px_3()
                                    .py_2()
                                    .rounded_lg()
                                    .bg(gpui::rgb(ui_theme::blue()))
                                    .text_color(gpui::white())
                                    .text_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.opacity(0.85))
                                    .child("打开并继续")
                                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                                        cx.open_url(&url);
                                        this.submit_elicitation(cx);
                                    })),
                            ),
                    );
                    continue;
                }
                if let ElicitFieldKindView::Text { secret } = &field.kind {
                    let input = self.elicitation_inputs.get(&fix).cloned();
                    body = body.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{}{}",
                                field.title,
                                if *secret { "（保密）" } else { "" }
                            )))
                            .children(input.map(|input| Input::new(&input).w_full())),
                    );
                    continue;
                }
                let (options, is_multi) = match &field.kind {
                    ElicitFieldKindView::Select(o) => (o, false),
                    ElicitFieldKindView::MultiSelect(o) => (o, true),
                    ElicitFieldKindView::Text { .. } => unreachable!(),
                    ElicitFieldKindView::ExternalUrl(_) => unreachable!(),
                };
                let chosen = card.chosen.get(&fix).cloned().unwrap_or_default();
                let mut row = h_flex().gap_2().flex_wrap();
                for (oix, opt) in options.iter().enumerate() {
                    let selected = chosen.contains(&oix);
                    row = row.child(
                        div()
                            .id(("acp-elicit-opt", fix * 1000 + oix))
                            .px_3()
                            .py_1()
                            .rounded_lg()
                            .border_1()
                            .border_color(if selected {
                                gpui::rgb(ui_theme::yellow()).into()
                            } else {
                                t.border
                            })
                            .when(selected, |d| d.bg(t.muted))
                            .text_sm()
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.85))
                            .child(opt.label.clone())
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.pick_elicit_option(fix, oix, cx);
                            })),
                    );
                }
                body = body.child(
                    v_flex()
                        .gap_1()
                        .when(multi_field, |d| {
                            d.child(div().text_xs().text_color(muted).child(format!(
                                "{}{}",
                                field.title.clone(),
                                if is_multi { "（可多选）" } else { "" }
                            )))
                        })
                        .child(row),
                );
            }
            if show_footer {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("acp-elicit-submit")
                                .px_3()
                                .py_1()
                                .rounded_lg()
                                .text_sm()
                                .when(ready, |d| {
                                    d.bg(gpui::rgb(ui_theme::green()))
                                        .text_color(gpui::white())
                                        .cursor_pointer()
                                        .hover(|x| x.opacity(0.85))
                                })
                                .when(!ready, |d| {
                                    d.border_1().border_color(t.border).text_color(muted)
                                })
                                .child("提交")
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    if this.elicit_ready(cx) {
                                        this.submit_elicitation(cx);
                                    }
                                })),
                        )
                        .child(
                            div()
                                .id("acp-elicit-skip")
                                .px_3()
                                .py_1()
                                .rounded_lg()
                                .text_sm()
                                .text_color(muted)
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.8))
                                .child("跳过（改用文字回答）")
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    this.dismiss_elicitation(cx);
                                })),
                        ),
                );
            }
            body
        });

        // 胶囊优先显示真实模型名；协议没给就退回适配器名——但要让人看得出
        // 那是「适配器」不是模型，不能拿包名冒充模型。
        let (pill_text, pill_is_model) = match &self.model {
            Some(m) => (m.current_name.clone(), true),
            None => (self.agent_label(), false),
        };
        // 候选模型（协议给了才有）：胶囊变成可点下拉，点一项即切。
        let model_options: Vec<(String, String)> = self
            .model
            .as_ref()
            .map(|m| m.options.clone())
            .unwrap_or_default();
        let current_model = self.model.as_ref().map(|m| m.current_name.clone());
        let model_config_id = self.model.as_ref().map(|m| m.config_id.clone());
        let config_options = self.config_options.clone();
        // 补全弹层画在输入框上方，并与 composer 共用宽度和容器。它仍在正常流中，
        // 因而不会被窗口底边裁掉，但视觉上不再是一条横贯消息区的列表。
        let completion_bar = self.completion.as_ref().map(|popup| {
            let mut list = v_flex()
                .id("acp-completion")
                .w_full()
                .max_w(px(1040.))
                .max_h(px(260.))
                .overflow_y_scroll()
                .track_scroll(&self.completion_scroll)
                .mb_2()
                .rounded_lg()
                .border_1()
                .border_color(t.border)
                .bg(ui_theme::glass_floating())
                .shadow_lg();
            for (ix, item) in popup.items.iter().enumerate() {
                let selected = ix == popup.selected;
                let label_color = if selected {
                    gpui::rgb(ui_theme::text_bright())
                } else {
                    gpui::rgb(ui_theme::text_mid())
                };
                let label = if let Some(range) = item.match_range.clone() {
                    h_flex()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family("monospace")
                        .text_color(label_color)
                        .child(item.label[..range.start].to_string())
                        .child(
                            div()
                                .font_semibold()
                                .text_color(gpui::rgb(ui_theme::accent()))
                                .child(item.label[range.clone()].to_string()),
                        )
                        .child(item.label[range.end..].to_string())
                        .into_any_element()
                } else {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_family("monospace")
                        .text_color(label_color)
                        .child(item.label.clone())
                        .into_any_element()
                };
                list = list.child(
                    h_flex()
                        .id(("acp-completion-item", ix))
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .when(selected, |d| d.bg(ui_theme::tint(ui_theme::accent(), 0x38)))
                        .cursor_pointer()
                        .hover(move |d| {
                            d.bg(if selected {
                                ui_theme::tint(ui_theme::accent(), 0x48)
                            } else {
                                ui_theme::overlay(0x20)
                            })
                        })
                        .child(label)
                        .when(!item.hint.is_empty(), |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(if selected { t.foreground } else { muted })
                                    .truncate()
                                    .child(item.hint.clone()),
                            )
                        })
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            if let Some(popup) = &mut this.completion {
                                popup.selected = ix;
                            }
                            this.accept_completion(window, cx);
                        })),
                );
            }
            list.child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(t.border)
                    .text_xs()
                    .text_color(muted)
                    .child("↑↓ 选择   Enter/Tab 插入   Esc 关闭"),
            )
        });

        let input_row = self.input.as_ref().map(|input| {
            let usage_pill = self.usage.map(|(used, size)| {
                // 协议异常或旧 daemon 的累计口径也不能把布局撑成几千个百分点。
                let pct = (((used as f64 / size as f64) * 100.0).round() as u32).min(100);
                let color = if pct >= 90 {
                    ui_theme::red()
                } else if pct >= 75 {
                    ui_theme::yellow()
                } else {
                    ui_theme::text_muted()
                };
                div()
                    .px_2p5()
                    .py_0p5()
                    .rounded_full()
                    .bg(gpui::rgba(0x80808020))
                    .text_xs()
                    .text_color(gpui::rgb(color))
                    .child(format!("上下文 {pct}%"))
            });
            let model_pill = {
                let label = if pill_is_model {
                    pill_text.clone()
                } else {
                    format!("适配器 {pill_text}")
                };
                let color = if pill_is_model {
                    ui_theme::purple()
                } else {
                    ui_theme::text_muted()
                };
                if model_options.len() > 1 {
                    let cur = current_model.clone();
                    let opts = model_options.clone();
                    let config_id = model_config_id.clone();
                    let this = cx.entity();
                    Button::new("acp-model-pill")
                        .ghost()
                        .xsmall()
                        .label(format!("{label} ▾"))
                        .text_color(gpui::rgb(color))
                        .dropdown_menu(move |menu, _window, _cx| {
                            let mut menu = menu.item(PopupMenuItem::label("切换模型"));
                            for (value, name) in &opts {
                                let is_cur = cur.as_deref() == Some(name.as_str());
                                let value = value.clone();
                                let config_id = config_id.clone();
                                let this = this.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone()).checked(is_cur).on_click(
                                        move |_ev, _window, cx| {
                                            let value = value.clone();
                                            if let Some(config_id) = config_id.clone() {
                                                this.update(cx, |v, _cx| {
                                                    v.set_config_option(config_id, value)
                                                });
                                            }
                                        },
                                    ),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                } else {
                    div()
                        .px_2p5()
                        .py_0p5()
                        .rounded_full()
                        .bg(ui_theme::overlay(0x18))
                        .text_xs()
                        .text_color(gpui::rgb(color))
                        .child(label)
                        .into_any_element()
                }
            };
            let config_pills: Vec<gpui::AnyElement> = config_options
                .into_iter()
                .filter(|config| config.options.len() > 1)
                .map(|config| {
                    let config_label = config.name.clone();
                    let current_label = config.current_name.clone();
                    // 输入栏只放当前值；配置名称留在下拉菜单标题，避免把同一语义
                    // 重复写一遍，也给窄窗口留出空间。
                    let label = current_label;
                    let config_id = config.config_id.clone();
                    let current = config.current_name.clone();
                    let options = config.options.clone();
                    let menu_title = config_label;
                    let this = cx.entity();
                    Button::new(format!("acp-config-pill-{config_id}"))
                        .ghost()
                        .xsmall()
                        .label(format!("{label} ▾"))
                        .text_color(gpui::rgb(ui_theme::text_muted()))
                        .dropdown_menu(move |menu, _window, _cx| {
                            let mut menu = menu.item(PopupMenuItem::label(menu_title.clone()));
                            for (value, name) in &options {
                                let is_cur = current == *name;
                                let value = value.clone();
                                let config_id = config_id.clone();
                                let this = this.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone()).checked(is_cur).on_click(
                                        move |_ev, _window, cx| {
                                            let value = value.clone();
                                            let config_id = config_id.clone();
                                            this.update(cx, |v, _cx| {
                                                v.set_config_option(config_id, value)
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                })
                .collect();
            let composer = v_flex()
                .w_full()
                .max_w(px(1040.))
                .rounded_xl()
                .border_1()
                .border_color(t.border)
                .bg(ui_theme::glass_input())
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .pb_2()
                        .min_h(px(88.))
                        .child(Input::new(input)),
                )
                // 排队消息条：运行中点发送不会立刻打过去（ACP 一个 session 一次
                // 只能有一个在跑的 turn），得让人看见「排上了」，还能反悔撤回。
                .when(!self.queued_prompts.is_empty(), |col| {
                    let mut strip = v_flex().px_4().pt_3().gap_1p5();
                    for (ix, (text, images)) in self.queued_prompts.iter().enumerate() {
                        let preview: String = text.chars().take(60).collect();
                        let preview = if text.chars().count() > 60 {
                            format!("{preview}…")
                        } else {
                            preview
                        };
                        let img_suffix = if images.is_empty() {
                            String::new()
                        } else {
                            format!("（含 {} 张图）", images.len())
                        };
                        strip = strip.child(
                            h_flex()
                                .id(("acp-queued-prompt", ix))
                                .gap_2()
                                .items_center()
                                .px_2p5()
                                .py_1()
                                .rounded_md()
                                .bg(ui_theme::overlay(0x14))
                                .border_1()
                                .border_color(t.border)
                                .child(
                                    Icon::new(IconName::LoaderCircle)
                                        .size_3p5()
                                        .text_color(gpui::rgb(ui_theme::text_muted())),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .child(format!("排队中 · {preview}{img_suffix}")),
                                )
                                .child(
                                    div()
                                        .id(("acp-queued-prompt-remove", ix))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .cursor_pointer()
                                        .hover(|d| d.opacity(0.8))
                                        .child("撤回")
                                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                                            if ix < this.queued_prompts.len() {
                                                this.queued_prompts.remove(ix);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        );
                    }
                    col.child(strip)
                })
                // 待发图片的缩略图条：粘完得看得见「贴上了」，还得能反悔。
                .when(!self.pending_images.is_empty(), |col| {
                    let mut strip = h_flex().px_4().pt_3().gap_2().items_center().flex_wrap();
                    for (ix, im) in self.pending_images.iter().enumerate() {
                        let preview_image = im.clone();
                        strip = strip.child(
                            div()
                                .id(("acp-pending-img", ix))
                                .relative()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _ev, _window, cx| {
                                    let _ = this;
                                    cx.emit(AcpViewEvent::PreviewImage(preview_image.clone()));
                                }))
                                .child(
                                    gpui::img(im.clone())
                                        .h(px(56.))
                                        .max_w(px(96.))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(t.border),
                                )
                                .child(
                                    // 右上角小 ×：点掉这张。
                                    div()
                                        .absolute()
                                        .top(px(-4.))
                                        .right(px(-4.))
                                        .size(px(16.))
                                        .rounded_full()
                                        .bg(ui_theme::overlay(0xcc))
                                        .text_xs()
                                        .text_color(gpui::rgb(ui_theme::text_mid()))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|d| d.opacity(0.8))
                                        .child("×")
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _ev, _window, cx| {
                                                if ix < this.pending_images.len() {
                                                    this.pending_images.remove(ix);
                                                }
                                                cx.stop_propagation();
                                                cx.notify();
                                            }),
                                        ),
                                ),
                        );
                    }
                    col.child(strip)
                })
                .child(
                    h_flex()
                        .px_4()
                        .pt_2()
                        .pb_4()
                        .gap_2()
                        .items_end()
                        // 配置项数量由 agent 决定，不能和发送按钮争同一行宽度。
                        // 左侧独立换行，右侧命令按钮保持固定可点。
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap_2()
                                .items_center()
                                .flex_wrap()
                                .children(usage_pill)
                                .child(model_pill)
                                .children(config_pills),
                        )
                        .when(matches!(self.phase, AcpPhase::Running), |row| {
                            row.child(
                                div()
                                    .id("acp-stop")
                                    .flex_shrink_0()
                                    .px_2p5()
                                    .py_1()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(t.border)
                                    .text_xs()
                                    .text_color(muted)
                                    .cursor_pointer()
                                    .hover(|d| d.opacity(0.8))
                                    .child("停止")
                                    .on_click(
                                        cx.listener(|this, _ev, _window, _cx| this.cancel_turn()),
                                    ),
                            )
                        })
                        .children(self.restart_error.as_ref().map(|err| {
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(gpui::rgb(ui_theme::red()))
                                .child(format!("重启失败：{err}"))
                        }))
                        .child(
                            // 主发送按钮（橙实心，对齐设计稿 Send ⏎）。
                            div()
                                .id("acp-send")
                                .flex_shrink_0()
                                .px_4()
                                .py_1p5()
                                .rounded_lg()
                                .bg(gpui::rgb(ui_theme::accent()))
                                .text_color(gpui::rgb(ui_theme::on_accent()))
                                .text_sm()
                                .font_semibold()
                                .cursor_pointer()
                                .hover(|d| d.opacity(0.9))
                                .child("发送 ⏎")
                                .on_click(cx.listener(|this, _ev, window, cx| {
                                    this.submit_input(window, cx);
                                })),
                        ),
                );

            v_flex()
                // 外层必须先有确定的宽度：`composer` 自己既是 `w_full` 又有
                // `max_w`，若父节点按内容收缩，IME 组合文本触发重测量时会让
                // `w_full` 在不同帧解析成不同宽度，导致输入框从居中跳到左侧。
                .w_full()
                .px_4()
                .py_3()
                .items_center()
                .children(completion_bar)
                .child(composer)
        });

        let plan_bar = self.render_plan_bar(cx);
        let activity_status = matches!(self.phase, AcpPhase::Running).then(|| {
            let elapsed = self
                .turn_started_at_ms
                .map(|started| unix_time_ms().saturating_sub(started));
            h_flex().w_full().justify_center().px_4().child(
                h_flex()
                    .w_full()
                    .max_w(px(1040.))
                    .relative()
                    .overflow_hidden()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(t.border)
                    .bg(gpui::rgb(ui_theme::bg_bar()))
                    .children(elapsed.map(|elapsed| {
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("进行中 · 已用 {}", format_duration(elapsed)))
                    }))
                    .when(show_thinking, |row| {
                        row.child(div().w(px(1.)).h_3().bg(t.border)).child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(Spinner::new().xsmall().color(muted))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(format!("{} 正在思考", self.agent.short_label())),
                                )
                                .child(
                                    // Discord「对方正在输入」经典三连跳点：比死气沉沉的
                                    // 「…」更有「真的在动脑子」的感觉。三颗点错开相位,
                                    // 逐个跳起再落下，只用 `top` 位移不影响布局。
                                    h_flex()
                                        .items_center()
                                        .gap(px(3.))
                                        .children((0..3usize).map(|n| {
                                            let phase = n as f32 / 3.0;
                                            div()
                                                .relative()
                                                .size(px(4.))
                                                .rounded_full()
                                                .bg(muted)
                                                .with_animation(
                                                    ("acp-thinking-dot", n),
                                                    Animation::new(
                                                        std::time::Duration::from_millis(900),
                                                    )
                                                    .repeat(),
                                                    move |this, delta| {
                                                        let t = (delta + phase).fract();
                                                        let lift = (t * std::f32::consts::TAU)
                                                            .sin()
                                                            .max(0.0);
                                                        this.top(px(-lift * 4.))
                                                    },
                                                )
                                        })),
                                )
                                .with_animation(
                                    "acp-thinking-breathe",
                                    Animation::new(std::time::Duration::from_millis(1800)).repeat(),
                                    |this, delta| {
                                        let wave =
                                            (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                                        this.opacity(0.68 + wave * 0.28)
                                    },
                                ),
                        )
                    })
                    .child(
                        // 底部这条线之前是整条一起淡入淡出的静态呼吸；现在改成一段
                        // 更亮的「彗星」在暗轨道上来回扫，观感更像正在跑的进度条，
                        // 而不是一条若有若无的静止线。容器有 `overflow_hidden`，
                        // 彗星划出卡片边界的部分会被圆角裁掉，不会露怪。
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(1.5))
                            .bg(ui_theme::tint(ui_theme::accent(), 0x1c)),
                    )
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .h(px(1.5))
                            .w(px(160.))
                            .bg(gpui::rgb(ui_theme::accent()))
                            .with_animation(
                                "acp-activity-sweep",
                                Animation::new(std::time::Duration::from_millis(2200)).repeat(),
                                |this, delta| this.left(px(-160. + delta * (1040. + 320.))),
                            ),
                    ),
            )
        });

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            // ⌘⏎ 快捷批准：有待审批卡片时等价于点绿色主按钮。挂在根上冒泡接收，
            // 输入框聚焦时也能生效（Input 只消费不带修饰键的 Enter）。
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.modifiers.platform
                    && ev.keystroke.key == "enter"
                    && !this.permissions.is_empty()
                {
                    this.pick_permission_primary(cx);
                    cx.stop_propagation();
                }
            }))
            // 补全弹层的键盘操作。同样只能走 **action 的 capture 阶段**：
            // 上/下/回车/Esc/Tab 在输入框里全都绑成了 action，冒泡阶段和
            // capture_key_down 都轮不到我们（见下面 ⌘V 那段的教训）。
            // 没在补全时一律不拦，按键原样交回输入框。
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveUp, _window, cx| {
                    if this.move_completion(-1, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &gpui_component::input::MoveDown, _window, cx| {
                    if this.move_completion(1, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Enter, window, cx| {
                    // 补全开着时回车是「选中这条」，不是发送——否则永远选不上。
                    if this.accept_completion(window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |this, _: &gpui_component::input::IndentInline, window, cx| {
                    if this.accept_completion(window, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, _window, cx| {
                    if this.completion.take().is_some() {
                        cx.notify();
                        cx.stop_propagation();
                    }
                }),
            )
            // ⌘V 贴图（输入框聚焦时，也就是绝大多数情况）：必须拦 **Paste
            // action 的 capture 阶段**，不能拦 key_down。
            //
            // 真实教训：第一版挂的是 capture_key_down，实测完全没反应。GPUI 的
            // dispatch_key_event 顺序是「先派发 action bindings，binding 消费掉
            // 就直接 return」，capture 阶段的 key listener 排在那之后——输入框
            // 把 cmd-v 绑成了 Paste（gpui-component input/state.rs），于是这个
            // 事件永远轮不到我们。而 action 的 capture 阶段是从根往下走的，
            // 挂在这里就能抢在输入框（更深的节点）前面拿到。
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, _window, cx| {
                    // 只有剪贴板真是图片才截胡；文本粘贴照样放行给输入框。
                    if this.take_clipboard_image(cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            // 焦点不在输入框里（点了消息流等）时 Paste binding 不匹配，
            // action 那条路走不到——这条按 key_down 兜底。
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if ev.keystroke.modifiers.platform
                    && ev.keystroke.key == "v"
                    && this.take_clipboard_image(cx)
                {
                    cx.stop_propagation();
                }
            }))
            .bg(acp_surface)
            .children(banner)
            .children(fork_banner)
            .children(plan_bar)
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(list)
                    .children((!self.entries.is_empty()).then(|| {
                        Scrollbar::vertical(&self.list_state)
                            .id("acp-message-scrollbar")
                            .scrollbar_show(ScrollbarShow::Always)
                    }))
                    .children(sticky_prompt)
                    .children(jump_to_latest),
            )
            .children(activity_status)
            .children(permission)
            .children(elicitation)
            .children(self.paste_hint.as_ref().map(|msg| {
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1p5()
                    .border_t_1()
                    .border_color(t.border)
                    .bg(ui_theme::tint(ui_theme::yellow(), 0x14))
                    .text_xs()
                    .text_color(gpui::rgb(ui_theme::yellow()))
                    .child(msg.clone())
            }))
            .children(input_row)
    }
}

/// GPUI 剪贴板图片格式 → 协议要的 MIME。
fn image_mime(format: gpui::ImageFormat) -> &'static str {
    match format {
        gpui::ImageFormat::Png => "image/png",
        gpui::ImageFormat::Jpeg => "image/jpeg",
        gpui::ImageFormat::Webp => "image/webp",
        gpui::ImageFormat::Gif => "image/gif",
        gpui::ImageFormat::Svg => "image/svg+xml",
        gpui::ImageFormat::Bmp => "image/bmp",
        gpui::ImageFormat::Tiff => "image/tiff",
        // 协议字段是必填的字符串，认不出的格式给个通用值让 agent 自己嗅探，
        // 总好过不发（ImageFormat 是 #[non_exhaustive]，会长新枝）。
        _ => "application/octet-stream",
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_acp_image(image: &AcpImage) -> Option<std::sync::Arc<gpui::Image>> {
    use base64::Engine as _;

    let format = match image.mime.as_str() {
        "image/png" => gpui::ImageFormat::Png,
        "image/jpeg" => gpui::ImageFormat::Jpeg,
        "image/webp" => gpui::ImageFormat::Webp,
        "image/gif" => gpui::ImageFormat::Gif,
        "image/svg+xml" => gpui::ImageFormat::Svg,
        "image/bmp" => gpui::ImageFormat::Bmp,
        "image/tiff" => gpui::ImageFormat::Tiff,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data_b64)
        .ok()?;
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

fn decode_entry_images(
    entries: &[AcpEntry],
    offset: usize,
) -> std::collections::HashMap<(usize, usize), std::sync::Arc<gpui::Image>> {
    entries
        .iter()
        .enumerate()
        .flat_map(|(entry_ix, entry)| match entry {
            AcpEntry::UserWithImages { images, .. } => images
                .iter()
                .enumerate()
                .filter_map(move |(image_ix, image)| {
                    decode_acp_image(image).map(|decoded| ((offset + entry_ix, image_ix), decoded))
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

fn build_diff_cache(
    entries: &[AcpEntry],
) -> std::collections::HashMap<String, Vec<Option<CachedDiff>>> {
    let mut cache = std::collections::HashMap::new();
    for entry in entries {
        let AcpEntry::ToolCall { id, output, .. } = entry else {
            continue;
        };
        let parts = output
            .iter()
            .map(|part| match part {
                ToolOutputPart::Diff {
                    old_text, new_text, ..
                } => {
                    let old = old_text.as_deref().unwrap_or("");
                    let full = diff_lines(old, new_text);
                    let added = full
                        .iter()
                        .filter(|line| line.tag == DiffLineTag::Added)
                        .count();
                    let removed = full
                        .iter()
                        .filter(|line| line.tag == DiffLineTag::Removed)
                        .count();
                    Some(CachedDiff {
                        old_fingerprint: text_fingerprint(old),
                        new_fingerprint: text_fingerprint(new_text),
                        lines: std::rc::Rc::new(compact_diff_lines(&full, 3)),
                        added,
                        removed,
                    })
                }
                ToolOutputPart::Text(_) => None,
            })
            .collect();
        cache.insert(id.clone(), parts);
    }
    cache
}

fn refresh_diff_cache(
    entries: &[AcpEntry],
    entries_offset: usize,
    cache: &mut std::collections::HashMap<String, Vec<Option<CachedDiff>>>,
) {
    let live_ids: std::collections::HashSet<&str> = entries
        .iter()
        .filter_map(|entry| match entry {
            AcpEntry::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    cache.retain(|id, _| live_ids.contains(id.as_str()));

    for entry in entries.iter().skip(entries_offset) {
        let AcpEntry::ToolCall { id, output, .. } = entry else {
            continue;
        };
        let old_parts = cache.remove(id).unwrap_or_default();
        let parts = output
            .iter()
            .enumerate()
            .map(|(part_ix, part)| match part {
                ToolOutputPart::Diff {
                    old_text, new_text, ..
                } => {
                    let old = old_text.as_deref().unwrap_or("");
                    let old_fingerprint = text_fingerprint(old);
                    let new_fingerprint = text_fingerprint(new_text);
                    if let Some(Some(cached)) = old_parts.get(part_ix)
                        && cached.old_fingerprint == old_fingerprint
                        && cached.new_fingerprint == new_fingerprint
                    {
                        return Some(cached.clone());
                    }
                    let full = diff_lines(old, new_text);
                    let added = full
                        .iter()
                        .filter(|line| line.tag == DiffLineTag::Added)
                        .count();
                    let removed = full
                        .iter()
                        .filter(|line| line.tag == DiffLineTag::Removed)
                        .count();
                    Some(CachedDiff {
                        old_fingerprint,
                        new_fingerprint,
                        lines: std::rc::Rc::new(compact_diff_lines(&full, 3)),
                        added,
                        removed,
                    })
                }
                ToolOutputPart::Text(_) => None,
            })
            .collect();
        cache.insert(id.clone(), parts);
    }
}

fn text_fingerprint(text: &str) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    (text.len(), hasher.finish())
}

fn is_user_entry(entry: &AcpEntry) -> bool {
    matches!(entry, AcpEntry::User(_) | AcpEntry::UserWithImages { .. })
}

/// 模型的思考摘要标题经常整行套 `**像这样**`/`__这样__`；折叠预览是纯文本
/// `div`，不走 markdown 渲染，裸露的星号看着像漏渲染的格式错误。只在整行
/// 前后都包着同一种标记时才剥掉，避免误伤正文里本来就有的单个星号。
fn strip_thought_heading_markers(line: &str) -> &str {
    for wrapper in ["**", "__"] {
        if let Some(inner) = line
            .strip_prefix(wrapper)
            .and_then(|s| s.strip_suffix(wrapper))
            && !inner.is_empty()
        {
            return inner;
        }
    }
    line
}

const HANDOFF_MAX_CHARS: usize = 24_000;
const HANDOFF_MESSAGE_MAX_CHARS: usize = 4_000;

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let value: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{value}\n[内容已截断]")
    } else {
        value
    }
}

/// ACP v1 没有 thread/fork。这里生成一份受控的交接提示：保留用户与正式回答，
/// 工具只交接可核对的摘要，排除思考、图片体和原始长输出。
fn build_handoff_prompt(
    entries: &[AcpEntry],
    through: usize,
    source_title: &str,
    cwd: Option<&str>,
) -> String {
    let mut segments = Vec::new();
    for entry in entries.iter().take(through.saturating_add(1)) {
        let segment = match entry {
            AcpEntry::User(text) => Some(format!(
                "用户：{}",
                truncate_chars(text.trim(), HANDOFF_MESSAGE_MAX_CHARS)
            )),
            AcpEntry::UserWithImages { text, images } => {
                let text = truncate_chars(text.trim(), HANDOFF_MESSAGE_MAX_CHARS);
                Some(if text.is_empty() {
                    format!("用户：[附带 {} 张图片，图片未复制]", images.len())
                } else {
                    format!("用户：{text}\n[附带 {} 张图片，图片未复制]", images.len())
                })
            }
            AcpEntry::Assistant {
                text,
                thought: false,
            } => Some(format!(
                "助手：{}",
                truncate_chars(text.trim(), HANDOFF_MESSAGE_MAX_CHARS)
            )),
            AcpEntry::Assistant { thought: true, .. } | AcpEntry::Divider(_) => None,
            AcpEntry::ToolCall {
                title,
                kind,
                status,
                output,
                ..
            } => {
                let diffs: Vec<String> = output
                    .iter()
                    .filter_map(|part| match part {
                        ToolOutputPart::Diff { path, .. } => Some(path.clone()),
                        ToolOutputPart::Text(_) => None,
                    })
                    .collect();
                let suffix = if diffs.is_empty() {
                    String::new()
                } else {
                    format!("；{}", diffs.join("，"))
                };
                Some(format!("工具：{title}（{kind:?}，{status:?}）{suffix}"))
            }
        };
        if let Some(segment) = segment.filter(|s| !s.trim().is_empty()) {
            segments.push(segment);
        }
    }

    let header = format!(
        "这是从 Smelt 原会话「{source_title}」创建的新 ACP 会话。\n工作目录：{}\n以下是截至所选回答的精简交接记录；它不是原会话的无损副本。",
        cwd.unwrap_or("未提供")
    );
    let footer =
        "请先核对当前工作区文件和 Git 状态，再从上述进度继续。不要假设未列出的工具输出仍然有效。";
    let fixed = header.chars().count() + footer.chars().count() + 8;
    let budget = HANDOFF_MAX_CHARS.saturating_sub(fixed);
    let mut selected = Vec::new();
    let mut used = 0usize;
    for segment in segments.into_iter().rev() {
        let len = segment.chars().count() + 2;
        if used + len > budget {
            continue;
        }
        used += len;
        selected.push(segment);
    }
    selected.reverse();
    format!("{header}\n\n{}\n\n{footer}", selected.join("\n\n"))
}

/// 工具输出默认只展开这么多行，其余折叠到「展开全部 N 行」后面。
const TOOL_OUTPUT_PREVIEW_LINES: usize = 8;

/// 工具卡片首次出现一律折叠，避免运行过程中的大量输出撑高消息流。
/// 用户手动点过后由 `expanded_tool_cards` / `collapsed_tool_cards` 覆盖这个默认值。
fn tool_card_default_expanded(_status: ToolCallStatus, _has_pending_permission: bool) -> bool {
    false
}

/// 一轮里最后一段非思考正文才是最终回答。工具调用和思考可以夹在正文之间，
/// 下一条用户消息或会话分隔符才开始新一轮。
fn is_turn_final_answer(entries: &[AcpEntry], index: usize) -> bool {
    if !matches!(
        entries.get(index),
        Some(AcpEntry::Assistant { thought: false, .. })
    ) {
        return false;
    }
    !entries[index + 1..]
        .iter()
        .take_while(|entry| !is_user_entry(entry) && !matches!(entry, AcpEntry::Divider(_)))
        .any(|entry| matches!(entry, AcpEntry::Assistant { thought: false, .. }))
}

#[derive(Clone, Copy)]
struct ProcessGroupInfo {
    first: usize,
    steps: usize,
    tools: usize,
    failed: usize,
}

/// 返回某条 entry 所属的“执行过程”组。每轮最后一段正式回答之外的 assistant
/// 内容与工具调用都属于过程；用户消息、分隔符和最终回答本身不属于。
fn process_group_for_entry(entries: &[AcpEntry], index: usize) -> Option<ProcessGroupInfo> {
    if index >= entries.len()
        || is_user_entry(&entries[index])
        || matches!(entries[index], AcpEntry::Divider(_))
        || is_turn_final_answer(entries, index)
    {
        return None;
    }
    let turn_start = entries[..index]
        .iter()
        .rposition(|entry| is_user_entry(entry) || matches!(entry, AcpEntry::Divider(_)))
        .map_or(0, |ix| ix + 1);
    let turn_end = entries[index..]
        .iter()
        .position(|entry| is_user_entry(entry) || matches!(entry, AcpEntry::Divider(_)))
        .map_or(entries.len(), |offset| index + offset);
    let final_ix = (turn_start..turn_end)
        .rev()
        .find(|ix| is_turn_final_answer(entries, *ix));
    let process_end = final_ix.unwrap_or(turn_end);
    if index >= process_end {
        return None;
    }
    let process_indices: Vec<usize> = (turn_start..process_end)
        .filter(|ix| {
            !is_user_entry(&entries[*ix])
                && !matches!(entries[*ix], AcpEntry::Divider(_))
                && !is_turn_final_answer(entries, *ix)
        })
        .collect();
    let first = *process_indices.first()?;
    if !process_indices.contains(&index) {
        return None;
    }
    let tools = process_indices
        .iter()
        .filter(|ix| matches!(entries[**ix], AcpEntry::ToolCall { .. }))
        .count();
    let failed = process_indices
        .iter()
        .filter(|ix| {
            matches!(
                entries[**ix],
                AcpEntry::ToolCall {
                    status: ToolCallStatus::Failed,
                    ..
                }
            )
        })
        .count();
    Some(ProcessGroupInfo {
        first,
        steps: process_indices.len(),
        tools,
        failed,
    })
}

fn format_duration(milliseconds: u64) -> String {
    let total_seconds = milliseconds / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// gpui-component 会把 Markdown 链接目标原样交给 `open_url`。相对文件路径在
/// macOS 上会被 LaunchServices 误当作应用标识并报 -50，因此在进入 Markdown
/// 渲染前把它们解析成基于会话 cwd 的 file URL。
fn markdown_text_for_cwd(text: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("](") {
        let target_start = open + 2;
        let Some(close_offset) = rest[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + close_offset;
        let target = &rest[target_start..target_end];
        out.push_str(&rest[..target_start]);
        if let Some(resolved) = resolve_relative_file_link(target, cwd) {
            out.push_str(&resolved);
        } else {
            out.push_str(target);
        }
        out.push(')');
        rest = &rest[target_end + 1..];
    }
    out.push_str(rest);
    out
}

fn resolve_relative_file_link(target: &str, cwd: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with('~')
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return None;
    }
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    // Agent 引用文件常写成 grep/编译器诊断那种 `path:行号` 或 `path:行号:列号`
    // 格式（不是 `#L行号` 片段）。这种没有 `#` 片段时，才尝试从冒号后缀里抠
    // 行号出来——不然会把 `:2765` 当成文件名的一部分拼进路径，读不到文件时
    // 还误报“可能是二进制文件”。
    let (path, fragment) = if fragment.is_empty() {
        match extract_trailing_line_number(path) {
            Some((base, line)) => (base, format!("L{line}")),
            None => (path, String::new()),
        }
    } else {
        (path, fragment.to_string())
    };
    let path = std::path::Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(cwd).join(path)
    };
    let mut url = url::Url::from_file_path(absolute).ok()?;
    if !fragment.is_empty() {
        url.set_fragment(Some(&fragment));
    }
    // `Url::set_scheme` 会把自定义 scheme 序列化成 `smelt-file:/path`
    // （单斜杠），而 macOS URL scheme 与主进程解析器都按 authority 形式接收。
    // 从标准 file URL 替换前缀可稳定保留 `smelt-file:///absolute/path`。
    Some(url.to_string().replacen("file://", "smelt-file://", 1))
}

/// 从 `path:行号` / `path:行号:列号` 里拆出末尾的行号：从右往左数，只要是纯
/// 数字的 segment 就一直往前吞（列号可选，最多吞两段），吞到的最后一个数字
/// segment 就是行号；一段数字都没吃到就说明不是这种格式，返回 None 原样处理
/// （比如 Windows 盘符 `C:\...`，虽然本 app 只跑 macOS，多判一下无妨）。
fn extract_trailing_line_number(path: &str) -> Option<(&str, u32)> {
    let mut rest = path;
    let mut line = None;
    for _ in 0..2 {
        let Some((head, tail)) = rest.rsplit_once(':') else {
            break;
        };
        if head.is_empty() {
            break;
        }
        let Ok(n) = tail.parse::<u32>() else {
            break;
        };
        line = Some(n);
        rest = head;
    }
    line.map(|n| (rest, n))
}

/// 审批请求按收到顺序串行展示和处理，不能越过队首回应后续 responder。
fn is_active_permission_selection(
    permissions: &[PendingPermission],
    tool_call_id: &str,
    option_id: &str,
) -> bool {
    permissions.first().is_some_and(|card| {
        card.tool_call_id == tool_call_id
            && card
                .options
                .iter()
                .any(|option| option.option_id == option_id)
    })
}

/// 工具标题里去掉与 kind 标签重复的前缀：adapter 常把标题写成
/// `Read crates/foo.rs`，而卡片左边已经有一个 `Read` 标签了。
fn strip_kind_prefix<'a>(title: &'a str, kind: &ToolKind) -> &'a str {
    let label = tool_kind_label(kind);
    title
        .strip_prefix(label)
        .map(|r| r.trim_start())
        .filter(|r| !r.is_empty())
        .unwrap_or(title)
}

/// ToolKind → 强调色：读类蓝、改类橙、执行类绿，一眼区分工具在干什么类型的事。
/// 原始 u32 色值——`tint()` 要的是这个，不是转换过的 `Rgba`，所以跟
/// `tool_accent_color` 分开放：后者给文字/图标上色，前者给徽章调透明度。
fn tool_accent_u32(kind: &ToolKind) -> u32 {
    match kind {
        ToolKind::Read | ToolKind::Search | ToolKind::Fetch => ui_theme::blue(),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => ui_theme::accent(),
        ToolKind::Execute => ui_theme::green(),
        ToolKind::Collaborate => ui_theme::blue(),
        ToolKind::Review => ui_theme::yellow(),
        ToolKind::Image => ui_theme::accent(),
        ToolKind::Compact | ToolKind::Wait => ui_theme::text_muted(),
        // `Other`（如 task_complete 这类协议里没有专门 kind 的调用）以及 SwitchMode：
        // 之前跟着 muted 灰走，跟卡片本身的灰色边框撞色，左边的强调条看起来像
        // "边框没删干净"。换成 purple 一眼能看出这也是一根有意画的强调条。
        _ => ui_theme::purple(),
    }
}

fn tool_accent_color(kind: &ToolKind) -> gpui::Rgba {
    gpui::rgb(tool_accent_u32(kind))
}

/// ToolKind → 简短英文标签（跟工具本身在协议里的调用名对齐，比长句子扫得快）。
fn tool_kind_label(kind: &ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Bash",
        ToolKind::Fetch => "Fetch",
        ToolKind::Think => "Think",
        ToolKind::SwitchMode => "Mode",
        ToolKind::Collaborate => "Agent",
        ToolKind::Review => "Review",
        ToolKind::Image => "Image",
        ToolKind::Compact => "Compact",
        ToolKind::Wait => "Wait",
        _ => "Tool",
    }
}

fn tool_kind_icon(kind: &ToolKind) -> IconName {
    match kind {
        ToolKind::Execute => IconName::SquareTerminal,
        ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move | ToolKind::Image => {
            IconName::File
        }
        ToolKind::Search | ToolKind::Fetch => IconName::Search,
        ToolKind::Collaborate => IconName::Bot,
        ToolKind::Review | ToolKind::Compact | ToolKind::Think | ToolKind::SwitchMode => {
            IconName::Asterisk
        }
        ToolKind::Wait | ToolKind::Other => IconName::Asterisk,
    }
}

/// 渲染一份 diff：逐行红（删）/绿（增）/灰（不变），等宽字体，滚动限高——大改动
/// 不能把整个消息流撑爆，超出部分滚动查看。`key` 保证同一条消息里多个 diff
/// 块各自有唯一 element id。行数据来自 `smelt_core::acp_chat::diff_lines`——
/// 跟头部「+N -M」摘要（`diff_line_stats`）共用同一次计算结果，数字不会对不上。
fn render_diff_lines(
    lines: &[DiffLine],
    key: (usize, usize),
    border_color: gpui::Hsla,
    muted_color: gpui::Hsla,
) -> gpui::AnyElement {
    let mut rows = v_flex()
        .id(("acp-diff", key.0 * 10_000 + key.1))
        .max_h(px(320.))
        .overflow_y_scroll()
        .rounded_md()
        .border_1()
        .border_color(border_color)
        .font_family("monospace")
        .text_xs();
    for line in lines {
        let (bg, prefix, fg): (Option<gpui::Hsla>, &str, gpui::Hsla) = match line.tag {
            DiffLineTag::Removed => (
                Some(smelt_ui::ui_theme::tint(smelt_ui::ui_theme::red(), 0x22).into()),
                "-",
                gpui::rgb(smelt_ui::ui_theme::red()).into(),
            ),
            DiffLineTag::Added => (
                Some(smelt_ui::ui_theme::tint(smelt_ui::ui_theme::green(), 0x22).into()),
                "+",
                gpui::rgb(smelt_ui::ui_theme::diff_add_text()).into(),
            ),
            DiffLineTag::Context => (None, " ", muted_color),
        };
        let mut row = h_flex().px_2().gap_2();
        if let Some(bg) = bg {
            row = row.bg(bg);
        }
        rows = rows.child(
            row.child(
                div()
                    .w(px(12.))
                    .flex_shrink_0()
                    .text_color(fg)
                    .child(prefix.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(fg)
                    .child(line.text.clone()),
            ),
        );
    }
    rows.into_any_element()
}

// strip_code_fence / is_interrupt_marker 的单测随实现一起搬进了
// smelt_core::acp_chat（见该模块的 #[cfg(test)]），这里不再重复。

#[cfg(test)]
mod tests {
    use super::{
        HANDOFF_MAX_CHARS, build_handoff_prompt, is_active_permission_selection,
        is_turn_final_answer, markdown_text_for_cwd, process_group_for_entry,
        resolve_restart_launch, tool_card_default_expanded,
    };
    use smelt_core::acp_chat::{AcpEntry, ToolCallStatus, ToolKind, ToolOutputPart};
    use smelt_core::acp_session::{
        ApprovalDetailsView, PendingPermission, PermissionOptionKindView, PermissionOptionView,
    };
    use smelt_core::agent_kind::{AcpAgentKind, AcpLaunchSpec, AcpProfile};
    use smelt_ui::agent_ui_config::AgentUiConfig;

    #[test]
    fn markdown_local_files_become_internal_file_urls() {
        let rendered = markdown_text_for_cwd(
            "看 [workspace.md](docs/workspace.md)、[源码](/tmp/source.rs#L42) 和 [官网](https://example.com)",
            Some("/tmp/project"),
        );
        assert!(rendered.contains("[workspace.md](smelt-file:///tmp/project/docs/workspace.md)"));
        assert!(rendered.contains("[源码](smelt-file:///tmp/source.rs#L42)"));
        assert!(rendered.contains("[官网](https://example.com)"));
    }

    /// grep / 编译器诊断常见的 `path:行号` 引用格式（不是 `#L行号` 片段）也要能
    /// 拆出行号——原来只认 `#`，`:2765` 会整段被当成文件名拼进路径，导致读不到
    /// 文件、误报“可能是二进制文件”（见用户反馈：点 ACP 对话里的文件引用链接）。
    #[test]
    fn markdown_colon_line_refs_resolve_to_fragment() {
        let rendered = markdown_text_for_cwd(
            "见 [acp_view.rs](crates/smelt-acp-view/src/acp_view.rs:2765)",
            Some("/tmp/project"),
        );
        assert!(rendered.contains(
            "[acp_view.rs](smelt-file:///tmp/project/crates/smelt-acp-view/src/acp_view.rs#L2765)"
        ));
    }

    /// `path:行号:列号` 形式（列号可选的第二段）也只取行号，列号丢弃。
    #[test]
    fn markdown_colon_line_col_refs_take_line_not_col() {
        let rendered = markdown_text_for_cwd("见 [x](src/main.rs:10:5)", Some("/tmp/project"));
        assert!(rendered.contains("[x](smelt-file:///tmp/project/src/main.rs#L10)"));
    }

    #[test]
    fn final_answer_is_the_last_body_in_each_user_turn() {
        let entries = vec![
            AcpEntry::User("修一下".into()),
            AcpEntry::Assistant {
                text: "先检查".into(),
                thought: false,
            },
            AcpEntry::ToolCall {
                id: "read-1".into(),
                title: "Read file".into(),
                kind: ToolKind::Read,
                status: ToolCallStatus::Completed,
                output: Vec::new(),
            },
            AcpEntry::Assistant {
                text: "已修复".into(),
                thought: false,
            },
            AcpEntry::User("再看看".into()),
            AcpEntry::Assistant {
                text: "没问题".into(),
                thought: false,
            },
        ];
        assert!(!is_turn_final_answer(&entries, 1));
        assert!(is_turn_final_answer(&entries, 3));
        assert!(is_turn_final_answer(&entries, 5));

        let group = process_group_for_entry(&entries, 1).expect("过程正文应进入执行过程组");
        assert_eq!(group.first, 1);
        assert_eq!(group.steps, 2);
        assert_eq!(group.tools, 1);
        assert!(process_group_for_entry(&entries, 2).is_some());
        assert!(process_group_for_entry(&entries, 3).is_none());
    }

    #[test]
    fn handoff_excludes_thought_images_and_raw_tool_output() {
        let entries = vec![
            AcpEntry::UserWithImages {
                text: "修复滚动".into(),
                images: vec![smelt_core::acp_chat::AcpImage {
                    mime: "image/png".into(),
                    data_b64: "BASE64_SECRET".into(),
                }],
            },
            AcpEntry::Assistant {
                text: "PRIVATE_THOUGHT".into(),
                thought: true,
            },
            AcpEntry::ToolCall {
                id: "edit-1".into(),
                title: "Edit acp_view.rs".into(),
                kind: ToolKind::Edit,
                status: ToolCallStatus::Completed,
                output: vec![
                    ToolOutputPart::Text("RAW_TOOL_OUTPUT".into()),
                    ToolOutputPart::Diff {
                        path: "src/acp_view.rs".into(),
                        old_text: Some("old\n".into()),
                        new_text: "new\nextra\n".into(),
                    },
                ],
            },
            AcpEntry::Assistant {
                text: "已修复".into(),
                thought: false,
            },
        ];
        let prompt = build_handoff_prompt(&entries, 3, "滚动问题", Some("/tmp/project"));
        assert!(prompt.contains("修复滚动"));
        assert!(prompt.contains("图片未复制"));
        assert!(prompt.contains("src/acp_view.rs"));
        assert!(prompt.contains("已修复"));
        assert!(!prompt.contains("BASE64_SECRET"));
        assert!(!prompt.contains("PRIVATE_THOUGHT"));
        assert!(!prompt.contains("RAW_TOOL_OUTPUT"));
    }

    #[test]
    fn handoff_is_bounded_and_stops_at_selected_answer() {
        let entries = vec![
            AcpEntry::User("x".repeat(HANDOFF_MAX_CHARS * 2)),
            AcpEntry::Assistant {
                text: "选中的回答".into(),
                thought: false,
            },
            AcpEntry::User("不应出现".into()),
        ];
        let prompt = build_handoff_prompt(&entries, 1, "长对话", None);
        assert!(prompt.chars().count() <= HANDOFF_MAX_CHARS);
        assert!(prompt.contains("选中的回答"));
        assert!(!prompt.contains("不应出现"));
    }

    #[test]
    fn restart_uses_updated_profile_launch_spec_when_profile_still_exists() {
        let current = AcpLaunchSpec::from_command("claude --old")
            .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/old");
        let config = AgentUiConfig {
            acp_cmd: "claude --current".into(),
            profiles: vec![AcpProfile {
                id: "quant".into(),
                kind_id: "claude".into(),
                label: "Quant".into(),
                workspace_dir: "~/Claude Workspaces/new quant".into(),
            }],
            ..AgentUiConfig::default()
        };

        let resolved = resolve_restart_launch(
            &current,
            Some("quant"),
            &config,
            AcpAgentKind::Claude,
            false,
        );

        assert_eq!(
            resolved.command, "claude --current",
            "profile 会话重启时应沿用该 agent 当前配置的命令"
        );
        assert_eq!(
            resolved.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/new quant"),
            "profile 会话重启时应重新读取 profile 的当前 workspace 配置"
        );
    }

    #[test]
    fn restart_keeps_persisted_launch_when_profile_was_deleted() {
        let current = AcpLaunchSpec::from_command("claude --persisted")
            .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant");

        let resolved = resolve_restart_launch(
            &current,
            Some("quant"),
            &AgentUiConfig::default(),
            AcpAgentKind::Claude,
            false,
        );

        assert_eq!(resolved, current);
    }

    #[test]
    fn restart_refreshes_ordinary_session_from_current_agent_command() {
        let current = AcpLaunchSpec::from_command("claude --stale");
        let config = AgentUiConfig {
            acp_cmd: "claude --current".into(),
            ..AgentUiConfig::default()
        };

        let resolved = resolve_restart_launch(&current, None, &config, AcpAgentKind::Claude, true);

        assert_eq!(resolved, AcpLaunchSpec::from_command("claude --current"));
    }

    #[test]
    fn restart_keeps_legacy_launch_when_refresh_is_disabled() {
        let current =
            AcpLaunchSpec::from_command("CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude");
        let config = AgentUiConfig {
            acp_cmd: "claude --current".into(),
            ..AgentUiConfig::default()
        };

        let resolved = resolve_restart_launch(&current, None, &config, AcpAgentKind::Claude, false);

        assert_eq!(resolved, current);
    }

    #[test]
    fn tool_cards_are_collapsed_by_default_for_every_state() {
        assert!(!tool_card_default_expanded(
            ToolCallStatus::Completed,
            false
        ));
        assert!(!tool_card_default_expanded(ToolCallStatus::Completed, true));
        assert!(!tool_card_default_expanded(ToolCallStatus::Pending, false));
        assert!(!tool_card_default_expanded(
            ToolCallStatus::InProgress,
            false
        ));
        assert!(!tool_card_default_expanded(ToolCallStatus::Failed, false));
    }

    #[test]
    fn permission_selection_only_accepts_the_queue_head() {
        let permission = |tool_call_id: &str, option_id: &str| PendingPermission {
            question: tool_call_id.into(),
            tool_call_id: tool_call_id.into(),
            options: vec![PermissionOptionView {
                option_id: option_id.into(),
                name: "Allow once".into(),
                kind: PermissionOptionKindView::AllowOnce,
            }],
            details: ApprovalDetailsView::Generic,
        };
        let permissions = vec![
            permission("tool-1", "allow-1"),
            permission("tool-2", "allow-2"),
        ];

        assert!(is_active_permission_selection(
            &permissions,
            "tool-1",
            "allow-1"
        ));
        assert!(!is_active_permission_selection(
            &permissions,
            "tool-2",
            "allow-2"
        ));
        assert!(!is_active_permission_selection(
            &permissions,
            "tool-1",
            "unknown"
        ));
    }
}
