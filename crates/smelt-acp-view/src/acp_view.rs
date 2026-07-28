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
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode,
    InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, list as virtual_list, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex,
};

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

/// Codex ACP 的 mode 值定义了审批与沙箱预设。只翻译其稳定的公开值；其他
/// agent 的自定义模式保持 agent 自己给出的名称，避免 Smelt 误导权限语义。
fn config_value_label(config_id: &str, value: &str, fallback: &str) -> String {
    if config_id != "mode" {
        return fallback.to_string();
    }
    match value {
        "read-only" => "只读".to_string(),
        "agent" => "需审批（工作区）".to_string(),
        "agent-full-access" => "全自动（免审批，完整访问）".to_string(),
        _ => fallback.to_string(),
    }
}

/// 消息流数据模型（AcpEntry/ToolOutputPart/ToolKind/ToolCallStatus）与 diff/
/// markdown 围栏这批纯逻辑现在都活在 `smelt_core::acp_chat`——不依赖 GPUI 也不
/// 依赖 agent_client_protocol，未来 web/mobile 端渲染同一份对话时不用重新实现
/// 一遍「怎么把协议事件变成可展示内容」。这里整段 re-export，文件里大量既有的
/// 裸 `AcpEntry::...` 用法不用逐处改路径。
pub use smelt_core::acp_chat::{
    AcpEntry, DiffLineTag, ToolCallStatus, ToolKind, ToolOutputPart, diff_line_stats, diff_lines,
    is_interrupt_marker, strip_code_fence,
};

/// `@` / `/` 补全弹层的状态。回合态，不落盘。
struct CompletionPopup {
    /// 触发 token 在输入框文本里的字节范围（含 `@`/`/`），接受候选时按它替换。
    start: usize,
    end: usize,
    items: Vec<smelt_ui::acp_completion::Candidate>,
    selected: usize,
}

/// AcpView 对外发的唯一事件：内容有实质变化，该存盘了。main.rs 订阅它触发
/// `Workspace::save_state`（与侧栏 resize 订阅同一惯用法）。
pub enum AcpViewEvent {
    Changed,
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
    /// 本会话的 agent 是否收图（握手 Ready 带来）。握手前默认 true——那时还没
    /// 粘图的机会，先假设支持，Ready 到了再按实际能力修正（Grok = false）。
    supports_image: bool,
    /// 「这个 agent 不收图」的一次性提示：粘图被拦时置上，输入框上方显示一行，
    /// 用户下次一打字（Change）就清掉，不占定时器。
    paste_hint: Option<String>,
    /// `@` / `/` 补全弹层的当前状态；None = 没在补全。
    completion: Option<CompletionPopup>,
    /// cwd 下的文件清单缓存（`@` 的候选源）。每敲一个字符跑一次 git ls-files
    /// 会明显卡手，所以一次会话只列一次。
    file_cache: Option<std::rc::Rc<Vec<String>>>,
    /// agent 侧真实的 session id：握手成功后写入（从 smeltd 的快照里镜像过来），
    /// `restart()` 拿它去尝试真续接；也存盘（main.rs AcpSaved），GUI 重开后
    /// 同样能续。「等自己刚发那条 prompt 的回声」这类归约细节已经完全下沉到
    /// smeltd（见 smelt_core::acp_session），这里不用再操心。
    acp_session_id: Option<SessionId>,
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
    /// 模型状态：当前名 + 可切换的候选（协议给什么显示什么）；None = agent
    /// 没上报过，UI 就不显示模型胶囊，不拿适配器包名冒充。
    model: Option<ModelState>,
    /// 除模型以外的 ACP 会话配置。agent 未上报则不显示。
    config_options: Vec<SessionConfigState>,
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
    /// 冷恢复占位待自动启动：GUI 重启后第一次切到这个会话时自动 restart，
    /// 有旧 session id 则协议级续接，没有则新建一轮但保留本地历史。只消费一次——
    /// 自动启动失败（Fatal → Ended）后回到手动，错误得让人看见，不能循环重试。
    auto_resume_pending: bool,
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
        });
        this.attach_handle(handle, cx);
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
            supports_image: true,
            paste_hint: None,
            completion: None,
            file_cache: None,
            acp_session_id: resume_session_id,
            available_commands: Vec::new(),
            usage: None,
            starting_since: None,
            plan: None,
            plan_collapsed: false,
            model: None,
            config_options: Vec::new(),
            expanded_tools: std::collections::HashSet::new(),
            expanded_tool_cards: std::collections::HashSet::new(),
            collapsed_tool_cards: std::collections::HashSet::new(),
            list_state: {
                let state = ListState::new(initial_entry_count, ListAlignment::Top, px(800.));
                state.set_follow_mode(FollowMode::Tail);
                state
            },
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
            resume_id: self.acp_session_id.as_ref().map(|s| s.to_string()),
        });
        self.attach_handle(handle, cx);
        cx.notify();
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
        let prompt = self.entries.iter().find_map(|entry| match entry {
            AcpEntry::User(text) if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })?;
        let single_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut chars = single_line.chars();
        let title: String = chars.by_ref().take(36).collect();
        Some(if chars.next().is_some() {
            format!("{title}...")
        } else {
            title
        })
    }

    /// 存档快照：写进 AcpSaved.resume_session_id，GUI 重开后「重新开始」
    /// 才有旧 session id 可用来尝试真续接。
    pub fn resume_session_id_for_save(&self) -> Option<SessionId> {
        self.acp_session_id.clone()
    }

    /// 停止当前 turn（session/cancel）。agent 会以 Cancelled 收尾，相位随 TurnEnded 回 Idle。
    fn cancel_turn(&mut self) {
        if let Some(h) = &self.handle {
            let _ = h.action_tx.try_send(AcpUserAction::Cancel);
        }
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
        cx.notify();
    }

    /// 上下移动补全选中项（返回 false = 当前没在补全，按键该交回输入框）。
    fn move_completion(&mut self, delta: i32, cx: &mut Context<Self>) -> bool {
        let Some(popup) = &mut self.completion else {
            return false;
        };
        let n = popup.items.len() as i32;
        popup.selected = (popup.selected as i32 + delta).rem_euclid(n) as usize;
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
                s.set_value(merged, window, cx);
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
        let images: Vec<PromptImage> = self
            .pending_images
            .iter()
            .map(|im| PromptImage {
                mime: image_mime(im.format).to_string(),
                data_b64: base64_encode(&im.bytes),
            })
            .collect();
        if let Some(h) = &self.handle {
            if h.action_tx
                .try_send(AcpUserAction::Prompt { text, images })
                .is_ok()
            {
                self.pending_images.clear();
                cx.notify();
            }
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
        self.send_prompt(text, cx);
    }

    /// 快照应用：整份状态从 smeltd 镜像过来。归约（entries 合并/phase 机/
    /// 回声去重）已经在服务端做完了，这里只做两件事：
    /// 1. 摊平快照字段进本地同名字段，渲染代码不用碰；
    /// 2. 持久化 / 重绘时机跟着快照走。四色状态、Dock 角标、待处理通知都由
    ///    外面的集中状态订阅维护，这里不再自己判相位跳变。
    fn apply_snapshot(&mut self, snap: AcpSnapshot, cx: &mut Context<Self>) {
        let should_persist = snap.should_persist;
        let old_entries_len = self.entries.len();
        let previous_permission = self
            .permissions
            .first()
            .map(|card| (card.tool_call_id.clone(), card.question.clone()));
        if snap.entries_offset <= self.entries.len() {
            self.entries.truncate(snap.entries_offset);
            self.entries.extend(snap.entries);
        } else {
            // 不应发生：Unix stream 有序且写失败会断开。保守清空，避免显示错位历史。
            self.entries = snap.entries;
        }
        let new_entries_len = self.entries.len();
        if snap.entries_offset <= old_entries_len {
            self.list_state.splice(
                snap.entries_offset..old_entries_len,
                new_entries_len - snap.entries_offset,
            );
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
        self.acp_session_id = snap.acp_session_id.map(SessionId::new);
        self.supports_image = snap.supports_image;
        self.available_commands = snap.available_commands;
        self.usage = snap.usage;
        self.plan = snap.plan;
        self.model = snap.model;
        self.config_options = snap.config_options;
        let _ = snap.completed_unread;
        self.prune_tool_ui_state();

        if matches!(self.phase, AcpPhase::Ended(_)) {
            self.handle = None;
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
                        let what = if self.acp_session_id.is_some() {
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
            let mut buttons = h_flex().gap_2().items_center().flex_wrap();
            if let Some(pix) = primary_ix {
                let name = card.options[pix].name.clone();
                let option_id = card.options[pix].option_id.clone();
                let tool_call_id = tool_call_id.clone();
                buttons = buttons.child(
                    div()
                        .id(format!("acp-perm-primary-{option_id}"))
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .bg(gpui::rgb(ui_theme::green()))
                        .text_color(gpui::rgb(ui_theme::on_accent()))
                        .text_sm()
                        .font_semibold()
                        .cursor_pointer()
                        .hover(|d| d.opacity(0.9))
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
                buttons = buttons.child(
                    div()
                        .id(format!("acp-perm-opt-{option_id}"))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(t.border)
                        .text_sm()
                        .cursor_pointer()
                        .when(danger, |d| d.text_color(gpui::rgb(ui_theme::red())))
                        .hover(|d| d.opacity(0.85))
                        .child(opt.name.clone())
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.pick_permission(&tool_call_id, &option_id, cx);
                        })),
                );
            }
            buttons.into_any_element()
        };

        // GPUI 的可变高虚拟列表只构建视口与 overdraw 范围内的项。
        // 每项的渲染通过 Entity 回到视图，保留工具卡的展开/收起交互。
        let view = cx.entity();
        let list = virtual_list(self.list_state.clone(), move |i, _window, app| {
            view.update(app, |this, cx| {
                let t = cx.theme();
                let muted = t.muted_foreground;
                let active_permission_tool_id = this
                    .permissions
                    .first()
                    .map(|card| card.tool_call_id.as_str());
                let entry = &this.entries[i];
                let el: gpui::AnyElement = match entry {
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
                                .bg(t.muted)
                                .text_sm()
                                .child(smelt_ui::markdown_mermaid::markdown_view(
                                    ("acp-user-md", i),
                                    text.clone(),
                                )),
                        )
                        .into_any_element(),
                    AcpEntry::Assistant { text, thought } => h_flex()
                        .w_full()
                        .items_start()
                        .gap_3()
                        .child(assistant_avatar())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pt(px(2.)) // 跟头像文字视觉基线对齐
                                .text_sm()
                                .when(*thought, |d| d.text_color(muted).italic())
                                .child(smelt_ui::markdown_mermaid::markdown_view(
                                    ("acp-md", i),
                                    text.clone(),
                                )),
                        )
                        .into_any_element(),
                    AcpEntry::ToolCall {
                        id,
                        title,
                        kind,
                        status,
                        output,
                    } => {
                        let accent = tool_accent_color(kind);
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
                        let diff_totals: Vec<(usize, usize)> = output
                            .iter()
                            .filter_map(|p| match p {
                                ToolOutputPart::Diff {
                                    old_text, new_text, ..
                                } => Some(diff_line_stats(
                                    old_text.as_deref().unwrap_or(""),
                                    new_text,
                                )),
                                _ => None,
                            })
                            .collect();
                        let has_diff = !diff_totals.is_empty();
                        let (total_added, total_removed) = diff_totals
                            .iter()
                            .fold((0usize, 0usize), |(a, r), (da, dr)| (a + da, r + dr));
                        let has_pending_permission = active_permission_tool_id == Some(id.as_str());
                        let card_expanded =
                            this.tool_card_is_expanded(id, *status, has_pending_permission);

                        let header_right: gpui::AnyElement = if has_diff {
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .font_family("monospace")
                                .child(
                                    div()
                                        .text_color(gpui::rgb(ui_theme::green()))
                                        .child(format!("+{total_added}")),
                                )
                                .child(
                                    div()
                                        .text_color(gpui::rgb(ui_theme::red()))
                                        .child(format!("-{total_removed}")),
                                )
                                .into_any_element()
                        } else {
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().size_2().rounded_full().bg(status_dot))
                                .child(div().text_xs().text_color(muted).child(status_label))
                                .into_any_element()
                        };

                        let id_for_toggle = id.clone();
                        let status_for_toggle = *status;
                        let mut card = v_flex()
                            .w_full()
                            .rounded_lg()
                            .border_1()
                            .border_color(t.border)
                            .child(
                                h_flex()
                                    .id(("acp-tool-card-toggle", i))
                                    .px_4()
                                    .py_2p5()
                                    .gap_2()
                                    .items_center()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(ui_theme::overlay(0x10)))
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
                                        Icon::new(tool_kind_icon(kind))
                                            .size(px(13.))
                                            .text_color(accent),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
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
                                    ToolOutputPart::Diff {
                                        path,
                                        old_text,
                                        new_text,
                                    } => {
                                        rendered_output_part = true;
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
                                                .child(render_diff_lines(
                                                    old_text.as_deref().unwrap_or(""),
                                                    new_text,
                                                    (i, part_ix),
                                                    t.border,
                                                    t.muted_foreground,
                                                )),
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
                                        card.child(
                                            v_flex()
                                                .px_4()
                                                .pb_3()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .font_family("monospace")
                                                        .child(shown),
                                                )
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
                // `gpui::list` 不像 flex 容器那样处理 `gap`；间距必须属于
                // 虚拟项本身，否则测得的高度不包含消息间的留白。
                div().w_full().px_4().pb_4().child(el).into_any_element()
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
        let thinking = if matches!(self.phase, AcpPhase::Running)
            && !matches!(self.entries.last(), Some(AcpEntry::Assistant { .. }))
        {
            Some(
                h_flex()
                    .px_4()
                    .pb_4()
                    .items_center()
                    .gap_2()
                    .child(Spinner::new().xsmall().color(muted))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("{} 正在思考…", self.agent.short_label())),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

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
                        .max_w(px(920.))
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
                                    div()
                                        .size_2()
                                        .rounded_full()
                                        .bg(gpui::rgb(ui_theme::yellow())),
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
        // 补全弹层：画在输入框**上方**，而且是正常流式元素不是绝对定位浮层——
        // 输入框贴着窗口底边，往下开的菜单一定会被窗口边缘裁掉（组件自带那套
        // 就是这么废的，见 acp_completion.rs 文件头）。往上顶消息流反而符合
        // CLI 补全条的直觉。
        let completion_bar = self.completion.as_ref().map(|popup| {
            let mut list = v_flex()
                .id("acp-completion")
                .max_h(px(220.))
                .overflow_y_scroll()
                .border_t_1()
                .border_color(t.border)
                .bg(ui_theme::overlay(0x14));
            for (ix, item) in popup.items.iter().enumerate() {
                let selected = ix == popup.selected;
                list = list.child(
                    h_flex()
                        .id(("acp-completion-item", ix))
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .when(selected, |d| d.bg(ui_theme::overlay(0x28)))
                        .cursor_pointer()
                        .hover(|d| d.bg(ui_theme::overlay(0x20)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(if selected {
                                    gpui::rgb(ui_theme::accent())
                                } else {
                                    gpui::rgb(ui_theme::text_mid())
                                })
                                .child(item.label.clone()),
                        )
                        .when(!item.hint.is_empty(), |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(muted)
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
                    .text_xs()
                    .text_color(muted)
                    .child("↑↓ 选择 · Enter/Tab 插入 · Esc 关闭"),
            )
        });

        let input_row = self.input.as_ref().map(|input| {
            let quick_actions_enabled = matches!(self.phase, AcpPhase::Idle);
            let quick_actions: Vec<gpui::AnyElement> = if matches!(self.agent, AcpAgentKind::Codex)
            {
                [
                    ("compact", "压缩", "/compact"),
                    ("review", "审查", "/review"),
                    ("plan", "计划", "/plan"),
                ]
                .into_iter()
                .map(|(id, label, command)| {
                    let this = cx.entity();
                    Button::new(format!("acp-quick-{id}"))
                        .ghost()
                        .xsmall()
                        .label(label)
                        .disabled(!quick_actions_enabled)
                        .text_color(gpui::rgb(ui_theme::text_muted()))
                        .on_click(move |_ev, _window, cx| {
                            this.update(cx, |view, cx| {
                                view.send_prompt(command.to_string(), cx);
                            });
                        })
                        .into_any_element()
                })
                .collect()
            } else {
                Vec::new()
            };
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
                    let current_value = config
                        .options
                        .iter()
                        .find(|(_, name)| *name == config.current_name)
                        .map(|(value, _)| value.as_str())
                        .unwrap_or_default();
                    let config_label = if config.config_id == "mode" {
                        "权限".to_string()
                    } else {
                        config.name.clone()
                    };
                    let current_label =
                        config_value_label(&config.config_id, current_value, &config.current_name);
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
                                    PopupMenuItem::new(config_value_label(
                                        &config_id, &value, name,
                                    ))
                                    .checked(is_cur)
                                    .on_click(
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
                .max_w(px(920.))
                .rounded_xl()
                .border_1()
                .border_color(t.border)
                .bg(ui_theme::overlay(0x0c))
                .shadow_sm()
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .pb_2()
                        .min_h(px(88.))
                        .child(Input::new(input)),
                )
                // 待发图片的缩略图条：粘完得看得见「贴上了」，还得能反悔。
                .when(!self.pending_images.is_empty(), |col| {
                    let mut strip = h_flex().px_4().pt_3().gap_2().items_center().flex_wrap();
                    for (ix, im) in self.pending_images.iter().enumerate() {
                        strip = strip.child(
                            div()
                                .id(("acp-pending-img", ix))
                                .relative()
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
                                .children(quick_actions)
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
                .border_t_1()
                .border_color(t.border)
                .bg(ui_theme::overlay(0x08))
                .px_4()
                .py_3()
                .items_center()
                .child(composer)
        });

        let plan_bar = self.render_plan_bar(cx);

        v_flex()
            .size_full()
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
            .bg(t.background)
            .children(banner)
            .children(plan_bar)
            .child(list)
            .children(thinking)
            .children(permission)
            .children(elicitation)
            .children(completion_bar)
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

/// assistant 消息的发送方头像：主橙方块 + 首字母（设计稿色板 ACCENT），
/// 给消息流一个视觉锚点。
fn assistant_avatar() -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(24.))
        .h(px(24.))
        .rounded_md()
        .bg(gpui::rgb(ui_theme::accent()))
        .flex()
        .items_center()
        .justify_center()
        .text_color(gpui::rgb(ui_theme::on_accent()))
        .text_xs()
        .font_semibold()
        .child("C")
}

/// 工具输出默认只展开这么多行，其余折叠到「展开全部 N 行」后面。
const TOOL_OUTPUT_PREVIEW_LINES: usize = 8;

/// 整个工具卡片的默认展开策略：已完成的工具收起，正在跑、失败、待审批的工具展开。
/// 用户手动点过后由 `expanded_tool_cards` / `collapsed_tool_cards` 覆盖这个默认值。
fn tool_card_default_expanded(status: ToolCallStatus, has_pending_permission: bool) -> bool {
    has_pending_permission || !matches!(status, ToolCallStatus::Completed)
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
fn tool_accent_color(kind: &ToolKind) -> gpui::Rgba {
    match kind {
        ToolKind::Read | ToolKind::Search | ToolKind::Fetch => gpui::rgb(ui_theme::blue()),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => gpui::rgb(ui_theme::accent()),
        ToolKind::Execute => gpui::rgb(ui_theme::green()),
        ToolKind::Collaborate => gpui::rgb(ui_theme::blue()),
        ToolKind::Review => gpui::rgb(ui_theme::yellow()),
        ToolKind::Image => gpui::rgb(ui_theme::accent()),
        ToolKind::Compact | ToolKind::Wait => gpui::rgb(ui_theme::text_muted()),
        _ => gpui::rgb(ui_theme::text_muted()),
    }
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
    old: &str,
    new: &str,
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
    for line in diff_lines(old, new) {
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
            .child(div().flex_1().min_w_0().text_color(fg).child(line.text)),
        );
    }
    rows.into_any_element()
}

// strip_code_fence / is_interrupt_marker 的单测随实现一起搬进了
// smelt_core::acp_chat（见该模块的 #[cfg(test)]），这里不再重复。

#[cfg(test)]
mod tests {
    use super::{
        is_active_permission_selection, resolve_restart_launch, tool_card_default_expanded,
    };
    use smelt_core::acp_chat::ToolCallStatus;
    use smelt_core::acp_session::{
        ApprovalDetailsView, PendingPermission, PermissionOptionKindView, PermissionOptionView,
    };
    use smelt_core::agent_kind::{AcpAgentKind, AcpLaunchSpec, AcpProfile};
    use smelt_ui::agent_ui_config::AgentUiConfig;

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
    fn tool_card_defaults_keep_active_or_attention_states_expanded() {
        assert!(!tool_card_default_expanded(
            ToolCallStatus::Completed,
            false
        ));
        assert!(tool_card_default_expanded(ToolCallStatus::Completed, true));
        assert!(tool_card_default_expanded(ToolCallStatus::Pending, false));
        assert!(tool_card_default_expanded(
            ToolCallStatus::InProgress,
            false
        ));
        assert!(tool_card_default_expanded(ToolCallStatus::Failed, false));
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
