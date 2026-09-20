//! 单个终端视图：一个 Terminal + 焦点 + IME + 网格渲染 + 键盘/滚轮输入。
//! 多个 TerminalView 由 Workspace 以标签形式管理。

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use smelt_ui::daemon_states_global::{AttentionGlobal, AttentionItem, AttentionKind};
use smol::Timer;

use crate::terminal::{self, Terminal};

/// 选区高亮背景色：跟终端主题一起切换（深色用暗蓝，浅色换成不刺眼的浅蓝，
/// 否则深色定死的暗蓝铺在浅底上，选中文字会糊在一起看不清）。
///
/// `pub(crate)`：下发给移动端的配色快照要带上同一份值，两端选区色才一致
/// （见 `settings::publish_terminal_theme`）。
pub(crate) fn sel_bg() -> u32 {
    if terminal::is_dark() {
        0x000c_3d7a
    } else {
        0x00d6_eaff
    }
}

/// 搜索命中底色：普通命中暗琥珀，当前命中亮琥珀（跟选区蓝区分开）；同样跟主题走。
pub(crate) fn search_hit_bg(active: bool) -> u32 {
    match (active, terminal::is_dark()) {
        (true, true) => 0x00d4_a017,
        (true, false) => 0x00ff_c107,
        (false, true) => 0x007a_5c20,
        (false, false) => 0x00ff_e9a8,
    }
}

/// 悬停链接的高亮前景色：同上，浅色主题换成对比度够的蓝。
fn link_fg() -> u32 {
    if terminal::is_dark() {
        0x0045_9ffe
    } else {
        0x000c_64c1
    }
}

// 终端字体族配置：搬进 smelt-core（跟 markdown_mermaid 的代码块渲染共用同一份，
// 见 font_config.rs），这里重导出成原来的裸名字。
pub(crate) use smelt_core::font_config::{DEFAULT_FONT_FAMILY, font_family, set_font_family};

/// 兜底等宽字体：macOS 系统自带，必定存在。放在 fallback 链末尾做最后防线，
/// 保证 cell_w 与实际字形宽度同源——否则测量和渲染各自 fallback 到不同字体，
/// 列数按错误的字宽算出来，终端内容只占窗格的一个恒定比例（用户实测约一半宽）。
const MONO_FALLBACK_FONT: &str = "Menlo";

/// 终端字体（用户配置的主字体 + 内嵌默认字体 + 系统等宽兜底）。渲染和测量都必须
/// 用这个，保持字形来源一致——否则测量用的字体和实际渲染用的字体对某个字符的
/// fallback 结果不一样，会导致列宽计算和实际显示对不上（拖选/鼠标定位跑偏、内容
/// 占宽错误）。DEFAULT_FONT_FAMILY 已内嵌进二进制（见 main.rs 的 add_fonts）且
/// 自带全部 Nerd Font 图标码位：用户自选字体缺图标时落到它，不必单独嵌图标字体。
fn terminal_font() -> Font {
    Font {
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            DEFAULT_FONT_FAMILY.to_string(),
            MONO_FALLBACK_FONT.to_string(),
        ])),
        ..font(font_family())
    }
}

// Tab / Shift-Tab 在终端聚焦时的专属动作。
//
// gpui-component 的 `Root` 全局把 "tab"/"shift-tab" 绑成了焦点跳转（`window.focus_next`），
// context 是 "Root"——而 GPUI 按键分发时，keymap 匹配到的 action 会在
// `on_key_down` 之类的原始按键监听器之前就被消费掉，根本轮不到终端自己处理，
// 导致 Tab 补全在终端里形同虚设。这里在 "Terminal" 这个更贴近焦点的 context 上
// 重新绑一份，深度更深的 context 按 GPUI 的 keymap 优先级规则会盖过 Root 那份，
// 从而把 Tab/Shift-Tab 交还给终端本身（见下方 render 里的 `.on_action`）。
gpui::actions!(
    smelt_terminal,
    [
        TerminalTab,
        TerminalBackTab,
        TerminalFind,
        TerminalFindNext,
        TerminalFindPrev,
        TerminalFindClose
    ]
);

/// 终端字号（px）：跟随设置页「字体大小」全局切换，见 `set_font_px`。单进程只有
/// 一套终端字号，用全局原子量足够，不必给每处渲染/量measure 调用各传一份——
/// 跟 terminal.rs 的 DARK_MODE 是同一路数。
static FONT_PX_ATOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(13);
/// 字号可调范围：太小认不清字形，太大一屏放不下几列，都没意义。
pub const MIN_FONT_PX: u32 = 9;
pub const MAX_FONT_PX: u32 = 22;

/// 切换终端字号（px，自动夹到 [MIN_FONT_PX, MAX_FONT_PX]）。
pub fn set_font_px(px: u32) {
    FONT_PX_ATOM.store(
        px.clamp(MIN_FONT_PX, MAX_FONT_PX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// 当前终端字号（px）。
pub fn font_px() -> f32 {
    FONT_PX_ATOM.load(std::sync::atomic::Ordering::Relaxed) as f32
}

/// 行高：固定按原始 18/13 的比例跟字号一起缩放（原设计：13px 字对 18px 行高）。
fn line_px() -> f32 {
    font_px() * (18.0 / 13.0)
}

/// 等宽字宽 ≈ 字号 × 该比例（用于从窗口宽度估算列数）。
const CELL_W_RATIO: f32 = 0.6;
/// 终端内容的每侧内边距（避免文字贴边被裁）。canvas 覆盖层保持满尺寸，
/// 只把网格原点按此偏移，故鼠标/IME 坐标一致，网格可用区 = 尺寸 − 2×PAD。
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 8.0;
/// Shift+PageUp/Down 每次滚动的行数。
const PAGE_LINES: i32 = 20;

/// 一个内嵌终端视图。
pub struct TerminalView {
    terminal: Terminal,
    /// 每次替换底层连接递增。旧连接的 redraw 任务退出时只能重连自己的 generation，
    /// 避免显式重启/其它成功重连已经装上新 Terminal 后又被迟到任务覆盖。
    terminal_generation: u64,
    focus_handle: FocusHandle,
    did_focus: bool,
    /// 上一帧的焦点状态，用来把「焦点变了」这件事上报给应用（DEC 1004，见 report_focus）。
    was_focused: bool,
    /// 上一帧的网格快照，渲染和命中测试共用（Zed 同样把 last_content 缓存在 model 上）。
    /// snapshot() 会把整个网格连同颜色/属性 clone 一遍，而 url_at / link_range_at /
    /// char_steps_between 都是鼠标事件里调的——按住 Cmd 划过屏幕时每个 move 事件都全量
    /// clone 一次实在太浪费。鼠标事件必然发生在刚渲染过的终端上，用上一帧足够。
    last_frame: Option<Rc<terminal::Frame>>,
    /// 输入法合成中的预编辑文本（未提交），仅用于满足 IME 协议，不发给 PTY。
    marked_text: Option<String>,
    title: String,
    /// 用户在侧栏子行右键改过的 pane 名；None = 用自动推导的标题。
    /// 跟着 `PaneState::Leaf` 一起持久化，重开 GUI 按 session_id reattach 时灌回来。
    custom_title: Option<String>,
    /// 初始工作目录（新建标签继承用）。
    cwd: Option<String>,
    /// 是否正在拖动框选（mouse_down 置位、mouse_up 清）。选区本体存在 alacritty 的
    /// Term.selection 里（缓冲区绝对坐标，滚动跟随/新输出漂移由它维护），这里只记
    /// 「拖没拖着」这个交互态。
    selecting: bool,
    /// 应用鼠标上报模式：mousedown 时已把 press 转发给 TUI，后续 drag/release 也走
    /// 应用路径，不再做本地框选。按住 Shift 强制本地选区（xterm 约定旁路）。
    app_mouse: bool,
    /// 终端内搜索：打开时顶部出输入条（Cmd+F）；命中高亮在 paint 里画。
    search_open: bool,
    search_input: Option<Entity<gpui_component::input::InputState>>,
    _search_sub: Option<Subscription>,
    /// 当前可视区内所有搜索命中（含 active）；关搜索时清空。
    search_hits: Vec<terminal::SearchHit>,
    /// 「3/12」；total=0 表示无结果。
    search_status: terminal::SearchStatus,
    /// 滚动条拖动中：记录按下时 thumb 内偏移（像素），None = 没在拖。
    scrollbar_drag: Option<f32>,
    /// 拖到可视区上/下边缘时的自动滚动方向：0 不滚，正=向上看历史，负=向下。
    drag_scroll: i32,
    /// 自动滚动期间选区活动端使用的列（沿用最后一次拖动事件的列）。
    drag_scroll_col: usize,
    /// 自动滚动定时器是否已在跑（防重复 spawn）。
    drag_scroll_running: bool,
    /// 上次测得的等宽字符像素宽（鼠标坐标换算用）。
    cell_w: f32,
    /// 网格原点（含内边距）的窗口像素坐标，由 canvas 在 paint 时写入。
    grid_origin: Rc<StdCell<(f32, f32)>>,
    /// 终端自身像素尺寸 (宽, 高)，由 canvas 写入；按卡片大小算行列（网格 Hub 用）。
    grid_size: Rc<StdCell<(f32, f32)>>,
    /// 当前 Cmd 悬停的链接范围：命中的每个物理行各一段 (行, 起列, 止列)，用于高亮 +
    /// 切换鼠标样式。软换行的长链接可能横跨多行，所以是个列表而不是单一区间（#21）。
    hover_url: Option<Vec<(usize, usize, usize)>>,
    /// 最近一帧的光标位置 (行, 列)，供 IME 定位候选窗（bounds_for_range）。
    cursor: Option<(usize, usize)>,
    /// 结构化状态上一帧是否已经是 Succeeded；hook 激活后完成边沿以此为准，
    /// 防止同一完成快照重复产生关注事件。
    was_structured_succeeded: bool,
    /// 守护里的会话 id（持久化到工作区快照；重开 GUI 按它 reattach）。
    session_id: String,
    /// 刚收到但尚未展示的 BEL。已有结构化状态后抑制它，避免重复通知。
    pending_bell_at: Option<Instant>,
    /// 使已经过期的 BEL grace timer 失效。
    bell_timer_generation: u64,
    /// 触控板滚轮的像素余数：触控板每帧只送几像素的增量，若逐事件独立按
    /// LINE_PX 取整会把大部分小增量截断成 0（滚了但没反应），造成"很不跟手"
    /// 的卡顿感。改为跨事件累加像素，攒够一整行再吐出、余数留到下次。
    scroll_accum: f32,
    /// 建终端时的启动方式（侧栏行图标用，见 `LaunchKind`）。
    launch_kind: LaunchKind,
    /// 快捷启动项的显示名（设置里配的 label）。侧栏标题在 agent 还没上报任务名时
    /// 回退到它，而不是 cwd 末段——否则「+ → Claude Code」建出来却显示项目名。
    launch_label: Option<String>,
    /// 快捷启动实际命令行。仅用于标识这个 pane 最初的启动方式；daemon 中会话
    /// 丢失后不会重跑该命令。
    launch_cmd: Option<String>,
    /// 首帧布局或 reattach 后强制发一次 PTY resize（含真实 cell 像素）。reattach
    /// 后守护 jolt 用 cell=0；普通 `resize` 同尺寸早退——两者都盖不住「同网格但缺
    /// 像素」的 TUI 排版。
    pty_kick_pending: bool,
    /// 断线自动重连是否已在跑（防并发：多个触发点同时 schedule 时只起一个后台任务）。
    reconnecting: bool,
    /// attachment 已断开但尚未重连时的用户输入。这里只收「明确没有进入旧写队列」
    /// 的输入，重连后按原顺序补发，不能把用户键入当成一次可丢的 UI 事件。
    recovery_input: VecDeque<RecoveryInput>,
    recovery_input_bytes: usize,
    /// 无法恢复时（例如会话已实际退出，或恢复队列超限）的单次提示。
    write_error: Option<String>,
    /// 当前 epoch 已确认没有 runtime。之后不再重连、不再把按键放进恢复队列，
    /// 避免 make install / 守护换代后连弹「未发送的输入已取消」。
    session_runtime_gone: bool,
}

/// 断线窗口内尚未进入旧 attachment 的用户输入。粘贴保留原文本，重连后按新终端
/// 当前的 bracketed-paste 模式编码；普通按键保留已经确定的终端字节序列。
enum RecoveryInput {
    Bytes(Vec<u8>),
    Paste(String),
}

impl RecoveryInput {
    fn byte_len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Paste(text) => text.len(),
        }
    }
}

/// 同一终端同文本的系统通知最小间隔。
/// BEL 常和 agent 的完成信号出现在同一批输出中。稍等一帧窗口，让更准确的
/// Succeeded/Failed/Waiting 通知优先，避免通知中心同时出现“响铃”和“已完成”。
const BELL_NOTIFICATION_GRACE: Duration = Duration::from_millis(250);
/// 与底层 writer 的待写输入上限一致。超过这个量时继续保存只会放大内存占用，且用户
/// 已经无法确认整段内容是否应在旧会话结束后补发。
const TERMINAL_RECOVERY_INPUT_MAX_BYTES: usize = 64 * 1024 * 1024;
const TERMINAL_RECOVERY_OVERFLOW_MESSAGE: &str = "终端重连期间待发送的输入过多，未发送部分已取消";
const TERMINAL_SESSION_ENDED_MESSAGE: &str = "终端会话已结束，未发送的输入已取消";

fn bell_notification_due(pending_at: Option<Instant>, now: Instant) -> bool {
    pending_at.is_some_and(|at| now.duration_since(at) >= BELL_NOTIFICATION_GRACE)
}

/// PTY 批次只有在网格内容或守护几何状态变化时才需要重画终端。
/// 后者不能只靠 alacritty damage：远端接管/释放尺寸租约时，行列可能完全没变。
fn terminal_event_needs_redraw(grid_damaged: bool, daemon_geometry_changed: bool) -> bool {
    grid_damaged || daemon_geometry_changed
}

fn fallback_attention(
    bell_due: bool,
    osc: Option<String>,
) -> Option<(AttentionKind, &'static str, String)> {
    if bell_due {
        return Some((AttentionKind::Bell, "响铃", "🔔 响铃".to_string()));
    }
    osc.map(|message| (AttentionKind::Notice, "终端通知", message))
}

fn daemon_agent_state(
    session_id: &str,
    cx: &App,
) -> Option<smelt_core::daemon_state::DaemonSessionState> {
    smelt_ui::daemon_states_global::DaemonStates::get(session_id, cx)
}

fn terminal_bell_notifications_enabled(cx: &App) -> bool {
    cx.try_global::<smelt_ui::agent_host_state::AgentHostState>()
        .map(|config| config.notify_terminal_bell)
        .unwrap_or(true)
}

/// 建终端时用的启动方式，决定侧栏行图标——跟「+」下拉菜单里注册的终端 agent
/// 一一对应，一眼认出这一行是哪种会话。
/// 建好之后不变：daemon 重启触发的 `reconnect()` 只换底层连接，不重置这个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    Terminal,
    Agent(crate::settings::TerminalAgentKind),
}

impl LaunchKind {
    /// 反过来映射回 `TerminalAgentKind`（`Terminal` 没有对应种类）。可直接接到
    /// `settings::icon_for_agent_kind` 等工具函数，不用再维护一份 Claude→Asterisk 这种 match。
    pub fn agent_kind(self) -> Option<crate::settings::TerminalAgentKind> {
        match self {
            Self::Terminal => None,
            Self::Agent(agent) => Some(agent),
        }
    }
}

/// 从 `launch` 命令行猜启动方式。前缀匹配的判断本体是
/// `TerminalAgentKind::from_command_prefix`——跟「+」菜单图标
/// (`icon_for_launch_command`) 共用同一份逻辑，以后加参数或 agent 不会失配。
fn classify_launch(launch: Option<&str>) -> LaunchKind {
    launch
        .and_then(crate::settings::TerminalAgentKind::from_command_prefix)
        .map(LaunchKind::Agent)
        .unwrap_or(LaunchKind::Terminal)
}

impl TerminalView {
    /// 用已 spawn 的 `Terminal` 包一层视图。**这是唯一的构造入口**——先
    /// `Terminal::spawn`（可失败）再决定是否建 Entity；不提供包着 expect 的
    /// 便捷构造：所有调用方都在 GPUI 的 ObjC 回调栈上（启动
    /// did_finish_launching / 用户事件），panic 不能跨 FFI unwind，一炸就是
    /// 整个 app abort——历史上「重启就崩」「拖文件夹就崩」都是它。
    pub fn from_terminal(
        cx: &mut Context<Self>,
        terminal: Terminal,
        cwd: Option<String>,
        session_id: String,
        launch: Option<&str>,
        launch_label: Option<&str>,
    ) -> Self {
        // Zed 式事件驱动重绘：读线程一有新内容就唤醒这里 cx.notify()（见 drive_redraws）。
        Self::drive_redraws(terminal.redraw_channel(), 0, cx);
        let launch_kind = classify_launch(launch);
        let launch_cmd = launch
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let launch_label = launch_label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // 标签标题：取工作目录最后一段
        let title = cwd
            .as_deref()
            .and_then(|p| p.trim_end_matches('/').rsplit('/').next())
            .filter(|s| !s.is_empty())
            .unwrap_or("终端")
            .to_string();

        let mut view = Self {
            terminal,
            terminal_generation: 0,
            focus_handle: cx.focus_handle(),
            did_focus: false,
            was_focused: false,
            last_frame: None,
            marked_text: None,
            title,
            custom_title: None,
            cwd,
            selecting: false,
            app_mouse: false,
            search_open: false,
            search_input: None,
            _search_sub: None,
            search_hits: Vec::new(),
            search_status: terminal::SearchStatus::default(),
            scrollbar_drag: None,
            drag_scroll: 0,
            drag_scroll_col: 0,
            drag_scroll_running: false,
            cell_w: 8.0,
            grid_origin: Rc::new(StdCell::new((0.0, 0.0))),
            grid_size: Rc::new(StdCell::new((0.0, 0.0))),
            hover_url: None,
            cursor: None,
            was_structured_succeeded: false,
            session_id,
            pending_bell_at: None,
            bell_timer_generation: 0,
            scroll_accum: 0.0,
            launch_kind,
            launch_label,
            launch_cmd,
            pty_kick_pending: true,
            reconnecting: false,
            recovery_input: VecDeque::new(),
            recovery_input_bytes: 0,
            write_error: None,
            session_runtime_gone: false,
        };
        if let Some(state) = daemon_agent_state(&view.session_id, cx) {
            view.handle_daemon_state(&state, cx);
        }
        view
    }

    /// 建终端时的启动方式（侧栏行图标对齐「+」菜单用）。
    pub fn launch_kind(&self) -> LaunchKind {
        self.launch_kind
    }

    /// 快捷启动项显示名（见字段注释）；普通「新建终端」为 None。
    pub fn launch_label(&self) -> Option<&str> {
        self.launch_label.as_deref()
    }

    /// 快捷启动实际命令行；裸终端为 None。
    pub fn launch_cmd(&self) -> Option<&str> {
        self.launch_cmd.as_deref()
    }

    /// 守护里的会话 id（关 pane 时用它让守护真正杀掉 shell）。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 驱动重绘的常驻任务：await 读线程的唤醒 → `cx.notify()`。内容一到就画，
    /// 不靠轮询。读线程退出（发送端 drop）时若当前连接仍是这一代且已标记 dead，
    /// 则触发自动重连；换上新连接后旧 channel 的 Err 不会误触发。
    fn drive_redraws(rx: smol::channel::Receiver<()>, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.handle_terminal_event(cx))
                    .is_err()
                {
                    return; // 视图已销毁
                }
            }
            // 读线程退出：连接断了。dead 判断放在 update 里，读的是「当前」Terminal。
            let _ = this.update(cx, |this, cx| {
                if this.terminal_generation == generation && this.terminal.is_dead() {
                    this.schedule_auto_reconnect(generation, cx);
                }
            });
        })
        .detach();
    }

    /// PTY 读线程每批输出只唤醒一次。终端内容与信息性 OSC/BEL 通知都在这里消费。
    fn handle_terminal_event(&mut self, cx: &mut Context<Self>) {
        let daemon_state = daemon_agent_state(&self.session_id, cx);
        let structured_events = daemon_state
            .as_ref()
            .is_some_and(|state| state.structured_events);
        let bell_received = self.terminal.take_bell_notification().is_some();
        let osc = self.terminal.take_notification();
        let now = Instant::now();
        let bell_notifications_enabled = terminal_bell_notifications_enabled(cx);

        // 结构化插件一旦激活，OSC/BEL fallback 永久让位给明确的 phase。
        if structured_events || !bell_notifications_enabled {
            self.invalidate_bell_timer();
        } else if bell_received {
            self.pending_bell_at = Some(now);
            self.bell_timer_generation = self.bell_timer_generation.wrapping_add(1);
            self.schedule_bell_timer(cx);
        }
        let bell_due = !structured_events
            && bell_notifications_enabled
            && bell_notification_due(self.pending_bell_at, now);
        if let Some((kind, title, message)) =
            fallback_attention(bell_due, (!structured_events).then_some(osc).flatten())
        {
            if kind == AttentionKind::Bell {
                self.invalidate_bell_timer();
            }
            self.publish_attention(kind, title, message, cx);
        }

        self.handle_structured_state(daemon_state.as_ref(), cx);

        let daemon_geometry_changed = self.terminal.sync_daemon_geometry();
        let grid_damaged = self.terminal.take_damage();
        if terminal_event_needs_redraw(grid_damaged, daemon_geometry_changed) {
            // 滚动/输出/网格尺寸变了：搜索高亮要按新的可视区重算命中。
            self.refresh_search_highlights();
            cx.notify();
        }
        // 纯 OSC 标题 / BEL 没有终端网格 damage：标题由 daemon 状态
        // 订阅合并刷新侧栏，提醒由 AttentionGlobal 投递，都不应重画画布。
    }

    fn invalidate_bell_timer(&mut self) {
        self.pending_bell_at = None;
        self.bell_timer_generation = self.bell_timer_generation.wrapping_add(1);
    }

    fn schedule_bell_timer(&mut self, cx: &mut Context<Self>) {
        let generation = self.bell_timer_generation;
        cx.spawn(async move |this, cx| {
            Timer::after(BELL_NOTIFICATION_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.bell_timer_generation != generation {
                    return;
                }
                let structured = daemon_agent_state(&this.session_id, cx)
                    .as_ref()
                    .is_some_and(|state| state.structured_events);
                if structured || !terminal_bell_notifications_enabled(cx) {
                    this.invalidate_bell_timer();
                    return;
                }
                if this.flush_bell_if_due(Instant::now(), cx) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn flush_bell_if_due(&mut self, now: Instant, cx: &mut Context<Self>) -> bool {
        if bell_notification_due(self.pending_bell_at, now) {
            self.invalidate_bell_timer();
            self.publish_attention(AttentionKind::Bell, "响铃", "🔔 响铃".to_string(), cx);
            true
        } else {
            false
        }
    }

    fn handle_structured_state(
        &mut self,
        daemon_state: Option<&smelt_core::daemon_state::DaemonSessionState>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = daemon_state.filter(|state| state.phase_is_authoritative()) else {
            return;
        };
        let succeeded = state.phase == crate::terminal::DaemonPhase::Succeeded;
        if succeeded && !self.was_structured_succeeded {
            self.finish_structured_session(cx);
        }
        self.was_structured_succeeded = succeeded;
    }

    /// 由 daemon 状态订阅直接分发，独立于该 pane 当前是否正在渲染。
    pub fn handle_daemon_state(
        &mut self,
        state: &smelt_core::daemon_state::DaemonSessionState,
        cx: &mut Context<Self>,
    ) {
        if state.id != self.session_id {
            return;
        }
        self.handle_structured_state(Some(state), cx);
        // phase / 标题由 Workspace 的 daemon 订阅统一刷新侧栏。这个 view
        // 只处理结构化完成边沿，不要因此重画终端网格。
    }

    fn finish_structured_session(&mut self, cx: &mut Context<Self>) {
        self.publish_attention(AttentionKind::Success, "已完成", "已完成".to_string(), cx);
    }

    /// 断线自动重连（后台，带退避）。守护 exec 交接 / 被强杀 / 重启时，会话本身
    /// 还活在守护里，reattach 即可恢复画面；只有 shell 真的退出/被杀（守护里查无
    /// 此会话）才不复活。
    ///
    /// 这是终端断线的**兜底自愈**：此前重连只挂在 `upgrade_daemon_seamless` 一条
    /// 路径上，而守护 exec 还有别的触发方式（ensure/handoff 迁移、装 App 时
    /// handoff、异常重启），那些路径断开连接后没有任何人重连终端 → 全部终端
    /// 永久冻结、只能重启 GUI。这里跟状态订阅通道的 2s 重连循环同一个思路，
    /// 让终端自己长出一条命来，不依赖调用方。
    fn schedule_auto_reconnect(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.reconnecting || self.session_runtime_gone {
            return;
        }
        self.reconnecting = true;
        let sid = self.session_id.clone();
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            // 退避：500ms 起步，翻倍到 10s 封顶，重到成功或会话消失为止。
            let mut delay = Duration::from_millis(500);
            loop {
                let still_current = this
                    .update(cx, |this, _| {
                        this.terminal_generation == generation && !this.session_runtime_gone
                    })
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
                // 1) 守护活着吗？exec 期间 socket 短暂不可连 / 守护还没起来时，
                //    都不能判「会话没了」——那是要等的，不是要放弃的。
                let daemon_up = cx
                    .background_executor()
                    .spawn(async { terminal::daemon_info().is_some() })
                    .await;
                let sid2 = sid.clone();
                let alive = this
                    .update(cx, |_, cx| {
                        smelt_ui::daemon_states_global::DaemonStates::runtime_alive(&sid2, cx)
                    })
                    .ok()
                    .flatten();
                match smelt_core::daemon_state::terminal_reconnect_action(daemon_up, alive) {
                    smelt_core::daemon_state::TerminalReconnectAction::Wait => {
                        cx.background_executor().timer(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(10));
                        continue;
                    }
                    smelt_core::daemon_state::TerminalReconnectAction::GiveUp => {
                        let _ = this.update(cx, |this, cx| {
                            if this.terminal_generation == generation {
                                this.mark_session_runtime_gone(TERMINAL_SESSION_ENDED_MESSAGE);
                                cx.notify();
                            }
                        });
                        break;
                    }
                    smelt_core::daemon_state::TerminalReconnectAction::Reattach => {}
                }
                // 3) 重连（reattach：守护按 id 重放历史画面，输出不丢）。
                // 镜像未知时也立刻 attach：tmux 式，attach 本人才是还活着的权威答案。
                let sid3 = sid.clone();
                let cwd3 = cwd.clone();
                let term = cx
                    .background_executor()
                    .spawn(async move { Terminal::reattach(24, 80, cwd3.as_deref(), &sid3) })
                    .await;
                match term {
                    Ok(t) => {
                        // 主线程挂回：adopt_terminal 换 Terminal、清 attention、
                        // 重置交互态并触发重绘。
                        let done = this.update(cx, |this, cx| {
                            if this.terminal_generation != generation {
                                return;
                            }
                            this.reconnecting = false;
                            this.adopt_terminal(t, cx);
                        });
                        if done.is_err() {
                            return; // 视图已销毁
                        }
                        break;
                    }
                    Err(_) => {
                        cx.background_executor().timer(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(10));
                    }
                }
            }
            // 会话消失（shell 退出）或视图销毁：复位标志，下次断线还能再触发。
            let _ = this.update(cx, |this, _| {
                if this.terminal_generation == generation {
                    this.reconnecting = false;
                }
            });
        })
        .detach();
    }

    /// 用已经在后台线程建好的 [`Terminal`] 替换当前连接（硬重启守护后批量重连用）。
    pub fn adopt_terminal(&mut self, terminal: Terminal, cx: &mut Context<Self>) {
        self.terminal_generation = self.terminal_generation.wrapping_add(1);
        self.terminal = terminal;
        // 旧 Terminal 一 drop，它读线程的发送端随之关闭，老的 redraw 任务 recv 到 Err
        // 自行退出；这里给新连接挂一个新的重绘任务。
        Self::drive_redraws(self.terminal.redraw_channel(), self.terminal_generation, cx);
        self.clear_attention(cx);
        self.was_structured_succeeded = false;
        self.invalidate_bell_timer();
        // 新 Terminal 自带空选区，只需重置本视图的拖选 / 应用鼠标交互态。
        self.selecting = false;
        self.app_mouse = false;
        self.drag_scroll = 0;
        self.cursor = None;
        // 重连后必须再 force 一次带 cell 像素的 resize（见 pty_kick_pending）。
        self.pty_kick_pending = true;
        self.session_runtime_gone = false;
        self.flush_recovery_input(cx);
        if let Some(state) = daemon_agent_state(&self.session_id, cx) {
            self.handle_daemon_state(&state, cx);
        }
        cx.notify();
    }

    fn publish_attention(&self, kind: AttentionKind, title: &str, message: String, cx: &mut App) {
        if cx.try_global::<AttentionGlobal>().is_some() {
            AttentionGlobal::publish_terminal_notification(
                AttentionItem {
                    session_id: self.session_id.clone(),
                    title: title.to_string(),
                    message,
                    kind,
                },
                Instant::now(),
                cx,
            );
        }
    }

    fn clear_attention(&self, cx: &mut App) {
        if cx.try_global::<AttentionGlobal>().is_some() {
            AttentionGlobal::mark_read(&self.session_id, cx);
        }
    }

    pub fn mark_read(&mut self, cx: &mut Context<Self>) {
        self.clear_attention(cx);
    }

    /// agent 报告的终端标题（含任务名 + 状态符号）；供侧栏 / 总览显示。
    pub fn agent_title(&self) -> Option<String> {
        self.terminal.current_title()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// 用户给这个 pane 起的名字；None = 还没改过名。
    pub fn custom_title(&self) -> Option<&str> {
        self.custom_title.as_deref()
    }

    /// 改名。传 None（或提交空串）= 清掉自定义名，回退到自动推导的标题。
    pub fn set_custom_title(&mut self, title: Option<String>) {
        self.custom_title = title.filter(|s| !s.trim().is_empty());
    }

    pub fn cwd(&self) -> Option<String> {
        self.cwd.clone()
    }

    /// 从外部写一段文本到 PTY（等价于粘贴），供 diff 视图「发到终端」等场景复用。
    /// 走 [`Terminal::paste`]：bracketed paste + 换行规范化，跟 Cmd+V 同一条路。
    ///
    /// **不会提交**：Claude 等开了 bracketed paste 时，粘贴内容里的 `\n` 只是多行文本。
    /// 若需回车执行，调用方应在文本末尾附带回车符（`\r`）。
    pub fn send_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.terminal.paste(text) {
            self.defer_input(RecoveryInput::Paste(text.to_string()));
            self.recover_input_transport(cx);
        }
        self.clear_attention(cx);
        cx.notify();
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.terminal.paste(text) {
            self.defer_input(RecoveryInput::Paste(text.to_string()));
            self.recover_input_transport(cx);
        }
        self.clear_attention(cx);
        cx.notify();
    }

    fn send_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        let accepted = self.terminal.send_input(bytes);
        if !accepted {
            self.defer_input(RecoveryInput::Bytes(bytes.to_vec()));
            self.recover_input_transport(cx);
            cx.notify();
        }
        accepted
    }

    /// 只会在底层 writer 明确拒绝整段输入后调用（见 `TerminalWriter::send_input`）。
    /// 因而不需要猜测这段输入是否已经抵达 PTY：它可以安全地放入恢复队列。
    fn defer_input(&mut self, input: RecoveryInput) {
        if self.session_runtime_gone {
            return;
        }
        let byte_len = input.byte_len();
        if byte_len == 0 {
            return;
        }
        if self.recovery_input_bytes.saturating_add(byte_len) > TERMINAL_RECOVERY_INPUT_MAX_BYTES {
            self.write_error = Some(TERMINAL_RECOVERY_OVERFLOW_MESSAGE.to_string());
            return;
        }
        self.recovery_input_bytes += byte_len;
        self.recovery_input.push_back(input);
    }

    /// 读通道 EOF 会触发同一条重连路径；写通道先失效时不能等 EOF 的调度时机，
    /// 否则用户已经键入的内容会停在一个没有消费者的窗口里。
    fn recover_input_transport(&mut self, cx: &mut Context<Self>) {
        if self.session_runtime_gone {
            return;
        }
        self.schedule_auto_reconnect(self.terminal_generation, cx);
    }

    /// 新 attachment 已就绪后补发断线期间未进入旧写队列的输入。若新连接又立即
    /// 失效，把当前项放回队首，下一轮重连继续保持原始顺序。
    fn flush_recovery_input(&mut self, cx: &mut Context<Self>) {
        while let Some(input) = self.recovery_input.pop_front() {
            let byte_len = input.byte_len();
            self.recovery_input_bytes = self.recovery_input_bytes.saturating_sub(byte_len);
            let accepted = match &input {
                RecoveryInput::Bytes(bytes) => self.terminal.send_input(bytes),
                RecoveryInput::Paste(text) => self.terminal.paste(text),
            };
            if accepted {
                continue;
            }
            self.recovery_input_bytes += byte_len;
            self.recovery_input.push_front(input);
            self.recover_input_transport(cx);
            return;
        }
    }

    fn mark_session_runtime_gone(&mut self, message: &str) {
        self.session_runtime_gone = true;
        self.discard_recovery_input_if_any(message);
    }

    fn discard_recovery_input_if_any(&mut self, message: &str) {
        if self.recovery_input.is_empty() {
            return;
        }
        self.recovery_input.clear();
        self.recovery_input_bytes = 0;
        self.write_error = Some(message.to_string());
    }

    /// 打开终端内搜索条（Cmd+F）。输入框获焦；Enter 下一个，Shift+Enter 上一个，Esc 关闭。
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};
        if self.search_open {
            if let Some(input) = &self.search_input {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("在终端中查找…"));
        input.update(cx, |s, cx| s.focus(window, cx));
        self._search_sub = Some(cx.subscribe_in(
            &input,
            window,
            |this, input, ev: &InputEvent, _window, cx| {
                match ev {
                    InputEvent::PressEnter { shift, .. } => {
                        let q = input.read(cx).value().to_string();
                        this.run_search(&q, *shift, cx);
                    }
                    InputEvent::Change => {
                        let q = input.read(cx).value().to_string();
                        // 边输入边重建命中列表，全部高亮；不滚动（等 Enter 再跳）。
                        this.search_status = this.terminal.set_search_query(&q);
                        this.search_hits = this.terminal.viewport_search_hits();
                        cx.notify();
                    }
                    _ => {}
                }
            },
        ));
        self.search_input = Some(input);
        self.search_open = true;
        self.search_hits.clear();
        self.search_status = terminal::SearchStatus::default();
        self.terminal.clear_search();
        cx.notify();
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_input = None;
        self._search_sub = None;
        self.search_hits.clear();
        self.search_status = terminal::SearchStatus::default();
        self.terminal.clear_search();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn run_search(&mut self, query: &str, backward: bool, cx: &mut Context<Self>) {
        self.search_status = self.terminal.find_next(query, backward);
        self.search_hits = self.terminal.viewport_search_hits();
        cx.notify();
    }

    fn refresh_search_highlights(&mut self) {
        if self.search_open {
            // 内容变了：触发异步重扫（内部有节流），高亮先用当前缓存映射。
            if let Some(q) = self.terminal.current_search_query() {
                self.terminal.set_search_query(&q);
            }
            self.search_hits = self.terminal.viewport_search_hits();
            self.search_status = self.terminal.search_status();
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// 窗口像素坐标 → 网格单元 (行, 列)。
    ///
    /// 直接按均匀格宽换算即可：render_row 已经把每一批文本钉死在 col * cell_w 上，
    /// 画面就是标准网格。（早先渲染靠字体 advance 自由流，中文一多就整体左漂，这里不得不
    /// 「重新整形一遍该行、用 index_for_x 反查列号」去复现那个歪掉的几何——渲染掰正之后
    /// 那套 workaround 反而会让鼠标跟画面对不上，已随之删除。）
    fn pos_to_cell(&self, pos: Point<Pixels>, _window: &mut Window) -> (usize, usize) {
        let (ox, oy) = self.grid_origin.get();
        let x = (f32::from(pos.x) - ox).max(0.0);
        let y = (f32::from(pos.y) - oy).max(0.0);
        let row = (y / line_px()).floor() as usize;
        let col = (x / self.cell_w.max(1.0)).floor() as usize;
        (row, col)
    }

    /// 窗口像素 x 落在其网格单元的左半还是右半：选区端点的 Side。alacritty 用它
    /// 决定端点格是否纳入选区（同格同侧 = 空选区），于是单击/同格微抖不会误选出
    /// 一格——否则 mouse_up 会把这次点击当成拖选，不再转发给开了鼠标上报的 TUI。
    fn pos_in_left_half(&self, pos: Point<Pixels>) -> bool {
        let (ox, _) = self.grid_origin.get();
        let x = (f32::from(pos.x) - ox).max(0.0);
        (x / self.cell_w.max(1.0)).fract() < 0.5
    }

    /// 拖选拖出可视区上/下边缘后的自动滚动循环：每 60ms 按 drag_scroll 方向滚一行，
    /// 并把选区活动端钉在对应边缘行（行传 0 / usize::MAX，由 selection_update 夹回
    /// 可视区），一边滚一边扩选。松开鼠标或拖回区内即停，定时器自行退出。
    fn start_drag_scroll(&mut self, cx: &mut Context<Self>) {
        if self.drag_scroll_running {
            return;
        }
        self.drag_scroll_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(60)).await;
                let go = this.update(cx, |this, cx| {
                    if !this.selecting || this.drag_scroll == 0 {
                        this.drag_scroll_running = false;
                        return false;
                    }
                    let dir = this.drag_scroll;
                    this.terminal.scroll(dir);
                    // 向上滚活动端钉在首行（扩向更早内容），向下钉在末行；Side 取
                    // 扩选方向的外侧，保证边缘行的端点格被选进来。
                    let row = if dir > 0 { 0 } else { usize::MAX };
                    this.terminal
                        .selection_update(row, this.drag_scroll_col, dir > 0);
                    cx.notify();
                    true
                });
                if !matches!(go, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// 某行 [a, b) 两个网格列之间要按几次左右方向键才能跨过去——不能直接拿列号
    /// 相减：宽字符（中/日/韩等）占两格但对 shell 的行编辑器来说只是一个字符，一次
    /// 方向键跨的是「一个字符」而不是「一格」。按列差算会在宽字符行里按过头（见
    /// Option+点击移动光标的调用处）。真正的字符数 = 该区间内非占位格（ch != '\0'）
    /// 的格子数——占位格是 terminal.rs 里宽字符后面那个跳过的空壳格，不代表独立字符。
    fn char_steps_between(&self, row: usize, a: usize, b: usize) -> usize {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let Some(cells) = self.last_frame.as_ref().and_then(|f| f.rows.get(row)) else {
            return hi - lo;
        };
        cells[lo..hi.min(cells.len())]
            .iter()
            .filter(|c| c.ch != '\0')
            .count()
    }

    /// 点击单元处若落在某个链接上，返回该目标（未做 file:// 转换，打开前还要经
    /// [`open_target`]）。软换行截断的长链接会被自动跨行拼起来（见 [`link_at`]），不会
    /// 因为正好卡在断行处就点出被截断的错误地址（#21）。
    fn url_at(&self, (r, c): (usize, usize)) -> Option<String> {
        let frame = self.last_frame.as_ref()?;
        link_at(frame, r, c).map(|(_, url)| url)
    }

    /// 单元处链接跨越的所有物理行区间 [(行, 起列, 止列), ...]，用于悬停高亮——软换行的
    /// 链接可能横跨好几行，每行各给一段列区间。
    fn link_range_at(&self, (r, c): (usize, usize)) -> Option<Vec<(usize, usize, usize)>> {
        let frame = self.last_frame.as_ref()?;
        link_at(frame, r, c).map(|(segs, _)| segs)
    }
}

/// 找到 `r` 所在的软换行逻辑行范围 `[first, last]`（物理行号，含端点）。
///
/// `frame.wrapped[i] == true` 表示第 i 行是被截断的，内容延续到第 i+1 行（终端没有
/// 真的输出换行符）。终端里打印的长链接经常正好卡在这种断点上——不把同一条软换行链
/// 串起来的话，行内正则扫描 / OSC 8 扩展都只能看到断点前或断点后的半截。
fn wrapped_line_range(frame: &terminal::Frame, r: usize) -> (usize, usize) {
    let mut first = r;
    while first > 0 && frame.wrapped.get(first - 1).copied().unwrap_or(false) {
        first -= 1;
    }
    let mut last = r;
    while frame.wrapped.get(last).copied().unwrap_or(false) && last + 1 < frame.rows.len() {
        last += 1;
    }
    (first, last)
}

/// 单元处的链接：命中的物理行区间列表 + 目标 URL/路径。
///
/// **先看 OSC 8**（`Cell::link`，终端协议层的链接）：`eza` / `gh` / `cargo` 这类输出里，
/// 可见文本往往只是标题、真正的 URL 藏在协议里，正则扫可见文本根本找不到。没有 OSC 8
/// 才回退到正则扫出来的 URL / 本地路径（[`find_links`]）。
///
/// 扫描前先把 `r` 所在的软换行逻辑行（[`wrapped_line_range`]）拼成一条缓冲区再整体找
/// 链接，最后把命中的缓冲区下标切回各物理行的列区间——否则打印的长链接卡在换行处就
/// 会被从中间切断，点出来的是被截断的错误地址（#21）。
type LinkRowRange = (usize, usize, usize);
type TerminalLink = (Vec<LinkRowRange>, String);

fn link_at(frame: &terminal::Frame, r: usize, c: usize) -> Option<TerminalLink> {
    let (first, last) = wrapped_line_range(frame, r);
    let mut buf: Vec<terminal::Cell> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(last - first + 2);
    for row_idx in first..=last {
        starts.push(buf.len());
        if let Some(row) = frame.rows.get(row_idx) {
            buf.extend(row.iter().cloned());
        }
    }
    starts.push(buf.len());

    let idx = starts.get(r - first).copied()? + c;
    if idx >= buf.len() {
        return None;
    }

    let (a, b, url) = if let Some(uri) = buf.get(idx).and_then(|cell| cell.link.clone()) {
        // 同一个链接铺在连续若干格上（可能跨行），向两侧扩到 uri 变化为止。
        let same = |i: usize| buf.get(i).and_then(|x| x.link.as_deref()) == Some(&*uri);
        let mut a = idx;
        while a > 0 && same(a - 1) {
            a -= 1;
        }
        let mut b = idx;
        while b + 1 < buf.len() && same(b + 1) {
            b += 1;
        }
        (a, b, uri.to_string())
    } else {
        let (a, b, url) = find_links(&buf)
            .into_iter()
            .find(|&(a, b, _)| idx >= a && idx <= b)?;
        (a, b, url)
    };

    // 把缓冲区下标区间 [a, b] 切回各物理行的列区间，供悬停高亮逐行绘制。
    let mut segs = Vec::new();
    for (i, row_idx) in (first..=last).enumerate() {
        let row_start = starts[i];
        let row_end = starts[i + 1];
        if row_end <= row_start {
            continue;
        }
        let lo = a.max(row_start);
        let hi = b.min(row_end - 1);
        if lo <= hi {
            segs.push((row_idx, lo - row_start, hi - row_start));
        }
    }
    Some((segs, url))
}

/// 输入法（IME）支持：中文等需要合成的输入走这里，最终提交的文字通过
/// replace_text_in_range 回调进来，写入 PTY。英文/可打印字符同样经此路径。
impl EntityInputHandler for TerminalView {
    /// 输入法拿这个接口取「文档里某一段文字」。我们的「文档」只有合成中的预编辑串
    /// （终端已提交的内容不属于可编辑文档），所以按 UTF-16 下标切片返回；越界就夹回
    /// 有效范围并通过 adjusted 告诉输入法。**不能不管问的是哪一段都把整串还回去**：
    /// 长度对不上会让输入法认为文档状态错乱，进而放弃合成、把拼音原文直接上屏。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.marked_text.as_ref()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = range_utf16.start.min(units.len());
        let end = range_utf16.end.clamp(start, units.len());
        if start != range_utf16.start || end != range_utf16.end {
            *adjusted = Some(start..end);
        }
        String::from_utf16(&units[start..end]).ok()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // 一直报 None → macOS 侧的 selectedRange 变成 {NSNotFound, 0}，等于告诉系统
        // 「这里没有文字光标」。切换输入法时那个提示气泡靠 selectedRange 判断当前
        // 焦点是否有效文字输入位置，一直是 NSNotFound 会导致它不出现（IME 候选窗本身
        // 走 hasMarkedText/setMarkedText，不受这个影响，所以合成打字不受影响）。这里
        // 汇报一个折叠的光标位置：合成中就在 marked_text 末尾，否则在 0。
        let len = self
            .marked_text
            .as_ref()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_text = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.send_input(text.as_bytes(), cx);
            self.clear_attention(cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_string())
        };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 候选窗要摆在光标格子上：从网格原点按 列×字宽 / 行×行高 偏移。
        let (row, col) = self.cursor.unwrap_or((0, 0));
        let origin = element_bounds.origin
            + point(
                px(PAD_X + col as f32 * self.cell_w),
                px(PAD_Y + row as f32 * line_px()),
            );
        Some(Bounds {
            origin,
            size: size(px(2.0), px(line_px())),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(message) = self.write_error.take() {
            crate::status_item::notify_error(message);
        }
        // 首帧把焦点抢到终端上。
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }

        // 正常由 PTY 事件处理先同步；这里兜底首帧/重连竞态。守护可能把尺寸所有权
        // 交给移动端，应用其 canonical grid 时不能把 resize 再回写给守护。
        self.terminal.sync_daemon_geometry();
        let remote_geometry_locked = self.terminal.remote_geometry_locked();
        // 远程（手机）拿走尺寸后，桌面不再一看见就抢回来：这一抢就是一次 SIGWINCH，
        // 不切备用屏的 CLI 会把整段对话重排重印一遍，而手机下次进来又要抢回去。改成
        // 用户真的点进这个终端（获得焦点）才发一次 claim，守护那边的宽限租约当场作废。
        let focus_claim =
            remote_geometry_locked && self.focus_handle.is_focused(window) && !self.was_focused;

        // 依据「本终端自身尺寸」重算行列（网格 Hub 里每个终端只占一格）。
        {
            let (w, h) = self.grid_size.get();
            // 精确测量等宽字符宽度（量一个 'M'）；异常时回退到 0.6 估算。
            // 必须用 terminal_font()（带 fallback 链）而非裸字体族：主字体没装时，
            // 裸字体和渲染各自 fallback 到不同字体，cell_w 就跟实际画出来的字宽脱节。
            let run = TextRun {
                len: 1,
                font: terminal_font(),
                color: hsla(0.0, 0.0, 1.0, 1.0),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let measured = f32::from(
                window
                    .text_system()
                    .layout_line("M", px(font_px()), &[run], None)
                    .width,
            );
            let cell_w = if measured > 1.0 {
                measured
            } else {
                font_px() * CELL_W_RATIO
            };
            self.cell_w = cell_w; // 供鼠标坐标换算
            // grid_size 未就绪（首帧为 0）时跳过 resize：保持 spawn 的默认 80 列，
            // 等 canvas 量到真实尺寸再调（避免 w=0 把终端缩成最小 4 列）。
            if w > 1.0 && h > 1.0 && (!remote_geometry_locked || focus_claim) {
                // 可用网格区 = 自身尺寸减去左右 / 上下各一份内边距。
                let cols = (((w - 2.0 * PAD_X) / cell_w).floor() as usize).clamp(4, 1000);
                let grid_rows = (((h - 2.0 * PAD_Y) / line_px()).floor() as usize).clamp(2, 1000);
                let cell_w_px = cell_w.round().clamp(1.0, 64.0) as u16;
                let cell_h_px = line_px().round().clamp(1.0, 128.0) as u16;
                if self.pty_kick_pending || focus_claim {
                    // 首帧 / reattach：无条件发 resize（含真实 cell 像素）。
                    // 守护 jolt 用 cell=0；普通 resize 同尺寸会早退——两处都补不到像素。
                    // focus claim 同理：本地 VT 已经跟着远程网格走了，不强发就拿不回来。
                    self.terminal
                        .force_resize(grid_rows, cols, cell_w_px, cell_h_px);
                    self.pty_kick_pending = false;
                } else {
                    self.terminal.resize(grid_rows, cols, cell_w_px, cell_h_px);
                }
            }
        }

        // 这一帧的网格：渲染和命中测试（url_at / link_range_at / char_steps_between）共用，
        // 见 last_frame 字段注释。
        let frame = Rc::new(self.terminal.snapshot());
        self.last_frame = Some(frame.clone());
        // 画反色块用可见光标（应用 CSI ?25l 藏光标时为 None）；IME 候选窗/预编辑
        // 定位、Option+点击移光标用**位置**（cursor_pos，含隐藏）——TUI 藏了光标
        // 输入法照样要知道往哪落。
        //
        // IME 合成中不画网格里的光标：预编辑串自带光标（画在拼音末尾，跟 iTerm2 一致）。
        let cursor = if self.marked_text.is_some() {
            None
        } else {
            frame.cursor
        };
        self.cursor = frame.cursor_pos;
        // 失焦的终端把光标画成空心框（见 paint_row）——多个终端并排时才看得出焦点在谁身上。
        let focused = self.focus_handle.is_focused(window);
        // 焦点变化上报给应用（DEC 1004；没开这个模式的应用收不到，见 report_focus）。
        if focused != self.was_focused {
            self.was_focused = focused;
            self.terminal.report_focus(focused);
            // 焦点变化只上报给应用。尺寸由上面的 grid_size 测量和 Terminal::resize
            // 判定；这里不能 force_resize，否则每次切回窗口都会额外触发 TIOCSWINSZ/SIGWINCH。
            // 唯一的例外是「远程留下的尺寸租约」：见下方 focus claim。
        }
        let hover_url = self.hover_url.clone();
        let has_hover = hover_url.is_some();
        // 滚动会改 display_offset：每帧按当前 offset 把绝对命中映到可视区。
        if self.search_open {
            // 后台搜索任务的结果落地后，本帧数据已更新；但结果到达本身可能没有
            // 其他事件触发 render，这里请求下一帧把新高亮画出来。
            if self.terminal.poll_search_results() {
                cx.notify();
            }
            self.search_hits = self.terminal.viewport_search_hits();
            self.search_status = self.terminal.search_status();
        }
        let search_hits = self.search_hits.clone();
        let search_status = self.search_status;
        let base_font = terminal_font();
        // 网格列宽：paint_row 用它把每一批文本钉到 col * cell_w 上（见 paint_row 头注）。
        let cell_w = self.cell_w;

        // IME 合成中的拼音预编辑串（marked text）：macOS 的分工是候选词浮窗由系统画
        // （bounds_for_range 只负责告诉它摆哪），**预编辑串由应用自己画**——不画的话
        // 打拼音就是盲打，只有候选窗没有输入回显。交给 paint_row 画在光标所在行的行内，
        // 光标已上滚离开可视区（cursor_pos 为 None）时自然不画。
        let ime = self.marked_text.clone().zip(frame.cursor_pos);
        let fh = self.focus_handle.clone();
        let entity = cx.entity();
        let origin_cell = self.grid_origin.clone();
        let size_cell = self.grid_size.clone();
        let size_cell_prepaint = size_cell.clone();
        let search_open = self.search_open;
        let search_input = self.search_input.clone();
        let scroll_info = self.terminal.scroll_info();

        // 背景层：底色（带透明度）+ 可选背景图，铺在终端内容之下。
        // 终端「默认底色」格子渲染时留空（见 render_row），故背景层能透出——所以这层
        // 的颜色必须跟 terminal::default_bg() 是同一个值。用户自选的底色已经并进
        // default_bg()（见 terminal::set_bg_override），OSC 11 应答和下发给手机的
        // 配色快照也都取它，四处同源。
        let ap = cx.global::<crate::Appearance>().clone();
        let bg_color = terminal::default_bg();
        // 终端是最高频的绘制区域。整窗透明度已由 NSWindow 统一处理，这里保持实底，
        // 避免每次终端输出都触发整块窗口的额外 alpha 混合。
        let mut bg_layer = div().absolute().inset_0().bg(rgb(bg_color));
        if let Some(path) = &ap.bg_image {
            bg_layer = bg_layer.child(crate::workspace_frame::background_image_layer(
                path,
                ap.bg_image_opacity,
            ));
        }
        // 整扇窗口的透明度由原生 NSWindow 统一处理，不能在这里重复叠加，
        // 否则终端背景会比侧栏/舞台额外变淡一次。

        div()
            .relative()
            .track_focus(&self.focus_handle)
            // 见 TerminalTab/TerminalBackTab 上的注释：让 Tab/Shift-Tab 在终端聚焦时
            // 归终端自己处理，别被 Root 的全局焦点跳转吃掉。
            .key_context("Terminal")
            .size_full()
            // 关键：裁剪溢出 + 允许收缩到 0，否则长行的 min-content 宽度会把
            // 容器越撑越宽，canvas 量到更大宽度 → 列数变多 → 行更长，形成放大循环。
            .overflow_hidden()
            .min_w_0()
            .min_h_0()
            .text_color(rgb(terminal::default_fg()))
            .font_family(font_family())
            .on_action(cx.listener(|this, _: &TerminalTab, _window, cx| {
                this.send_input(b"\t", cx);
                this.terminal.scroll_to_bottom();
                this.clear_attention(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TerminalBackTab, _window, cx| {
                this.send_input(b"\x1b[Z", cx); // xterm 反向 Tab（back-tab）序列
                this.terminal.scroll_to_bottom();
                this.clear_attention(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TerminalFind, window, cx| {
                this.open_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &TerminalFindNext, _window, cx| {
                if let Some(input) = &this.search_input {
                    let q = input.read(cx).value().to_string();
                    this.run_search(&q, false, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &TerminalFindPrev, _window, cx| {
                if let Some(input) = &this.search_input {
                    let q = input.read(cx).value().to_string();
                    this.run_search(&q, true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &TerminalFindClose, window, cx| {
                this.close_search(window, cx);
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                let m = &ks.modifiers;
                // 搜索条打开时：Esc 关闭；其它键留给输入框（不要灌进 PTY）。
                if this.search_open {
                    if ks.key == "escape" {
                        this.close_search(window, cx);
                    }
                    // 搜索框属于终端；不能再让按键冒泡给工作区的文件树 / Git 快捷键。
                    cx.stop_propagation();
                    return;
                }
                // IME 合成中：这些键归输入法（backspace 删拼音、enter/space/数字选词），
                // 不能再往 PTY 发一份，否则终端会当成真实按键吃掉。上屏的文字走
                // replace_text_in_range 进来。
                if this.marked_text.is_some() && !m.platform {
                    cx.stop_propagation();
                    return;
                }
                // Cmd+F 打开搜索（action 也会绑，这里兜底）。
                if m.platform && ks.key == "f" {
                    this.open_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                // Cmd+C 复制选区（alacritty 按缓冲区绝对行取文本，跨屏选区也完整）
                if m.platform && ks.key == "c" {
                    if let Some(text) = this.terminal.selection_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    cx.stop_propagation();
                    return;
                }
                // Cmd+V 粘贴：读剪贴板写入 PTY（bracketed paste / 换行规范化见 Terminal::paste）
                if m.platform && ks.key == "v" {
                    if let Some(text) = cx.read_from_clipboard().and_then(|it| it.text()) {
                        this.paste_text(&text, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                // Shift+PageUp/Down 滚动历史缓冲
                if m.shift && (ks.key == "pageup" || ks.key == "pagedown") {
                    let delta = if ks.key == "pageup" {
                        PAGE_LINES
                    } else {
                        -PAGE_LINES
                    };
                    this.terminal.scroll(delta);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if let Some(bytes) = keystroke_to_bytes(
                    ks,
                    this.terminal.app_cursor_mode(),
                    this.terminal.kitty_keyboard_mode(),
                ) {
                    this.send_input(&bytes, cx);
                    this.terminal.scroll_to_bottom(); // 敲键盘即回到最新输出，跟真实终端一致
                    this.clear_attention(cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, window, cx| {
                // 新的一次触控板手势开始时清空余数，避免上一次手势的残留跟这次叠加。
                if matches!(ev.touch_phase, TouchPhase::Started) {
                    this.scroll_accum = 0.0;
                }
                let delta_px = match ev.delta {
                    ScrollDelta::Lines(p) => p.y * line_px(),
                    ScrollDelta::Pixels(p) => f32::from(p.y),
                };
                this.scroll_accum += delta_px;
                let lines = (this.scroll_accum / line_px()).trunc();
                if lines != 0.0 {
                    this.scroll_accum -= lines * line_px();
                    // 按终端模式分流：TUI（Claude Code）转成鼠标滚轮事件，普通 shell 滚历史。
                    let (row, col) = this.pos_to_cell(ev.position, window);
                    this.terminal.scroll_wheel(lines as i32, row, col);
                    cx.notify();
                }
            }))
            // 鼠标框选
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    let cell = this.pos_to_cell(ev.position, window);
                    // Cmd+点击打开链接
                    if ev.modifiers.platform
                        && let Some(url) = this.url_at(cell) {
                            cx.open_url(&open_target(&url));
                            return;
                        }
                    // Option+点击：模拟 iTerm2/Terminal.app 的「点击移动光标」——只在
                    // 点击的正是光标所在那一行时才生效（shell 当前输入行），发对应数量
                    // 的左右方向键让 shell 的行编辑器（readline/zsh line editor）把光标
                    // 挪过去。终端本身没法直接把光标「传送」到任意格：光标位置由 shell
                    // 端的行编辑器状态决定，我们只能模拟按键让它自己移动。
                    if ev.modifiers.alt {
                        if let Some((cursor_row, cursor_col)) = this.cursor
                            && cell.0 == cursor_row && cell.1 != cursor_col {
                                let app_cursor = this.terminal.app_cursor_mode();
                                let step: &[u8] = if cell.1 > cursor_col {
                                    if app_cursor { b"\x1bOC" } else { b"\x1b[C" }
                                } else if app_cursor {
                                    b"\x1bOD"
                                } else {
                                    b"\x1b[D"
                                };
                                let count = this.char_steps_between(cell.0, cursor_col, cell.1);
                                let mut bytes = Vec::with_capacity(step.len() * count);
                                for _ in 0..count {
                                    bytes.extend_from_slice(step);
                                }
                                this.send_input(&bytes, cx);
                            }
                        return;
                    }
                    // 应用开了鼠标上报且没按 Shift → 把 press 转发给 TUI（vim/less/
                    // Claude 等靠这个点选）。Shift 旁路 = 强制本地框选（xterm 约定）。
                    // 双击/三击永远走本地选词/选行（应用鼠标协议没有语义选区）。
                    let app_wants_mouse =
                        this.terminal.mouse_mode() && !ev.modifiers.shift && ev.click_count <= 1;
                    if app_wants_mouse && this.terminal.mouse_button(0, true, cell.0, cell.1) {
                        this.app_mouse = true;
                        this.selecting = false;
                        this.terminal.selection_clear();
                        cx.notify();
                        return;
                    }
                    this.app_mouse = false;
                    let kind = match ev.click_count {
                        2 => terminal::SelectionKind::Word, // 双击选词（语义边界）
                        n if n >= 3 => terminal::SelectionKind::Line, // 三击选整行
                        _ => terminal::SelectionKind::Simple,
                    };
                    this.terminal.selection_start(
                        cell.0,
                        cell.1,
                        this.pos_in_left_half(ev.position),
                        kind,
                    );
                    this.selecting = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    let (row, col) = this.pos_to_cell(ev.position, window);
                    if this.app_mouse {
                        // TUI 拖选/拖动：左键 motion（button 32）。
                        this.terminal.mouse_drag(0, row, col);
                        return;
                    }
                    if this.selecting {
                        this.terminal.selection_update(
                            row,
                            col,
                            this.pos_in_left_half(ev.position),
                        );
                        // 拖出可视区上/下边缘 → 记方向并启动自动滚动（一边滚一边扩选）。
                        let (_, oy) = this.grid_origin.get();
                        let (_, h) = this.grid_size.get();
                        let y = f32::from(ev.position.y) - oy;
                        this.drag_scroll = if y < 0.0 {
                            1
                        } else if y > h - 2.0 * PAD_Y {
                            -1
                        } else {
                            0
                        };
                        this.drag_scroll_col = col;
                        if this.drag_scroll != 0 {
                            this.start_drag_scroll(cx);
                        }
                        cx.notify();
                    }
                } else {
                    let cell = this.pos_to_cell(ev.position, window);
                    // 全开 MOUSE_MOTION 时无键悬停也上报（button 35）
                    if this.terminal.mouse_mode() && !ev.modifiers.shift {
                        this.terminal.mouse_motion(cell.0, cell.1);
                    }
                    // 按住 Cmd 悬停链接：记录链接范围（用于高亮 + 手型）
                    let hl = if ev.modifiers.platform {
                        this.link_range_at(cell)
                    } else {
                        None
                    };
                    if hl != this.hover_url {
                        this.hover_url = hl;
                        cx.notify();
                    }
                }
            }))
            // 按/松 Cmd 时（鼠标不动也）即时更新链接高亮/手型
            .on_modifiers_changed(cx.listener(|this, ev: &ModifiersChangedEvent, window, cx| {
                let hl = if ev.modifiers.platform {
                    this.link_range_at(this.pos_to_cell(window.mouse_position(), window))
                } else {
                    None
                };
                if hl != this.hover_url {
                    this.hover_url = hl;
                    cx.notify();
                }
            }))
            // 中键：MOUSE_MODE 时转发给应用；否则粘贴剪贴板（X11 风格，macOS 触控板少见）
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    let cell = this.pos_to_cell(ev.position, window);
                    if !ev.modifiers.shift && this.terminal.mouse_button(1, true, cell.0, cell.1) {
                        return;
                    }
                    if let Some(text) = cx.read_from_clipboard().and_then(|it| it.text()) {
                        this.paste_text(&text, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseUpEvent, window, _cx| {
                    if !ev.modifiers.shift {
                        let cell = this.pos_to_cell(ev.position, window);
                        this.terminal.mouse_button(1, false, cell.0, cell.1);
                    }
                }),
            )
            // 右键：TUI 开了鼠标上报时转发给应用（Shift 旁路 → 系统菜单）；
            // 否则不转发，交给 context_menu（新建任务等）。
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    if ev.modifiers.shift {
                        return;
                    }
                    if this.terminal.mouse_mode() {
                        let cell = this.pos_to_cell(ev.position, window);
                        this.terminal.mouse_button(2, true, cell.0, cell.1);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseUpEvent, window, _cx| {
                    if ev.modifiers.shift {
                        return;
                    }
                    if this.terminal.mouse_mode() {
                        let cell = this.pos_to_cell(ev.position, window);
                        this.terminal.mouse_button(2, false, cell.0, cell.1);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    this.drag_scroll = 0;
                    // 应用鼠标路径：补发 release，不再碰本地选区。
                    if this.app_mouse {
                        this.app_mouse = false;
                        this.selecting = false;
                        let cell = this.pos_to_cell(ev.position, window);
                        this.terminal.mouse_button(0, false, cell.0, cell.1);
                        cx.notify();
                        return;
                    }
                    this.selecting = false;
                    // 真的拖出了非空选区：选中即复制（iTerm2 copy-on-select）。TUI 重绘
                    // 会清掉 alacritty 选区，松手瞬间进剪贴板才稳。
                    if let Some(text) = this.terminal.selection_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        return;
                    }
                    // 未拖动的本地单击：若应用开了鼠标但 mousedown 没接管（比如当时
                    // 按着 Shift，现在松了），仍可在 mouseup 发一次 click 脉冲——但
                    // 当前若 mouse_mode 且没 shift，mousedown 已经走 app 路径了。
                    // 这里只清空选区。
                    this.terminal.selection_clear();
                    cx.notify();
                }),
            )
            // 背景层（最底）：底色 / 背景图 / 透明度
            .child(bg_layer)
            // 终端主体：逐行画 alacritty 网格快照（底色 / 文字 / 光标 / 选区 / IME）。
            //
            // 走 canvas 直接 paint、而不是「每行一个 div + StyledText」：网格对齐要靠
            // `shape_line(force_width = cell_w)`（见 paint_row 头注），而 StyledText 走的是
            // shape_text，压根没有这个参数。顺带也省掉了每行每批一个元素的布局开销。
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        let rows = &frame.rows;
                        // 网格原点吸到设备像素上（照 Zed：terminal_element.rs:1062，它的注释说
                        // 分数原点会让字形在帧与帧之间抖，看着像闪烁）。分屏时布局给的 bounds
                        // 很容易落在半个像素上。
                        let scale = window.scale_factor();
                        let snap = |v: Pixels| px((f32::from(v) * scale).floor() / scale);
                        let ox = snap(bounds.origin.x + px(PAD_X));
                        let oy = snap(bounds.origin.y + px(PAD_Y));
                        for (r, row) in rows.iter().enumerate() {
                            let cur = match cursor {
                                Some((cr, cc, kind)) if cr == r => Some((cc, kind)),
                                _ => None,
                            };
                            let hl = hover_url
                                .as_ref()
                                .and_then(|segs| segs.iter().find(|&&(hr, _, _)| hr == r))
                                .map(|&(_, a, b)| (a, b));
                            // 同一行可能有多段命中；传整行命中列表给 paint_row。
                            let row_hits: Vec<(usize, usize, bool)> = search_hits
                                .iter()
                                .filter(|h| h.row == r)
                                .map(|h| (h.col_start, h.col_end, h.active))
                                .collect();
                            let ime_here = match &ime {
                                Some((text, (ir, ic))) if *ir == r => Some((text.as_str(), *ic)),
                                _ => None,
                            };
                            let origin = point(ox, oy + px(r as f32 * line_px()));
                            paint_row(
                                row,
                                PaintRowParams {
                                    origin,
                                    cursor: cur,
                                    focused,
                                    base_font: &base_font,
                                    hover_link: hl,
                                    search_hits: &row_hits,
                                    cell_w,
                                    ime: ime_here,
                                },
                                window,
                                cx,
                            );
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            // 透明覆盖层：paint 阶段注册 IME 输入处理器，并记录网格原点。
            .child(
                canvas(
                    // prepaint：建一个覆盖终端区的 hitbox（供设置鼠标样式用），并在
                    // 首次拿到真实布局尺寸后请求下一帧。尺寸是在这一阶段才可用的；
                    // 若首帧终端输出已经触发过重绘，下一次 render 可能永远不来，
                    // PTY 就会一直停在默认网格，直到后续重绘操作碰巧触发它。
                    move |bounds, window, _cx| {
                        let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        if size_cell_prepaint.get() != size {
                            size_cell_prepaint.set(size);
                            window.request_animation_frame();
                        }
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |bounds, hitbox, window, cx| {
                        // 鼠标样式：悬停链接时手型，否则文本 I-beam
                        window.set_cursor_style(
                            if has_hover {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::IBeam
                            },
                            &hitbox,
                        );
                        // 网格原点 = 覆盖层原点 + 内边距（终端主体带内边距，坐标相应右下偏移）
                        origin_cell.set((
                            f32::from(bounds.origin.x) + PAD_X,
                            f32::from(bounds.origin.y) + PAD_Y,
                        ));
                        // 记录自身尺寸，供按卡片大小算行列
                        size_cell
                            .set((f32::from(bounds.size.width), f32::from(bounds.size.height)));
                        window.handle_input(&fh, ElementInputHandler::new(bounds, entity), cx);
                    },
                )
                .absolute()
                .inset_0(),
            )
            // 搜索条：叠在顶部，不抢网格布局（absolute）
            .when(search_open, |root| {
                let status_label = if search_status.total == 0 {
                    "无结果".to_string()
                } else {
                    format!("{}/{}", search_status.current, search_status.total)
                };
                let bar = div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .w(px(300.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    // 底色跟随全局色板（写死的深蓝在换色板后会突兀）。
                    .bg(rgb(crate::ui_theme::bg_card()))
                    .border_1()
                    .border_color(rgb(crate::ui_theme::border_mid()))
                    .shadow_md()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(if let Some(input) = search_input {
                                Input::new(&input).cleanable(true).into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(if terminal::is_dark() {
                                0x0080_8a9a
                            } else {
                                0x0057_6069
                            }))
                            .child(status_label),
                    );
                root.child(bar)
            })
            // 滚动条：有 scrollback 时画在右侧；拖 thumb / 点轨道跳转。
            .when(scroll_info.max_offset > 0, |root| {
                root.child(self.render_scrollbar(scroll_info, cx))
            })
    }
}

/// 滚动条轨道宽度（像素）。
const SCROLLBAR_W: f32 = 9.0;
/// thumb 最短高度，太短不好点。
const SCROLLBAR_THUMB_MIN: f32 = 28.0;

/// 滚动条 thumb 几何：返回 (thumb 高度, thumb 顶部 y)。
/// offset=0 → thumb 在底部；offset=max → 顶部（跟 alacritty display_offset 一致）。
fn scrollbar_thumb(
    track_h: f32,
    viewport_rows: usize,
    max_offset: usize,
    offset: usize,
) -> (f32, f32) {
    let total = viewport_rows.saturating_add(max_offset).max(1);
    // 首帧 grid_size 未量出来时轨道可能比 THUMB_MIN 还矮，min 必须让位，
    // 否则 clamp 遇到 min > max 直接 panic。
    let thumb_min = SCROLLBAR_THUMB_MIN.min(track_h);
    let thumb_h = ((viewport_rows as f32 / total as f32) * track_h).clamp(thumb_min, track_h);
    let travel = (track_h - thumb_h).max(0.0);
    let thumb_y = if max_offset == 0 {
        0.0
    } else {
        travel * (1.0 - offset as f32 / max_offset as f32)
    };
    (thumb_h, thumb_y)
}

impl TerminalView {
    /// 右侧滚动条：offset=0 贴底，offset=max 贴顶（跟 alacritty display_offset 一致）。
    fn render_scrollbar(
        &self,
        info: terminal::ScrollInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (_, h) = self.grid_size.get();
        let track_h = (h - 2.0 * PAD_Y).max(1.0);
        let (thumb_h, thumb_y) =
            scrollbar_thumb(track_h, info.viewport_rows, info.max_offset, info.offset);
        // 中性灰，不要焦点蓝。终端底比侧栏更深，border_loud 会融进去，用 text_faint 才能看见。
        let thumb_color = rgb(crate::ui_theme::text_faint());
        let max_off = info.max_offset;
        let viewport = info.viewport_rows;

        div()
            .id("term-scrollbar")
            .absolute()
            .top(px(PAD_Y))
            .right(px(2.0))
            .w(px(SCROLLBAR_W))
            .h(px(track_h))
            .rounded_full()
            .bg(if terminal::is_dark() {
                rgba(0x0000_2c31_4955)
            } else {
                rgba(0x00_d0_d7_de_66)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    let (_, oy) = this.grid_origin.get();
                    let y = (f32::from(ev.position.y) - oy).clamp(0.0, track_h);
                    // 点在 thumb 上：开始拖；点在轨道：跳到对应位置
                    if y >= thumb_y && y <= thumb_y + thumb_h {
                        this.scrollbar_drag = Some(y - thumb_y);
                    } else {
                        this.scrollbar_drag = None;
                        this.jump_scrollbar_to(y, track_h, thumb_h, max_off, cx);
                    }
                    window.prevent_default();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _window, cx| {
                if let Some(grab) = this.scrollbar_drag
                    && ev.pressed_button == Some(MouseButton::Left)
                {
                    let (_, oy) = this.grid_origin.get();
                    let y = (f32::from(ev.position.y) - oy - grab).clamp(0.0, track_h - thumb_h);
                    this.jump_scrollbar_to(y + thumb_h * 0.5, track_h, thumb_h, max_off, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.scrollbar_drag = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(thumb_y))
                    .left(px(1.0))
                    .w(px(SCROLLBAR_W - 2.0))
                    .h(px(thumb_h))
                    .rounded_full()
                    .bg(thumb_color)
                    // 占位避免编译器抱怨 viewport 未用
                    .when(viewport == 0, |d| d),
            )
    }

    /// 根据轨道上的 y（相对网格顶）设置 display_offset。
    fn jump_scrollbar_to(
        &mut self,
        y: f32,
        track_h: f32,
        thumb_h: f32,
        max_off: usize,
        cx: &mut Context<Self>,
    ) {
        if max_off == 0 || track_h <= thumb_h {
            self.terminal.set_scroll_offset(0);
        } else {
            // thumb 中心位置 → 0..=1，再反转到 offset（底=0）
            let center = y.clamp(thumb_h * 0.5, track_h - thumb_h * 0.5);
            let t = (center - thumb_h * 0.5) / (track_h - thumb_h);
            let offset = ((1.0 - t) * max_off as f32).round() as usize;
            self.terminal.set_scroll_offset(offset.min(max_off));
        }
        self.refresh_search_highlights();
        cx.notify();
    }
}

/// 画一行。**终端是网格，第 N 列就必须画在 N × cell_w**——字体的字形宽度只决定「字长
/// 什么样」，不决定「它在哪」。
///
/// 这一点靠 `shape_line(.., force_width = Some(cell_w))` 落实（跟 Zed 终端同一手法）：
/// GPUI 的 `apply_force_width_to_layout` 会把每个 glyph 的 x **强制钉到 `序号 × cell_w`**，
/// 字体自己的 advance 直接作废。于是批内位置也跟字宽彻底脱钩——中文 fallback 到 PingFang
/// 也好，`·` `—` 这类「东亚歧义宽度」字符（终端只给一格、字体却画成全角）也好，都只是
/// 自己画宽一点、覆盖到邻格上，**推不动后面任何一个字符**。
///
/// 这正是 div + StyledText 做不到的：`StyledText` 走 `shape_text`，只有 wrap_width、没有
/// force_width，批内只能交给排版器按字体 advance 自由流——`——正` 这种批里，两个破折号
/// 会把「正」一路顶右，撞到下一批钉在网格列上的字上（用户可见：中文叠字）。
///
/// 唯一还须守住的不变量：**宽字符必须落在批尾**。force_width 是按 glyph 序号钉位的，而
/// 宽字符占两格却只算一个 glyph，它后面若还有同批字符就会整体少一格。好在宽字符后面必
/// 跟 '\0' 占位格，使下一个字符列号对不上而断批（见 [`text_batches`] 及其测试）。
///
/// `ime`（预编辑串, 起始列）不为空时在行内叠一层：垫终端底色遮住底下的内容、下划线标示
/// 「合成中」，光标跟在拼音末尾（合成中网格里的光标块不画，见 render 里 cursor 的取值）。
struct PaintRowParams<'a> {
    origin: Point<Pixels>,
    cursor: Option<(usize, terminal::CursorKind)>,
    focused: bool,
    base_font: &'a Font,
    hover_link: Option<(usize, usize)>,
    /// 本行搜索命中：(起列, 止列含, 是否当前 active)。
    search_hits: &'a [(usize, usize, bool)],
    cell_w: f32,
    /// 预编辑串与起始列（来自 cursor_pos，含被 TUI 隐藏的光标）。
    ime: Option<(&'a str, usize)>,
}

fn paint_row(
    row: &[terminal::Cell],
    params: PaintRowParams<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    let PaintRowParams {
        origin,
        cursor,
        focused,
        base_font,
        hover_link,
        search_hits,
        cell_w,
        ime,
    } = params;
    // 失焦时一律画成空心框（跟 iTerm2 / Zed 一致，terminal_element.rs:1250）——驾驶舱里
    // 多个终端并排，每个都亮着一模一样的实心块的话，根本看不出焦点在谁身上。
    let cursor = cursor.map(|(col, kind)| {
        let kind = if focused {
            kind
        } else {
            terminal::CursorKind::Hollow
        };
        (col, kind)
    });
    // 只有实心块要把底下的字反色（底色由 bg_spans 画、字色由 style_of 换）。竖线 / 下划线 /
    // 空心框都不动文字，光标本身作为一个 quad 画在文字之上。
    let block_at = match cursor {
        Some((col, terminal::CursorKind::Block)) => Some(col),
        _ => None,
    };
    // 光标压在宽字符（中文/emoji）上时要盖满**两格**：第二格是 '\0' 占位，样式得跟着一起换，
    // 否则实心块只有半格（Zed 走的是 shaped_width.max(cell_width)，我们直接看占位格）。
    let is_wide_at = |col: usize| row.get(col + 1).is_some_and(|c| c.ch == '\0');
    let in_block = |i: usize| match block_at {
        Some(col) => i == col || (i == col + 1 && is_wide_at(col)),
        None => false,
    };

    let is_link = |i: usize| hover_link.is_some_and(|(a, b)| i >= a && i <= b);
    // 返回 (是否命中, 是否 active)。active 画得更亮。
    let search_at = |i: usize| -> Option<bool> {
        search_hits
            .iter()
            .find(|(a, b, _)| i >= *a && i <= *b)
            .map(|(_, _, active)| *active)
    };
    // 悬停链接：高亮色 + 下划线；再叠加光标反色 / 选区 / 搜索命中背景。
    let style_of = |i: usize| -> CellStyle {
        let c = &row[i];
        let mut fg = c.fg;
        // None = 默认底色（不画，让背景层透出），见 CellStyle::bg。
        let mut bg = (!c.bg_default).then_some(c.bg);
        // OSC 8 链接常驻下划线（跟 Zed 一致：terminal_element.rs:623 把 hyperlink 也算进
        // underline）——不然可见文本只是普通标题，用户根本看不出这里有链接可点。
        let mut underline = c.underline || c.link.is_some();
        if is_link(i) {
            fg = link_fg();
            underline = true;
        }
        if in_block(i) {
            // 光标实心块：底色取前景色，字色取原底色（默认底色时就是终端底色）。
            let under = bg.unwrap_or_else(terminal::default_bg);
            bg = Some(fg);
            fg = under;
        } else if c.selected {
            bg = Some(sel_bg());
        } else if let Some(active) = search_at(i) {
            bg = Some(search_hit_bg(active));
        }
        CellStyle {
            fg,
            bg,
            bold: c.bold,
            italic: c.italic,
            dim: c.dim,
            underline,
            undercurl: c.undercurl,
            strikeout: c.strikeout,
        }
    };

    let h = px(line_px());
    let at = |col: usize| point(origin.x + px(col as f32 * cell_w), origin.y);
    let run_of = |text: &str, st: CellStyle| {
        let mut font = base_font.clone();
        if st.bold {
            font.weight = FontWeight::BOLD;
        }
        if st.italic {
            font.style = FontStyle::Italic;
        }
        let mut color = Hsla::from(rgb(st.fg));
        if st.dim {
            // faint(SGR 2)：深色维持原层级；浅色底本来就会冲淡字色，少压一点
            // alpha，避免 TUI 的辅助信息淡到难以辨认。
            color.a *= if terminal::is_dark() { 0.7 } else { 0.8 };
        }
        TextRun {
            len: text.len(),
            font,
            color,
            background_color: None, // 底色走 quad，见 bg_spans
            underline: st.underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: st.undercurl,
            }),
            strikethrough: st.strikeout.then(|| StrikethroughStyle {
                thickness: px(1.0),
                color: Some(color),
            }),
        }
    };
    let shape = |text: String, run: TextRun, window: &mut Window| {
        window.text_system().shape_line(
            text.into(),
            px(font_px()),
            std::slice::from_ref(&run),
            Some(px(cell_w)), // ← 网格定位的关键，见函数头注
        )
    };

    // 底色**扫整行**（bg_spans 内部按 row.len() 走，没有截断参数可传错）。
    // 起点向下取整、宽度向上取整（照 Zed 的 LayoutRect::paint）：相邻两块不同底色的矩形
    // 若落在半个像素上，中间会透出一条缝，背景图 / 半透明底下尤其显眼。
    for (col, span, bg) in bg_spans(row, &style_of) {
        let x0 = at(col).x.floor();
        let x1 = (at(col).x + px(span as f32 * cell_w)).ceil();
        window.paint_quad(fill(
            Bounds::new(point(x0, origin.y), size(x1 - x0, h)),
            rgb(bg),
        ));
    }

    // 字形只画到最后一个「非 blank」，尾部那些什么都不画的空格不必进批次。
    // 实心块光标压在尾部空格上时，那格要画（反色后的空格底色已由 bg_spans 铺好，这里是为了
    // 让批次覆盖到它——真正要紧的是块下若有字符，得用反色重画一遍）。
    let mut end = visible_end(row, &is_link);
    if let Some(col) = block_at {
        end = end.max((col + 1).min(row.len()));
    }
    for b in text_batches(row, end, &style_of) {
        let run = run_of(&b.text, b.style);
        if b.wide {
            // 宽字符：字形（约 1.0em）比两格（1.2em）窄，左对齐会让它贴着格子左边——光标块
            // （满两格）压上去时左右空隙就不对称，整行中文看着也都偏左。这里**不加**
            // force_width 地 shape 一次拿到真实字形宽度，再居中放进两格里。
            // （force_width 只会把 glyph 钉到 `序号 × cell_w`，做不了居中；宽字符为此独占
            // 一批，见 text_batches。）
            let shaped = window.text_system().shape_line(
                b.text.into(),
                px(font_px()),
                std::slice::from_ref(&run),
                None,
            );
            let slack = 2.0 * cell_w - f32::from(shaped.width);
            let x = at(b.col).x + px(slack.max(0.0) / 2.0);
            let _ = shaped.paint(point(x, origin.y), h, TextAlign::Left, None, window, cx);
        } else {
            let shaped = shape(b.text, run, window);
            let _ = shaped.paint(at(b.col), h, TextAlign::Left, None, window, cx);
        }
    }

    // 非实心块的光标形状：画在文字之上。宽度按宽字符占几格算。
    if let Some((col, kind)) = cursor {
        let fg = rgb(terminal::default_fg());
        let w = px(if is_wide_at(col) {
            2.0 * cell_w
        } else {
            cell_w
        });
        let bounds = Bounds::new(at(col), size(w, h));
        match kind {
            // 实心块已经靠 bg_spans + 反色字画好了
            terminal::CursorKind::Block => {}
            terminal::CursorKind::Hollow => {
                window.paint_quad(outline(bounds, fg, BorderStyle::Solid));
            }
            terminal::CursorKind::Bar => {
                window.paint_quad(fill(Bounds::new(at(col), size(px(2.0), h)), fg));
            }
            terminal::CursorKind::Underline => {
                let y = origin.y + h - px(2.0);
                window.paint_quad(fill(Bounds::new(point(at(col).x, y), size(w, px(2.0))), fg));
            }
        }
    }

    if let Some((text, col)) = ime {
        let fg = terminal::default_fg();
        // 预编辑串：终端默认前景 + 下划线标示「合成中」。
        let run = run_of(
            text,
            CellStyle {
                fg,
                underline: true,
                ..CellStyle::default()
            },
        );
        let shaped = shape(text.to_string(), run, window);
        let w = shaped.width;
        // 先垫底色盖住底下的终端内容，再画拼音，最后把光标接在末尾。
        window.paint_quad(fill(
            Bounds::new(at(col), size(w + px(cell_w), h)),
            rgb(terminal::default_bg()),
        ));
        let _ = shaped.paint(at(col), h, TextAlign::Left, None, window, cx);
        let cursor_at = point(at(col).x + w, origin.y);
        window.paint_quad(fill(Bounds::new(cursor_at, size(px(cell_w), h)), rgb(fg)));
    }
}

/// 一格的最终样式（cell 自带的属性 + 光标反色 / 选区 / 悬停链接叠加之后）。
/// 同样式且列号连续的格子会连成一批，见 [`text_batches`]。
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    fg: u32,
    /// 底色。**None = 终端默认底色**，这种格子不画底色矩形，让底下的背景层 / 背景图 /
    /// 桌面透出来。用 Option 而不是「跟 default_bg() 比 RGB」：应用可以显式设一个恰好
    /// 等于默认底色的 RGB，那是真要画的一块底色（见 `Cell::bg_default`）。
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    dim: bool,
    underline: bool,
    undercurl: bool,
    strikeout: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            fg: terminal::default_fg(),
            bg: None,
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            undercurl: false,
            strikeout: false,
        }
    }
}

/// 一行里要画底色的列区间：(起始列, 占几格, 颜色)。默认底色不产出，让底下的背景层 /
/// 背景图 / 桌面透出。
///
/// **必须扫到行尾，不能跟着字形一起截断在最后一个可见字符**——终端每行都补空格到满列宽，
/// 而「拿空格承载底色」是常规操作：fzf 的选中行、tmux/vim 的状态栏、TUI 菜单的选中项、
/// 多行拖选的中间行，尾巴上全是「带底色的空格」。截断的话高亮就缺一截（多行选区看着像
/// 锯齿），整行彩色空格的状态条更是一个像素都画不出来。所以这里只吃 `row`、不收 end 参数，
/// 没有截断可传错。Zed 同样在 `is_blank` 判定**之前**无条件收底色（terminal_element.rs:407）。
///
/// 底色走列区间而不是靠文字的 background_color：后者只覆盖字形的实际宽度，中文字形窄于两格
/// 时选区高亮会露出缝隙，且宽字符的 '\0' 占位格根本没有字符去承载底色。
fn bg_spans(
    row: &[terminal::Cell],
    style_of: &dyn Fn(usize) -> CellStyle,
) -> Vec<(usize, usize, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let bg = style_of(i).bg;
        let start = i;
        while i < row.len() && style_of(i).bg == bg {
            i += 1;
        }
        // None = 默认底色：不画，让背景层 / 背景图 / 桌面透出。
        if let Some(bg) = bg {
            out.push((start, i - start, bg));
        }
    }
    out
}

/// 字形要画到第几列为止：最后一个「非 blank」的下一列。
///
/// blank = 空格（或宽字符的 '\0' 占位）且没有任何要画的装饰。**底色不在判据里**——底色由
/// [`bg_spans`] 独立扫全行负责，这里只关心「有没有字形 / 下划线 / 删除线要画」。带下划线的
/// 空格（含悬停链接）是要画线的，不算 blank。对应 Zed 的 `is_blank`（terminal_element.rs:1630，
/// 它的 `has_visible_style_modifier` = `ALL_UNDERLINES | INVERSE | STRIKEOUT`；INVERSE 在我们这
/// 边 snapshot 时就换成非默认底色了，由 bg_spans 兜住）。
fn visible_end(row: &[terminal::Cell], is_link: &dyn Fn(usize) -> bool) -> usize {
    let blank = |i: usize| {
        let c = &row[i];
        (c.ch == ' ' || c.ch == '\0')
            && !c.underline
            && !c.strikeout
            && c.link.is_none() // OSC 8 铺在空格上时也要画下划线
            && !is_link(i)
    };
    (0..row.len()).rposition(|i| !blank(i)).map_or(0, |i| i + 1)
}

/// 一批文本：钉在 `起始列 × cell_w` 上绘制。`wide` = 这批是**单个宽字符**（中文 / emoji）。
struct Batch {
    col: usize,
    text: String,
    style: CellStyle,
    wide: bool,
}

/// 把一行切成若干「批」。
///
/// 续接一批的条件：**样式相同，且这一格紧接着本批已占的格子**。
///
/// **宽字符（占两格的）独占一批**：它的字形（约 1.0em）窄于两格（1.2em），得按真实字形宽度
/// 在两格内**居中**才好看——左对齐的话字会贴着格子左边，光标块一压上去左右空隙就不对称
/// （见 [`paint_row`] 里的居中绘制）。而 force_width 只会把 glyph 钉到 `序号 × cell_w`、
/// 一律左对齐，所以宽字符不能跟别人混在一批里，否则没法单独定位。
///
/// 零宽字符（变体选择器 / 组合变音符，见 `Cell::zw`）紧跟基字符进同一批，但 `count`
/// **不加**——`count` 记的是**格数**不是字符数。加了的话后面每个字符的列号都会错一格。
/// gpui 的 `apply_force_width_to_layout` 认得它们（排在基字符同一个 x，不推进 glyph 计数器，
/// 贴着基字符走），所以位置不受影响。跟 Zed 的 `append_zero_width_chars` 是一回事。
fn text_batches(
    row: &[terminal::Cell],
    end: usize,
    style_of: &dyn Fn(usize) -> CellStyle,
) -> Vec<Batch> {
    let mut out: Vec<Batch> = Vec::new();
    // (起始列, 已占格数, 文本, 样式)
    let mut cur: Option<(usize, usize, String, CellStyle)> = None;
    let flush = |cur: &mut Option<(usize, usize, String, CellStyle)>, out: &mut Vec<Batch>| {
        if let Some((col, _, text, style)) = cur.take() {
            out.push(Batch {
                col,
                text,
                style,
                wide: false,
            });
        }
    };
    for i in 0..end.min(row.len()) {
        let cell = &row[i];
        let ch = cell.ch;
        if ch == '\0' {
            continue; // 宽字符占位格：不产生字形，但列号照常前进
        }
        let style = style_of(i);
        let zw = cell.zw.as_deref().unwrap_or_default();
        let mut text = String::from(ch);
        text.extend(zw); // 零宽字符：进文本，不占格

        // 宽字符（后面跟着 '\0' 占位格）：独占一批，绘制时按真实字形宽度居中到两格里。
        if row.get(i + 1).is_some_and(|c| c.ch == '\0') {
            flush(&mut cur, &mut out);
            out.push(Batch {
                col: i,
                text,
                style,
                wide: true,
            });
            continue;
        }

        match cur.as_mut() {
            Some((start, count, buf, st)) if *st == style && *start + *count == i => {
                buf.push_str(&text);
                *count += 1;
            }
            _ => {
                flush(&mut cur, &mut out);
                cur = Some((i, 1, text, style));
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// 在一行里找出所有 URL，返回 (起列, 止列含, url)。
fn find_urls(row: &[terminal::Cell]) -> Vec<(usize, usize, String)> {
    let n = row.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if starts_scheme(row, i) {
            let mut j = i;
            while j < n && is_url_char(row[j].ch) {
                j += 1;
            }
            // 去掉结尾的标点（跳过宽字符占位格再判）
            let mut end = j;
            while end > i {
                let ch = row[end - 1].ch;
                if ch == '\0' {
                    end -= 1;
                    continue;
                }
                if matches!(
                    ch,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
                ) {
                    end -= 1;
                    continue;
                }
                break;
            }
            if end > i {
                let url = cells_to_token(row, i, end);
                // 最短合法 URL 大约 "http://a.b"（10 字符级）；滤掉误扫到的短前缀
                if url.len() >= 10 {
                    out.push((i, end - 1, url));
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// 合并 URL + 本地文件路径的可点链接，供 [`TerminalView::url_at`]/[`TerminalView::link_range_at`] 共用。
fn find_links(row: &[terminal::Cell]) -> Vec<(usize, usize, String)> {
    let mut out = find_urls(row);
    out.extend(find_paths(row));
    out
}

/// 在一行里找出所有本地文件路径（绝对路径 / `~/` 开头），返回 (起列, 止列含, 展开后的
/// 绝对路径)。跟 URL 一样按「连续非空白 token」扫描，但额外要求磁盘上真实存在——否则
/// 随便一段带斜杠的文本（命令参数、注释里的 a/b/c）都会被当成可点链接，误判太多。
fn find_paths(row: &[terminal::Cell]) -> Vec<(usize, usize, String)> {
    let n = row.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let starts = row[i].ch == '/' || (row[i].ch == '~' && i + 1 < n && row[i + 1].ch == '/');
        if starts {
            let mut j = i;
            while j < n && is_url_char(row[j].ch) {
                j += 1;
            }
            let mut end = j;
            while end > i {
                let ch = row[end - 1].ch;
                if ch == '\0' {
                    end -= 1;
                    continue;
                }
                if matches!(
                    ch,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
                ) {
                    end -= 1;
                    continue;
                }
                break;
            }
            if end > i {
                // 宽字符第二格是 `'\0'` 占位——拼 token 时必须跳过，否则带中文的路径会夹
                // NUL，`Path::exists` 永远失败。扫描时仍把 `'\0'` 当 token 内字符（见
                // is_url_char），这样 `/Users/中文/x` 不会在「中」后面被截断。
                let token = cells_to_token(row, i, end);
                if let Some(path) = expand_existing_path(&token) {
                    out.push((i, end - 1, path));
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// 把 `[start, end)` 列上的可见字符拼成字符串：跳过宽字符占位 `'\0'`，并带上基字符
/// 上的零宽字符（变体选择器等）。列范围本身仍含占位格，悬停高亮才能盖满两格。
fn cells_to_token(row: &[terminal::Cell], start: usize, end: usize) -> String {
    let mut s = String::new();
    for c in row.iter().take(end.min(row.len())).skip(start) {
        if c.ch == '\0' {
            continue;
        }
        s.push(c.ch);
        if let Some(zw) = c.zw.as_deref() {
            s.extend(zw.iter().copied());
        }
    }
    s
}

/// `~` 展开成 home 目录，并确认路径在磁盘上真实存在（文件或目录）；不存在则不认为
/// 是可点路径，避免误判。
fn expand_existing_path(token: &str) -> Option<String> {
    let expanded = match token.strip_prefix('~') {
        Some(rest) => dirs::home_dir()?
            .join(rest.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned(),
        None => token.to_string(),
    };
    std::path::Path::new(&expanded).exists().then_some(expanded)
}

/// 把 [`TerminalView::url_at`] 返回的目标转成 `cx.open_url` 能吃的字符串：http(s)
/// 链接原样返回；本地路径转成 `file://` URL 并 percent-encode 每个非常规字节——
/// `NSURL::initWithString:` 对未编码的 UTF-8（中文路径）很挑剔，不编码直接建不出 NSURL。
fn open_target(target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    let mut out = String::from("file://");
    for b in target.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 判断第 i 列起是否是 http:// 或 https://。
fn starts_scheme(row: &[terminal::Cell], i: usize) -> bool {
    let at = |pat: &str| {
        let pc: Vec<char> = pat.chars().collect();
        i + pc.len() <= row.len() && (0..pc.len()).all(|k| row[i + k].ch == pc[k])
    };
    at("http://") || at("https://")
}

fn is_url_char(c: char) -> bool {
    // `'\0'` = 宽字符占位格：扫描 token 时要当成「继续」而不是断点，否则
    // `/Users/中文/x` 会在「中」后被截断。真正拼字符串时再跳过（见 cells_to_token）。
    c == '\0' || (!c.is_whitespace() && !matches!(c, '<' | '>' | '"' | '`' | '|' | '{' | '}' | '^'))
}

/// 把一次「非文本按键」转成写给 PTY 的字节：特殊键和 Ctrl 组合。
/// 可打印字符与空格走 IME 的 replace_text_in_range，不在这里处理。
///
/// 键表大体对齐 Zed `mappings/keys.rs`（xterm PC-style function keys）：
/// - 裸方向键 / Home / End 尊重 DECCKM（app_cursor → SS3）
/// - 带修饰的方向键 / F 键 / Page / Home / End → CSI `1;{mod}` 或 `N;{mod}~`
/// - Enter：开了 kitty 消歧层时带修饰走 CSI u；否则遗留编码
///
/// `app_cursor`：DECCKM（见 Terminal::app_cursor_mode）。
/// `kitty_keys`：kitty keyboard DISAMBIGUATE 层（见 Terminal::kitty_keyboard_mode）。
fn keystroke_to_bytes(ks: &Keystroke, app_cursor: bool, kitty_keys: bool) -> Option<Vec<u8>> {
    let m = &ks.modifiers;

    // Cmd+字母/数字/标点留给应用（复制、搜索、面板、分屏、切 pane）。
    // Backspace / Delete / 方向键 / Enter 等命名键跟 Ctrl/Alt 走同一条编码，
    // 不按 TUI 品牌或修饰键种类开白名单。
    if m.platform && is_super_app_chord(ks.key.as_str()) {
        return None;
    }

    // Enter 单独拎出来：遗留编码里 Shift/Alt/Ctrl+Enter 全都塌缩成 `\r`，跟裸 Enter 无从
    // 区分，所以 Claude Code 那种「Shift+Enter 换行、Enter 提交」在传统终端里天然做不到。
    // 对端开了 kitty keyboard protocol 时才按 CSI u 上报修饰键（Shift+Enter → `ESC[13;2u`）；
    // 没开就必须继续发 `\r`，否则 bash/zsh 里按 Shift+Enter 会把 `[13;2u` 当文本吐出来。
    if ks.key == "enter" {
        let mods = csi_u_modifiers(m);
        if kitty_keys && mods > 1 {
            return Some(format!("\x1b[13;{mods}u").into_bytes());
        }
        // 协议没开时的兜底：Alt+Enter 按传统 meta 前缀发 `ESC` + `CR`，Claude Code 认这条。
        if m.alt {
            return Some(b"\x1b\r".to_vec());
        }
        // 无 kitty 时 Shift+Enter 跟 Zed 一样发 LF（部分多行 prompt 靠这个）；裸 Enter 仍是 CR。
        // 注意：bash/zsh 默认把 LF 也当提交，行为与 CR 接近；有 kitty 时走上面 CSI u。
        if m.shift {
            return Some(b"\n".to_vec());
        }
        return Some(b"\r".to_vec());
    }

    // cmux/Ghostty 对 Cmd/Ctrl+Backspace、Cmd/Ctrl+Delete 直接发 readline 行删除
    // C0（^U / ^K）。Grok 认的是 Ctrl+U / Ctrl+K（删到行首/行尾），不是 CSI u 的
    // Ctrl+Delete（那是删词）。开着 kitty 也必须走 C0，否则对不上 cmux。
    if (m.platform || m.control)
        && !m.alt
        && let Some(bytes) = line_kill_c0(ks.key.as_str())
    {
        return Some(bytes);
    }

    // kitty keyboard protocol（对端发过 `CSI > 1 u`）：只把 **没有传统 CSI ~ / CSI
    // A-D 编码** 的键走 CSI u（Backspace / Tab / Escape；Enter 已单独处理）。
    // Delete / Insert / Page / 方向 / F 键 kitty 规定仍是 `CSI 3;5~` 这种
    // legacy functional 形式——cmux/Ghostty 也这么发。旧实现把 Delete 编成
    // `ESC[16;5u`，crossterm 当成 Char(0x10)，Grok 里 Ctrl+Delete 就没反应。
    if kitty_keys && modifiers_any(m) {
        if let Some(seq) = kitty_csi_u_key(ks.key.as_str(), m) {
            return Some(seq);
        }
        // Ctrl+Shift / Ctrl+Alt 的字符键：C0 控制码表达不了 shift/alt，硬发 C0 会
        // 丢修饰（程序把 Ctrl+Shift+P 当成 Ctrl+P）。kitty 模式按 CSI u 上报码点 +
        // 修饰；纯 Ctrl+字母保持 C0（readline 历史等依赖 0x10 语义，不能动）。
        if m.control
            && (m.shift || m.alt)
            && let Some(c) = ks.key.chars().next()
            && c.is_ascii()
        {
            return Some(format!("\x1b[{};{}u", u32::from(c), csi_u_modifiers(m)).into_bytes());
        }
    }

    // 没开 kitty 时，Cmd 不能编进 xterm 修饰位（只有 shift/alt/ctrl）。对齐
    // Ghostty/cmux 的 macOS natural editing：发成对应的 C0 控制符。
    // kitty 开着时 Super 位可以编进 CSI，交给下面的 modified_special_key。
    if m.platform && !kitty_keys {
        return super_natural_edit(ks.key.as_str());
    }

    // 有任意修饰键时，优先发 xterm 修饰序列（方向 / F / 导航键）。
    // 必须在「裸键」表之前：否则 Shift+Up 会掉进裸 `\x1b[A`，readline 词跳等全废。
    // Super（Cmd）也算：kitty 开着时 Cmd+Delete 是 `CSI 3;9~`，不能塌成裸 `CSI 3~`。
    if modifiers_any(m)
        && let Some(seq) = modified_special_key(ks.key.as_str(), m)
    {
        return Some(seq);
    }

    // 裸特殊键 + 部分固定修饰（Shift+Tab / Ctrl+Backspace 等）
    let named: Option<Vec<u8>> = match (ks.key.as_str(), m.shift, m.alt, m.control) {
        ("backspace", _, true, _) => Some(b"\x1b\x7f".to_vec()),
        ("backspace", _, _, true) => Some(b"\x08".to_vec()),
        ("backspace", _, _, _) => Some(b"\x7f".to_vec()),
        ("tab", true, _, _) => Some(b"\x1b[Z".to_vec()),
        ("tab", _, _, _) => Some(b"\t".to_vec()),
        ("escape", _, _, _) => Some(b"\x1b".to_vec()),
        ("left", _, _, _) => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }.to_vec()),
        ("right", _, _, _) => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }.to_vec()),
        ("up", _, _, _) => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }.to_vec()),
        ("down", _, _, _) => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }.to_vec()),
        ("home", _, _, _) => Some(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }.to_vec()),
        ("end", _, _, _) => Some(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }.to_vec()),
        ("insert", _, _, _) => Some(b"\x1b[2~".to_vec()),
        ("delete", _, _, _) => Some(b"\x1b[3~".to_vec()),
        ("pageup", _, _, _) => Some(b"\x1b[5~".to_vec()),
        ("pagedown", _, _, _) => Some(b"\x1b[6~".to_vec()),
        ("f1", _, _, _) => Some(b"\x1bOP".to_vec()),
        ("f2", _, _, _) => Some(b"\x1bOQ".to_vec()),
        ("f3", _, _, _) => Some(b"\x1bOR".to_vec()),
        ("f4", _, _, _) => Some(b"\x1bOS".to_vec()),
        ("f5", _, _, _) => Some(b"\x1b[15~".to_vec()),
        ("f6", _, _, _) => Some(b"\x1b[17~".to_vec()),
        ("f7", _, _, _) => Some(b"\x1b[18~".to_vec()),
        ("f8", _, _, _) => Some(b"\x1b[19~".to_vec()),
        ("f9", _, _, _) => Some(b"\x1b[20~".to_vec()),
        ("f10", _, _, _) => Some(b"\x1b[21~".to_vec()),
        ("f11", _, _, _) => Some(b"\x1b[23~".to_vec()),
        ("f12", _, _, _) => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(bytes);
    }

    // Ctrl+字母 / 若干标点 → C0 控制符
    if m.control
        && !m.alt
        && let Some(b) = ctrl_byte(ks.key.as_str())
    {
        return Some(vec![b]);
    }

    None
}

/// kitty CSI u：仅用于没有 legacy CSI ~ / CSI A–D 编码的键。
/// Delete 是 `CSI 3 ~`，必须走 [`modified_special_key`]，不能发 `CSI 16 u`。
/// Enter 已单独处理。键码见 kitty keyboard protocol Functional key definitions。
fn kitty_csi_u_key(key: &str, m: &Modifiers) -> Option<Vec<u8>> {
    let code = match key {
        "tab" => 9,
        "escape" => 27,
        "backspace" => 127,
        _ => return None,
    };
    Some(format!("\x1b[{code};{}u", csi_u_modifiers(m)).into_bytes())
}

/// xterm 修饰特殊键。mod 编码：1+ shift|alt<<1|ctrl<<2 → 2..=8。
/// 见 <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-PC-Style-Function-Keys>
fn modified_special_key(key: &str, m: &Modifiers) -> Option<Vec<u8>> {
    let mod_code = csi_u_modifiers(m);
    if mod_code <= 1 {
        return None;
    }
    let seq = match key {
        "up" => format!("\x1b[1;{mod_code}A"),
        "down" => format!("\x1b[1;{mod_code}B"),
        "right" => format!("\x1b[1;{mod_code}C"),
        "left" => format!("\x1b[1;{mod_code}D"),
        "home" => format!("\x1b[1;{mod_code}H"),
        "end" => format!("\x1b[1;{mod_code}F"),
        "f1" => format!("\x1b[1;{mod_code}P"),
        "f2" => format!("\x1b[1;{mod_code}Q"),
        "f3" => format!("\x1b[1;{mod_code}R"),
        "f4" => format!("\x1b[1;{mod_code}S"),
        "f5" => format!("\x1b[15;{mod_code}~"),
        "f6" => format!("\x1b[17;{mod_code}~"),
        "f7" => format!("\x1b[18;{mod_code}~"),
        "f8" => format!("\x1b[19;{mod_code}~"),
        "f9" => format!("\x1b[20;{mod_code}~"),
        "f10" => format!("\x1b[21;{mod_code}~"),
        "f11" => format!("\x1b[23;{mod_code}~"),
        "f12" => format!("\x1b[24;{mod_code}~"),
        "insert" => format!("\x1b[2;{mod_code}~"),
        "delete" => format!("\x1b[3;{mod_code}~"),
        "pageup" => format!("\x1b[5;{mod_code}~"),
        "pagedown" => format!("\x1b[6;{mod_code}~"),
        _ => return None,
    };
    Some(seq.into_bytes())
}

fn ctrl_byte(key: &str) -> Option<u8> {
    if key.len() != 1 {
        return None;
    }
    let c = key.as_bytes()[0];
    match c {
        b'@' => Some(0x00),
        b'a'..=b'z' => Some(c - b'a' + 1),
        b'A'..=b'Z' => Some(c.to_ascii_lowercase() - b'a' + 1),
        b'[' => Some(0x1b),
        b'\\' => Some(0x1c),
        b']' => Some(0x1d),
        b'^' => Some(0x1e),
        b'_' => Some(0x1f),
        b'?' => Some(0x7f),
        b' ' => Some(0x00),
        _ => None,
    }
}

/// CSI u / xterm 修饰键参数：基数 1，再按位叠加 shift(1) / alt(2) / ctrl(4) / super(8)。
/// 例：Shift+Enter → 2，于是 `ESC[13;2u`；Ctrl+Left → 5，于是 `ESC[1;5D`；
/// Cmd+Backspace → 9，于是 `ESC[127;9u`。
fn csi_u_modifiers(m: &Modifiers) -> u8 {
    1 + u8::from(m.shift)
        + (u8::from(m.alt) << 1)
        + (u8::from(m.control) << 2)
        + (u8::from(m.platform) << 3)
}

fn modifiers_any(m: &Modifiers) -> bool {
    m.shift || m.alt || m.control || m.platform
}

/// Cmd+单码点（字母/数字/标点）是应用快捷键，不进 PTY。
fn is_super_app_chord(key: &str) -> bool {
    key.chars().count() == 1
}

/// Cmd/Ctrl+Backspace → ^U（删到行首）；Cmd/Ctrl+Delete → ^K（删到行尾）。
fn line_kill_c0(key: &str) -> Option<Vec<u8>> {
    Some(match key {
        "backspace" => b"\x15".to_vec(),
        "delete" => b"\x0b".to_vec(),
        _ => return None,
    })
}

/// 无 kitty 时 Cmd 编不进 xterm 修饰位，发成 readline/Ghostty 那套 C0。
fn super_natural_edit(key: &str) -> Option<Vec<u8>> {
    Some(match key {
        "left" | "home" => b"\x01".to_vec(), // Ctrl+A：行首
        "right" | "end" => b"\x05".to_vec(), // Ctrl+E：行尾
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    // 不能 `use super::*`：那会把 gpui 的 `test` 属性宏一起带进来，盖掉标准 #[test]。
    use super::{
        BELL_NOTIFICATION_GRACE, CellStyle, LaunchKind, SCROLLBAR_THUMB_MIN, bell_notification_due,
        bg_spans, cells_to_token, classify_launch, fallback_attention, keystroke_to_bytes, link_at,
        scrollbar_thumb, terminal_event_needs_redraw, text_batches, visible_end,
    };
    use crate::terminal;
    use crate::terminal::Cell;
    use gpui::{Keystroke, Modifiers};
    use smelt_core::attention::AttentionKind;
    use std::time::{Duration, Instant};

    /// 默认样式：bg = None 表示「终端默认底色」（不画底色矩形）。
    const PLAIN: CellStyle = CellStyle {
        fg: 0xffffff,
        bg: None,
        bold: false,
        italic: false,
        dim: false,
        underline: false,
        undercurl: false,
        strikeout: false,
    };

    #[test]
    fn daemon_geometry_change_redraws_even_without_grid_damage() {
        assert!(terminal_event_needs_redraw(false, true));
        assert!(terminal_event_needs_redraw(true, false));
        assert!(!terminal_event_needs_redraw(false, false));
    }

    #[test]
    fn classifies_all_builtin_terminal_agents() {
        for agent in crate::settings::TerminalAgentKind::ALL {
            assert_eq!(
                classify_launch(Some(agent.quick_terminal_cmd())),
                LaunchKind::Agent(agent),
                "{} 的快捷命令应由统一注册表分类",
                agent.id()
            );
        }
        assert_eq!(
            classify_launch(Some("grok --minimal")),
            LaunchKind::Agent(crate::settings::TerminalAgentKind::Grok)
        );
        assert_eq!(classify_launch(Some("zsh")), LaunchKind::Terminal);
    }

    #[test]
    fn terminal_bell_waits_for_completion_grace_period() {
        let received_at = Instant::now();
        assert!(!bell_notification_due(Some(received_at), received_at));
        assert!(!bell_notification_due(
            Some(received_at),
            received_at + BELL_NOTIFICATION_GRACE - Duration::from_millis(1)
        ));
        assert!(bell_notification_due(
            Some(received_at),
            received_at + BELL_NOTIFICATION_GRACE
        ));
        assert!(!bell_notification_due(None, received_at));
    }

    #[test]
    fn terminal_notifications_are_informational() {
        let bell = fallback_attention(true, Some("ignored".into())).unwrap();
        assert_eq!(bell.0, AttentionKind::Bell);
        assert_eq!(bell.2, "🔔 响铃");

        let notice = fallback_attention(false, Some("turn complete".into())).unwrap();
        assert_eq!(notice.0, AttentionKind::Notice);
        assert!(fallback_attention(false, None).is_none());
    }

    /// 造一行 cell：宽字符（中文）自动补一个 '\0' 占位格，跟 alacritty 的网格一致。
    ///
    /// 宽度判定必须跟真实终端一致，否则测不出「歧义宽度」这类 bug：CJK / 全角
    /// （U+2E80 起）算两格，而 `·`(U+00B7) `—`(U+2014) `…`(U+2026) 这些**东亚歧义
    /// 宽度**字符终端只给一格——正是它们的字形（fallback 到中文字体后是全角）跟格子
    /// 对不上，才会把同批的后续字符顶歪。
    fn row(s: &str) -> Vec<Cell> {
        let cell = |ch: char| Cell {
            ch,
            fg: 0xffffff,
            bg: 0x000000,
            bg_default: true, // 默认底色（不画底色矩形）
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            undercurl: false,
            strikeout: false,
            zw: None,
            link: None,
            selected: false,
        };
        let mut out = Vec::new();
        for ch in s.chars() {
            let wide = (ch as u32) >= 0x2e80;
            out.push(cell(ch));
            if wide {
                out.push(cell('\0'));
            }
        }
        out
    }

    fn batches(cells: &[Cell]) -> Vec<(usize, String)> {
        text_batches(cells, cells.len(), &|_| PLAIN)
            .into_iter()
            .map(|b| (b.col, b.text))
            .collect()
    }

    /// 造一个只改了底色的样式（给 bg_spans 的测试用）。
    fn with_bg(bg: u32) -> CellStyle {
        CellStyle {
            bg: Some(bg),
            ..PLAIN
        }
    }

    /// 「拿空格承载底色」是终端里的常规操作：fzf 的选中行、tmux/vim 的状态栏、TUI 菜单的
    /// 选中项、多行拖选的中间行——文字后面跟着一长串**带底色的空格**（终端每行都补空格到
    /// 满列宽）。底色必须一路画到行尾；跟着字形一起截断在最后一个可见字符的话，高亮就缺
    /// 一截，多行选区看着像锯齿，而整行彩色空格的状态条会一个像素都画不出来。
    #[test]
    fn background_runs_to_end_of_row_not_to_last_glyph() {
        // 一行 8 格：ab + 6 个空格，整行同一个非默认底色（状态栏那种）
        let cells = row("ab      ");
        let sel = 0x0033_4a6a;
        assert_eq!(
            bg_spans(&cells, &|_| with_bg(sel)),
            vec![(0, 8, sel)],
            "底色要铺满 8 格，而不是停在最后一个可见字符（第 2 格）"
        );

        // 整行全是带底色的空格（纯色状态条）：一个可见字符都没有，照样得画满。
        let blanks = row("        ");
        assert_eq!(
            bg_spans(&blanks, &|_| with_bg(sel)),
            vec![(0, 8, sel)],
            "没有任何可见字符时，整行底色不能消失"
        );
    }

    /// 「默认底色」必须按**颜色枚举**判（`Cell::bg_default`），不能拿 RGB 去比。应用完全可以
    /// 显式设一个恰好等于默认底色的 RGB（`\e[48;2;…m`）——那是真要画的一块底色，而默认底色的
    /// 格子是**留空让背景图 / 透明度透出来**的。判错就会在本该是纯色块的地方漏出背景图。
    #[test]
    fn explicit_bg_equal_to_default_rgb_is_still_painted() {
        let bg = 0x1a1b26; // 假设它恰好就是当前主题的默认底色 RGB
        let mut cells = row("ab");
        for c in &mut cells {
            c.bg = bg;
            c.bg_default = false; // 应用显式设的，不是默认底色
        }
        let style_of = |i: usize| CellStyle {
            bg: (!cells[i].bg_default).then_some(cells[i].bg),
            ..PLAIN
        };
        assert_eq!(
            bg_spans(&cells, &style_of),
            vec![(0, 2, bg)],
            "应用显式设的底色要画出来，哪怕它的 RGB 跟默认底色一模一样"
        );

        // 反过来：默认底色的格子不画（让背景层透出）。
        let blanks = row("ab"); // row() 造出来的就是 bg_default = true
        assert_eq!(
            bg_spans(&blanks, &|i| CellStyle {
                bg: (!blanks[i].bg_default).then_some(blanks[i].bg),
                ..PLAIN
            }),
            vec![],
            "默认底色不画底色矩形"
        );
    }

    /// 反过来，字形不必画到行尾：尾部那些「什么都不画的空格」不进批次。但带下划线的空格
    /// （下划线本身要画）不算 blank。
    #[test]
    fn glyphs_stop_at_last_non_blank_cell() {
        let cells = row("ab      ");
        assert_eq!(visible_end(&cells, &|_| false), 2, "尾部纯空格不出字形");

        // 第 5 格是带下划线的空格（比如 OSC 8 链接铺在空格上）：要画线，不能截在它前面。
        let mut underlined = row("ab      ");
        underlined[5].underline = true;
        assert_eq!(
            visible_end(&underlined, &|_| false),
            6,
            "带下划线的空格不算 blank"
        );

        // 悬停链接高亮压在尾部空格上时同理。
        assert_eq!(
            visible_end(&cells, &|i| i == 4),
            5,
            "链接高亮的空格不算 blank"
        );
    }

    /// 造一个单行 Frame（`wrapped` 全 false），给不涉及软换行的 link_at 测试用。
    fn single_row_frame(cells: Vec<Cell>) -> terminal::Frame {
        terminal::Frame {
            rows: vec![cells],
            cursor: None,
            cursor_pos: None,
            wrapped: vec![false],
        }
    }

    /// OSC 8 超链接：可见文本只是标题（`Release notes`），真正的 URL 藏在协议里。正则扫
    /// 可见文本是找不到的，必须读 `Cell::link`，且范围要覆盖铺着同一个 URI 的所有格子。
    #[test]
    fn osc8_link_wins_over_regex_and_spans_its_cells() {
        use std::sync::Arc;
        let uri: Arc<str> = Arc::from("https://example.com/notes");
        let mut cells = row("ab cd");
        // 「cd」两格挂着 OSC 8 链接
        cells[3].link = Some(uri.clone());
        cells[4].link = Some(uri);
        let frame = single_row_frame(cells);

        assert_eq!(
            link_at(&frame, 0, 3),
            Some((vec![(0, 3, 4)], "https://example.com/notes".to_string())),
            "命中 OSC 8：范围覆盖挂着同一 URI 的连续格子"
        );
        assert_eq!(
            link_at(&frame, 0, 4),
            link_at(&frame, 0, 3),
            "同一链接内任意一格结果相同"
        );
        assert_eq!(
            link_at(&frame, 0, 0),
            None,
            "没挂链接、可见文本也不是 URL 的格子：没有链接"
        );
    }

    /// #21 的回归测试：长链接被终端软换行截成两截，`wrapped[0] == true` 表示第 0 行
    /// 没有真的换行、内容延续到第 1 行。点第 0 行末尾或第 1 行开头，都要拿到拼接后的
    /// 完整 URL，而不是被断行切出来的半截。
    #[test]
    fn url_split_across_soft_wrap_is_reassembled() {
        // 模拟窄终端（8 列）：一整条 URL 被截成两行显示。
        // 行 0: "https://" 行 1: "a.b/c123"（拼起来是 "https://a.b/c123"）
        let row0 = row("https://");
        let row1 = row("a.b/c123");
        let frame = terminal::Frame {
            rows: vec![row0, row1],
            cursor: None,
            cursor_pos: None,
            wrapped: vec![true, false],
        };

        let full = "https://a.b/c123".to_string();
        // 点在第 0 行最后一格（断行前）
        let (segs, url) = link_at(&frame, 0, 7).expect("软换行前半截也应命中链接");
        assert_eq!(url, full, "拼接后应是完整 URL，而不是被换行切断的前半截");
        assert_eq!(
            segs,
            vec![(0, 0, 7), (1, 0, 7)],
            "高亮范围要跨两行给出各自的列区间"
        );

        // 点在第 1 行开头（断行后）结果应完全一致
        assert_eq!(link_at(&frame, 1, 0), Some((segs, full)));
    }

    /// 零宽字符（变体选择器 U+FE0F、组合变音符等）挂在基字符那一格上（alacritty 的
    /// `cell.zerowidth()`）。它们**必须**跟着基字符一起进批交给排版器——丢了的话 `⚠️` 掉成
    /// 黑白的 `⚠`、`é` 掉成 `e`，而复制出去的文本却是带着它们的（alacritty 复制时会带），
    /// 于是「看到的 ≠ 复制到的」。
    ///
    /// 但它们**不能占格子**：`count` 记的是格数，多算一格的话，这一批里它后面每个字符的
    /// 列号都会偏，且下一批的续接判定（`start + count == i`）也会错位。
    #[test]
    fn zero_width_chars_ride_along_without_taking_a_cell() {
        // 网格：⚠(0) x(1) —— U+26A0 是窄字符，占一格；U+FE0F 挂在它身上、占零格。
        let mut cells = row("⚠x");
        cells[0].zw = Some(vec!['\u{fe0f}'].into_boxed_slice());

        assert_eq!(
            batches(&cells),
            vec![(0, "⚠\u{fe0f}x".to_string())],
            "变体选择器要跟着基字符进同一批，且不占格——x 仍然续在这一批里（列号连续）"
        );
    }

    /// 东亚歧义宽度字符（`·` `—` `…` 中文引号等）终端只给一格、字体却画成全角，**不需要**
    /// 在分批这层特殊照顾：批内每个 glyph 的 x 由 `shape_line` 的 force_width 钉死在
    /// `序号 × cell_w`（见 paint_row 头注），字形宽窄推不动任何人。所以它们跟后面的字符
    /// 连成一批是正确的，这里只钉住「确实连成了一批」，免得日后有人又去分批层打补丁。
    #[test]
    fn ambiguous_width_chars_batch_normally() {
        // 网格：—(0) —(1) 正(2,3) —— 破折号只占一格、不带 '\0' 占位，所以它俩连成一批；
        // 「正」是宽字符，按规矩独占一批（见 wide_chars_never_share_a_batch）。
        assert_eq!(
            batches(&row("——正")),
            vec![(0, "——".into()), (2, "正".into())]
        );
    }

    /// 核心不变量：每个全角字各自成批，且起始列 = 它在网格里的真实列号。
    /// 这正是表格错位的修复点——「明细条数」这样的中文后面跟着的 `│`，其列号必须是
    /// 按「每个中文占 2 格」算出来的，而不是由字体的实际字形宽度累积出来的。
    #[test]
    fn wide_chars_each_get_their_own_batch_at_grid_columns() {
        // 网格：中(0,1) 文(2,3) a(4) b(5)
        let cells = row("中文ab");
        assert_eq!(
            batches(&cells),
            vec![(0, "中".into()), (2, "文".into()), (4, "ab".into())],
            "两个中文各自成批、列号 0 和 2；后面的 ascii 从第 4 列起连成一批"
        );
    }

    /// **宽字符永远独占一批**，两条理由都是硬的：
    ///
    /// 1. **要居中**。中文字形约 1.0em，两格是 1.2em，左对齐会让字贴着格子左边——光标块
    ///    （满两格）压上去时左右空隙不对称，整行中文也都偏左。paint_row 得单独把它按真实
    ///    字形宽度居中到两格里，混在批里就没法单独定位。
    /// 2. **force_width 按 glyph 序号钉位**（`glyph_pos × cell_w`），而宽字符占两格却只算
    ///    一个 glyph。它后面若还有同批字符，那些字符会整体少一格。独占一批就彻底没这问题。
    #[test]
    fn wide_chars_never_share_a_batch() {
        // 网格：a(0) 中(1,2) x(3)
        assert_eq!(
            batches(&row("a中x")),
            vec![(0, "a".into()), (1, "中".into()), (3, "x".into())],
            "「中」独占一批钉在第 1 列，x 重新钉在第 3 列"
        );

        // 多个窄字符在前也一样。
        assert_eq!(
            batches(&row("ab中cd")),
            vec![(0, "ab".into()), (2, "中".into()), (4, "cd".into())],
        );
    }

    /// 纯 ascii 不该被切碎——一整段连续同样式的文本仍然只有一批（保住性能）。
    #[test]
    fn plain_ascii_stays_one_batch() {
        let cells = row("hello world");
        assert_eq!(batches(&cells), vec![(0, "hello world".into())]);
    }

    /// 样式变了就断批，且新批的列号要对（否则上色段会整体错位）。
    #[test]
    fn style_change_splits_batch_at_right_column() {
        let cells = row("abcd");
        // 前两格一种颜色，后两格另一种。
        let got: Vec<(usize, String)> = text_batches(&cells, cells.len(), &|i| {
            if i < 2 {
                PLAIN
            } else {
                CellStyle {
                    fg: 0xff0000,
                    ..PLAIN
                }
            }
        })
        .into_iter()
        .map(|b| (b.col, b.text))
        .collect();
        assert_eq!(got, vec![(0, "ab".into()), (2, "cd".into())]);
    }

    /// 空行不该产出任何批。
    #[test]
    fn empty_row_yields_no_batches() {
        assert!(batches(&[]).is_empty());
    }

    fn enter(shift: bool, alt: bool, control: bool) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                shift,
                alt,
                control,
                ..Default::default()
            },
            key: "enter".into(),
            key_char: None,
        }
    }

    /// 开了 kitty keyboard protocol 的 TUI（Claude Code v2.1+）必须能把 Shift+Enter
    /// 跟裸 Enter 区分开，否则「换行」会被当成「提交」。
    #[test]
    fn shift_enter_reports_csi_u_when_kitty_enabled() {
        let bytes = keystroke_to_bytes(&enter(true, false, false), false, true).unwrap();
        assert_eq!(bytes, b"\x1b[13;2u");
    }

    /// 修饰键位的叠加：alt=2、ctrl=4，都在基数 1 上加。
    #[test]
    fn other_enter_modifiers_report_csi_u_when_kitty_enabled() {
        let alt = keystroke_to_bytes(&enter(false, true, false), false, true).unwrap();
        assert_eq!(alt, b"\x1b[13;3u");
        let ctrl = keystroke_to_bytes(&enter(false, false, true), false, true).unwrap();
        assert_eq!(ctrl, b"\x1b[13;5u");
        let all = keystroke_to_bytes(&enter(true, true, true), false, true).unwrap();
        assert_eq!(all, b"\x1b[13;8u");
    }

    /// 裸 Enter 即便在 kitty 模式下也发遗留的 `\r`——协议如此规定，也让协议没被复位时
    /// 用户还能在 shell 里敲 `reset` 把终端救回来。
    #[test]
    fn plain_enter_stays_carriage_return() {
        let with_kitty = keystroke_to_bytes(&enter(false, false, false), false, true).unwrap();
        assert_eq!(with_kitty, b"\r");
        let without = keystroke_to_bytes(&enter(false, false, false), false, false).unwrap();
        assert_eq!(without, b"\r");
    }

    /// 没开协议的程序不认 CSI u：Shift+Enter 不能吐 `[13;2u` 乱码。跟 Zed 一样发 LF
    /// （`\n`）；bash/zsh 默认把 LF 也当提交，行为与裸 Enter 接近。
    #[test]
    fn shift_enter_falls_back_to_lf_without_kitty() {
        let bytes = keystroke_to_bytes(&enter(true, false, false), false, false).unwrap();
        assert_eq!(bytes, b"\n");
    }

    /// 协议没开时 Alt+Enter 还有条传统通道：meta 前缀 `ESC` + `CR`，Claude Code 认这个。
    #[test]
    fn alt_enter_falls_back_to_meta_prefix_without_kitty() {
        let bytes = keystroke_to_bytes(&enter(false, true, false), false, false).unwrap();
        assert_eq!(bytes, b"\x1b\r");
    }

    fn ks(key: &str, shift: bool, alt: bool, control: bool) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                shift,
                alt,
                control,
                ..Default::default()
            },
            key: key.into(),
            key_char: None,
        }
    }

    fn ks_cmd(key: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                platform: true,
                ..Default::default()
            },
            key: key.into(),
            key_char: None,
        }
    }

    /// readline / zsh 词跳靠 Ctrl+Left/Right 的 xterm 修饰序列；发裸方向键等于没按。
    #[test]
    fn ctrl_arrow_sends_xterm_modifier_sequence() {
        let left = keystroke_to_bytes(&ks("left", false, false, true), false, false).unwrap();
        assert_eq!(left, b"\x1b[1;5D");
        let up = keystroke_to_bytes(&ks("up", true, false, false), false, false).unwrap();
        assert_eq!(up, b"\x1b[1;2A");
    }

    /// 对端开了 kitty keyboard protocol（grok / Claude Code 启动发 `CSI > 1 u`）：
    /// Backspace/Tab/Esc 走 CSI u；Delete 按 kitty 规定仍是 `CSI 3;mods~`（cmux 同款）。
    #[test]
    fn kitty_mode_sends_csi_u_for_modified_function_keys() {
        // cmux 行删除：Ctrl+Delete → ^K，Ctrl+Backspace → ^U（开 kitty 也一样）
        assert_eq!(
            keystroke_to_bytes(&ks("delete", false, false, true), false, true).unwrap(),
            b"\x0b"
        );
        assert_eq!(
            keystroke_to_bytes(&ks("backspace", false, false, true), false, true).unwrap(),
            b"\x15"
        );
        // Shift+Tab 也走 CSI u（比遗留 `ESC[Z` 更标准）
        assert_eq!(
            keystroke_to_bytes(&ks("tab", true, false, false), false, true).unwrap(),
            b"\x1b[9;2u"
        );
        // 裸功能键无修饰时 kitty 模式仍保持传统编码（协议兼容）
        assert_eq!(
            keystroke_to_bytes(&ks("delete", false, false, false), false, true).unwrap(),
            b"\x1b[3~"
        );
        // Ctrl+Shift 字母走 CSI u（C0 表达不了 shift），纯 Ctrl 保持 C0（readline 依赖）
        assert_eq!(
            keystroke_to_bytes(&ks("P", true, false, true), false, true).unwrap(),
            b"\x1b[80;6u"
        );
        assert_eq!(
            keystroke_to_bytes(&ks("p", false, false, true), false, true).unwrap(),
            vec![0x10]
        );
        // Ctrl+Alt 字母走 CSI u；Ctrl+Esc 也带修饰上报
        assert_eq!(
            keystroke_to_bytes(&ks("a", false, true, true), false, true).unwrap(),
            b"\x1b[97;7u"
        );
        assert_eq!(
            keystroke_to_bytes(&ks("escape", false, false, true), false, true).unwrap(),
            b"\x1b[27;5u"
        );
    }

    /// 没开 kitty 时 Cmd 编不进 xterm 修饰位；对齐 Ghostty/cmux 发 C0。
    /// 任意 TUI / shell（不只 grok）都认 Ctrl+U/K/A/E。
    #[test]
    fn cmd_editing_keys_fall_back_to_c0_without_kitty() {
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("backspace"), false, false).unwrap(),
            b"\x15"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("delete"), false, false).unwrap(),
            b"\x0b"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("left"), false, false).unwrap(),
            b"\x01"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("right"), false, false).unwrap(),
            b"\x05"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("home"), false, false).unwrap(),
            b"\x01"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("end"), false, false).unwrap(),
            b"\x05"
        );
    }

    /// kitty 开着时 Cmd/Ctrl+Backspace/Delete 仍发行删除 C0（对齐 cmux 覆盖 kitty）。
    #[test]
    fn cmd_named_keys_report_super_in_kitty_mode() {
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("backspace"), false, true).unwrap(),
            b"\x15"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("delete"), false, true).unwrap(),
            b"\x0b"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("left"), false, true).unwrap(),
            b"\x1b[1;9D"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("right"), false, true).unwrap(),
            b"\x1b[1;9C"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("home"), false, true).unwrap(),
            b"\x1b[1;9H"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("end"), false, true).unwrap(),
            b"\x1b[1;9F"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("tab"), false, true).unwrap(),
            b"\x1b[9;9u"
        );
        assert_eq!(
            keystroke_to_bytes(&ks_cmd("enter"), false, true).unwrap(),
            b"\x1b[13;9u"
        );
    }

    /// Cmd+字母仍是应用快捷键（复制/搜索/面板/分屏），不能灌进 PTY。
    #[test]
    fn cmd_letter_stays_with_the_app() {
        assert_eq!(keystroke_to_bytes(&ks_cmd("c"), false, false), None);
        assert_eq!(keystroke_to_bytes(&ks_cmd("c"), false, true), None);
        assert_eq!(keystroke_to_bytes(&ks_cmd("k"), false, true), None);
        assert_eq!(keystroke_to_bytes(&ks_cmd("a"), false, true), None);
        assert_eq!(keystroke_to_bytes(&ks_cmd("["), false, true), None);
        assert_eq!(keystroke_to_bytes(&ks_cmd("1"), false, false), None);
    }

    /// Ctrl+字母走 C0：Ctrl+U/K/W 是行编辑，任意 shell / TUI 都认，不依赖 kitty。
    #[test]
    fn ctrl_letter_sends_c0() {
        assert_eq!(
            keystroke_to_bytes(&ks("u", false, false, true), false, false).unwrap(),
            vec![0x15]
        );
        assert_eq!(
            keystroke_to_bytes(&ks("k", false, false, true), false, false).unwrap(),
            vec![0x0b]
        );
        assert_eq!(
            keystroke_to_bytes(&ks("w", false, false, true), false, false).unwrap(),
            vec![0x17]
        );
        // kitty 模式下纯 Ctrl+字母仍保持 C0，readline 语义不能改成 CSI u。
        assert_eq!(
            keystroke_to_bytes(&ks("u", false, false, true), false, true).unwrap(),
            vec![0x15]
        );
        // Ctrl+Delete / Ctrl+Backspace 对齐 cmux：删行，不是 CSI 词删除。
        assert_eq!(
            keystroke_to_bytes(&ks("delete", false, false, true), false, false).unwrap(),
            b"\x0b"
        );
        assert_eq!(
            keystroke_to_bytes(&ks("backspace", false, false, true), false, false).unwrap(),
            b"\x15"
        );
    }

    /// 未开 kitty 时 Ctrl+Shift+P 仍退化 C0——shell/readline 场景不受影响。
    /// Ctrl+Delete/Backspace 走行删除 C0，见 `ctrl_letter_sends_c0`。
    #[test]
    fn non_kitty_mode_keeps_xterm_encodings() {
        assert_eq!(
            keystroke_to_bytes(&ks("P", true, false, true), false, false).unwrap(),
            vec![0x10]
        );
    }

    #[test]
    fn function_keys_encode_like_xterm() {
        assert_eq!(
            keystroke_to_bytes(&ks("f5", false, false, false), false, false).unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            keystroke_to_bytes(&ks("f1", false, false, false), false, false).unwrap(),
            b"\x1bOP"
        );
        // 带修饰
        assert_eq!(
            keystroke_to_bytes(&ks("f5", true, false, false), false, false).unwrap(),
            b"\x1b[15;2~"
        );
    }

    /// 宽字符第二格是 `'\0'` 占位：拼路径 token 时必须跳过，否则 `Path::exists` 因 NUL 失败。
    #[test]
    fn cells_to_token_skips_wide_char_spacers() {
        // 网格：/(0) 中(1,2=\0) 文(3,4=\0)
        let cells = row("/中文");
        assert!(
            cells.iter().any(|c| c.ch == '\0'),
            "测试前提：中文应带占位格"
        );
        let token = cells_to_token(&cells, 0, cells.len());
        assert_eq!(token, "/中文");
        assert!(!token.as_bytes().contains(&0), "token 里不能夹 NUL");
    }

    /// 首帧 grid_size 还是 (0,0)，track_h 兜底成 1.0——比 THUMB_MIN 还矮。
    /// `clamp(28.0, 1.0)` 会 panic（min > max），GPUI 启动回调不能 unwind → 整个 app abort。
    #[test]
    fn scrollbar_thumb_survives_first_frame_tiny_track() {
        let (thumb_h, thumb_y) = scrollbar_thumb(1.0, 40, 100, 0);
        assert!(thumb_h <= 1.0, "thumb 不能超出轨道: {thumb_h}");
        assert!(thumb_y >= 0.0);
    }

    /// 正常尺寸下 thumb 高度按可视行占比走，且不小于最短高度。
    #[test]
    fn scrollbar_thumb_normal_geometry() {
        let track_h = 600.0;
        // 一半可视一半回滚：thumb 占轨道一半
        let (thumb_h, thumb_y) = scrollbar_thumb(track_h, 50, 50, 0);
        assert_eq!(thumb_h, 300.0);
        assert_eq!(thumb_y, 300.0, "offset=0 应贴底");
        // 回滚极深：受最短高度托底
        let (thumb_h, thumb_y) = scrollbar_thumb(track_h, 40, 100_000, 100_000);
        assert_eq!(thumb_h, SCROLLBAR_THUMB_MIN);
        assert_eq!(thumb_y, 0.0, "offset=max 应贴顶");
        // 无回滚：占满轨道、贴顶
        let (thumb_h, thumb_y) = scrollbar_thumb(track_h, 40, 0, 0);
        assert_eq!(thumb_h, track_h);
        assert_eq!(thumb_y, 0.0);
    }
}
