//! 内嵌终端前端：连接 smeltd 守护进程拿字节流 + alacritty_terminal 做终端状态机。
//!
//! PTY 与 shell 活在 smeltd 里（GUI 退出不杀会话，重开按 id 重连并重放恢复画面，
//! 类 tmux；协议见 crates/smeltd/src/main.rs 头注释）。数据流：后台线程读守护 socket →
//! vte 解析器 advance → 更新共享的 Term 网格；UI 线程定时对网格做快照并重绘。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{
    Config, SEMANTIC_ESCAPE_CHARS, Term, TermDamage, TermMode, point_to_viewport, viewport_to_point,
};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use smelt_core::daemon_protocol::DaemonOperation;

type SearchMatch = (Point, Point);
type SearchResult = (u64, String, i32, Vec<SearchMatch>);
type HandshakeResult = (BufReader<UnixStream>, TermSize, usize, Option<String>, bool);

/// 深浅色模式：进程内只有一套主题（设置页全局切换），用一个原子量足够，不必给
/// 每个 Terminal/EventProxy 各传一份——见 `set_dark_mode`（main.rs 在
/// Appearance.theme_mode 变化时同步调用）与下面 `default_fg`/`default_bg`/`palette`。
static DARK_MODE: AtomicBool = AtomicBool::new(true);

/// 切换终端配色跟随的深浅色模式。
pub fn set_dark_mode(dark: bool) {
    DARK_MODE.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

/// 测试用：深浅色模式和用户自选底色都是进程级全局态，同一个测试二进制里的用例
/// 并行跑会互相踩。凡是要定住这些全局态的用例统一在这把锁上排队。
#[cfg(test)]
pub(crate) fn lock_theme_globals() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 默认前景色：深色取 iTerm2 风格灰白正文，浅色取近白底 + 深灰正文。
const DEFAULT_FG_DARK: u32 = 0x00d8_d8d8;
const DEFAULT_FG_LIGHT: u32 = 0x0024_292e;

pub fn default_fg() -> u32 {
    if is_dark() {
        DEFAULT_FG_DARK
    } else {
        DEFAULT_FG_LIGHT
    }
}

/// 默认背景色：跟卡片本体同色系（`ui_theme::bg_stage`），不再是独立的 Tokyo
/// Night 深蓝黑——终端面板紧贴在舞台头下面，两者用不同色系时，标题栏透明后
/// 反而更显眼地露出一条界缝。ANSI 16 色板（下面 PALETTE_DARK/LIGHT）仍保留
/// Tokyo Night 配色（语法高亮，不是 UI 皮），只有「没手动设置背景色」时兜底的
/// 这个默认底色跟着舞台底走。
///
/// 用户在设置里自选过底色时以用户的为准（见 `set_bg_override`）：渲染层、OSC 11
/// 应答、下发给手机的配色快照必须是同一个值，否则 TUI 按查到的底色挑灰度就会
/// 挑错档。
pub fn default_bg() -> u32 {
    match BG_OVERRIDE.load(Ordering::Relaxed) {
        NO_BG_OVERRIDE => crate::ui_theme::bg_stage(),
        color => color,
    }
}

/// 「没有用户自选底色」的哨兵值：合法色值只占低 24 位，这个值不可能撞上。
const NO_BG_OVERRIDE: u32 = u32::MAX;
static BG_OVERRIDE: AtomicU32 = AtomicU32::new(NO_BG_OVERRIDE);

/// 设置/清除用户自选终端底色（`Appearance.bg_color` 被改过时传 Some）。
/// 由 `settings::apply_appearance` 统一调用，跟 `set_dark_mode` 一个套路：
/// PTY 线程上的 `EventProxy` 读不到 GPUI 全局态，只能靠原子量镜像一份。
pub fn set_bg_override(color: Option<u32>) {
    BG_OVERRIDE.store(color.unwrap_or(NO_BG_OVERRIDE), Ordering::Relaxed);
}

/// 16 色 ANSI 调色板：深色沿用 Tokyo Night（白/亮白改为灰白/纯白，iTerm2 风格）；
/// 浅色是同色相压深/加饱和的对应版本，保证在浅底上仍有足够对比度。
const PALETTE_DARK: [u32; 16] = [
    0x0015_161e,
    0x00f7_768e,
    0x009e_ce6a,
    0x00e0_af68,
    0x007a_a2f7,
    0x00bb_9af7,
    0x007d_cfff,
    0x00c7_c7c7,
    0x002c_3149,
    0x00f7_768e,
    0x009e_ce6a,
    0x00e0_af68,
    0x007a_a2f7,
    0x00bb_9af7,
    0x007d_cfff,
    0x00ff_ffff,
];
const PALETTE_LIGHT: [u32; 16] = [
    // 显式 ANSI black 常被 TUI 用来画分隔线；浅底上用深灰，避免比正文还抢眼。
    0x003f_4654,
    0x00c0_324a,
    0x004e_8a2f,
    0x00a1_690f,
    0x0037_60bf,
    0x0078_47bd,
    0x000f_7b9e,
    0x004a_4a4a,
    0x006b_7089,
    0x00d7_495f,
    0x005f_ae3f,
    0x00c4_8511,
    0x002e_6fe0,
    0x0091_61d9,
    0x0010_93c2,
    0x001a_1b26,
];

fn palette() -> &'static [u32; 16] {
    if is_dark() {
        &PALETTE_DARK
    } else {
        &PALETTE_LIGHT
    }
}

/// 当前 ANSI 16 色板（构建下发给移动端的配色快照用，见 `settings::publish_terminal_theme`）。
pub fn ansi_palette() -> &'static [u32; 16] {
    palette()
}

/// 一个渲染用的终端单元：字符 + 前景/背景 rgb + 字形修饰 + 是否在选区内。
///
/// 需要 Clone：跨物理行拼接链接文本（见 terminal_view.rs 的 `wrapped_line_range` /
/// 软换行拼接逻辑）要把若干行的 cell 拷进一个临时缓冲区再整体扫描。
#[derive(Clone)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
    /// SGR 3。bat / delta 的注释、agent 输出的强调文本都在用。
    pub italic: bool,
    /// SGR 2（faint）。CLI 里做视觉层级的主力——git 的次要信息、`ls` 的元数据、
    /// agent 的灰色提示行。渲染侧把前景色的 alpha 乘 0.7（跟 Zed / alacritty 一致）。
    pub dim: bool,
    /// 任意一种下划线（SGR 4 及其变体，alacritty 的 `ALL_UNDERLINES` 聚合位）。
    pub underline: bool,
    /// 下划线是波浪线（SGR 4:3）。编译器诊断、`rg --hyperlink`、TUI 的错误标注在用。
    pub undercurl: bool,
    /// SGR 9 删除线。
    pub strikeout: bool,
    /// 挂在这一格上的**零宽字符**（alacritty 的 `cell.zerowidth()`）：变体选择器
    /// （`⚠` + U+FE0F 才是彩色 emoji ⚠️）、组合变音符（`e` + U+0301 = é）、ZWJ 等。
    ///
    /// 它们必须跟着基字符一起交给排版器，但**不占格子**。丢掉的话：emoji 掉成黑白字形，
    /// 带声调的文字直接掉音标——而 alacritty 复制时是带上它们的，于是「看到的 ≠ 复制到的」。
    /// 绝大多数格子没有零宽字符，用 Option 免掉每格一次分配。
    pub zw: Option<Box<[char]>>,
    /// OSC 8 超链接（`ESC]8;;uri ST`）的目标 URI。`eza` / `gh` / `npm` / `cargo` 和各家
    /// agent 都在用它——**可见文本是标题、URL 藏在协议里**，所以光靠正则扫可见文本
    /// （见 find_urls）是找不到的。这是终端协议层的东西，不绑定任何一家 agent。
    pub link: Option<Arc<str>>,
    /// 这一格的底色是不是「终端默认底色」。**必须按颜色枚举判**（`Color::Named(Background)`，
    /// 跟 Zed 的 `is_default_background_color` 一致），不能拿解析出来的 RGB 去比：应用完全
    /// 可以显式设一个恰好等于默认底色的 RGB（`\e[48;2;…m`），那时它是「真的画了一块底色」，
    /// 而我们开着背景图 / 透明度时，默认底色的格子是**留空让背景透出来**的——判错就会在
    /// 本该是纯色块的地方漏出背景图。
    pub bg_default: bool,
    pub selected: bool,
}

/// 选区类型（对 terminal_view 屏蔽 alacritty 的 SelectionType）：
/// Simple=普通拖选，Word=双击选词（语义边界），Line=三击选整行。
#[derive(Clone, Copy)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}

/// 光标形状（对 terminal_view 屏蔽 alacritty 的 CursorShape）。应用用 DECSCUSR
/// （`CSI Ps SP q`）切换——zsh 的 vi-mode 用它在插入态显竖线、普通态显方块。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorKind {
    /// 实心方块（默认）
    Block,
    /// 下划线
    Underline,
    /// 竖线
    Bar,
    /// 空心方块
    Hollow,
}

/// 一帧终端快照：网格行 + 光标。
pub struct Frame {
    pub rows: Vec<Vec<Cell>>,
    /// **可见**光标 (行, 列, 形状)。None = 已上滚离开可视区，或应用用 `CSI ?25l`
    /// 隐藏了光标——全屏 TUI（Cursor CLI / Claude Code 等）常隐藏真实光标、在自己的
    /// 输入框里画反色假光标，这时真实光标往往停在角落，照画会多出一个孤立色块。
    pub cursor: Option<(usize, usize, CursorKind)>,
    /// 光标**位置** (行, 列)，含被隐藏的情况；None 仅表示不在可视区内。
    /// IME 候选窗 / 预编辑串定位用——光标藏没藏，输入法都得知道往哪落。
    pub cursor_pos: Option<(usize, usize)>,
    /// `wrapped[i] == true` 表示第 i 行是**软换行**——内容本来更长，只是屏幕宽度不够
    /// 被截到下一行，并不是应用真的输出了换行符（alacritty 的 `Flags::WRAPLINE`，打在
    /// 该行最后一格上）。终端里打印的长链接常常正好卡在这种断点上：链接识别
    /// （见 terminal_view.rs 的 `find_links`）按整行扫描的话会在断点处把 URL 切成两截，
    /// 点击只拿到前/后半截、打开错误地址（#21）。用这个数组把同一条软换行链串起来的
    /// 物理行拼成一行再扫描，就不会切错。长度与 `rows` 一致。
    pub wrapped: Vec<bool>,
}

/// 把 alacritty 的 Color 解析成 0xRRGGBB。is_fg 决定「默认色」取前景还是背景。
fn resolve(color: Color, is_fg: bool) -> u32 {
    match color {
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        Color::Indexed(i) => indexed_rgb(i),
        Color::Named(n) => named_rgb(n, is_fg),
    }
}

fn named_rgb(n: NamedColor, is_fg: bool) -> u32 {
    use NamedColor::*;
    let p = palette();
    match n {
        Black => p[0],
        Red => p[1],
        Green => p[2],
        Yellow => p[3],
        Blue => p[4],
        Magenta => p[5],
        Cyan => p[6],
        White => p[7],
        BrightBlack => p[8],
        BrightRed => p[9],
        BrightGreen => p[10],
        BrightYellow => p[11],
        BrightBlue => p[12],
        BrightMagenta => p[13],
        BrightCyan => p[14],
        BrightWhite => p[15],
        Background => default_bg(),
        // Foreground / Cursor / Dim* / 未来新增变体统一回落到默认色
        _ => {
            if is_fg {
                default_fg()
            } else {
                default_bg()
            }
        }
    }
}

/// xterm 256 色索引 → rgb：0-15 用调色板，16-231 为 6×6×6 色立方，232-255 为灰阶。
fn indexed_rgb(i: u8) -> u32 {
    match i {
        0..=15 => palette()[i as usize],
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| -> u32 { if v == 0 { 0 } else { 55 + v as u32 * 40 } };
            (step(i / 36) << 16) | (step((i % 36) / 6) << 8) | step(i % 6)
        }
        232..=255 => {
            let v = 8 + (i as u32 - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

/// 终端尺寸，实现 alacritty 的 Dimensions（先固定行列，resize 留到下一步）。
#[derive(Clone, Copy)]
pub struct TermSize {
    pub rows: usize,
    pub cols: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// OSC 9/99/777 普通通知槽；UI 侧轮询后用于横幅/通知记录，不参与 Agent 状态。
type NotifySlot = Arc<Mutex<Option<String>>>;

/// 网格行列 + 单元格像素尺寸，给 OSC/CSI 查询（TextAreaSizeRequest）和 PTY resize 用。
#[derive(Clone, Copy)]
struct TermMetrics {
    rows: u16,
    cols: u16,
    /// 单格宽/高（像素）。0 = 未知（首帧量字宽之前）。
    cell_w: u16,
    cell_h: u16,
}

#[derive(Default)]
struct DaemonGeometrySignal {
    generation: u64,
    geometry: Option<smelt_core::osc::TerminalGeometryOsc>,
}

/// smeltd 对单帧的硬上限；大输入由 writer 线程按这个值流式切分后再写 socket。
const TERMINAL_FRAME_MAX_BYTES: usize = 1 << 20;
const TERMINAL_WRITE_QUEUE_CAPACITY: usize = 256;
/// 输入请求本身可以大于帧队列预算，但仍要有独立上限，避免用户连续粘贴把内存吃满。
const TERMINAL_PENDING_INPUT_MAX_BYTES: usize = 64 * 1024 * 1024;
const TERMINAL_WRITE_QUEUE_MAX_BYTES: usize = 4 * 1024 * 1024;

enum TerminalWrite {
    /// 已经是单帧的控制/resize 写入。
    Frame { ty: u8, payload: Vec<u8> },
    /// 用户/终端输入；writer 线程消费时再按单帧上限切分，保持请求内顺序。
    Input(Vec<u8>),
}

/// GUI / PTY 读线程到 smeltd attachment 的单写端。
///
/// 所有实际 socket 写入都在专属线程里执行；调用方只做有界 `try_send`，因此守护停止
/// 消费时不能把 GPUI 主线程睡在内核的 `send`/`write` 里。队列满或写线程失败则主动
/// 断开 attachment，复用既有的自动 reattach；输入请求另有 64 MiB 上限，避免把
/// 单次大粘贴误当成 4 MiB 的帧队列上限。
#[derive(Clone)]
struct TerminalWriter {
    tx: smol::channel::Sender<TerminalWrite>,
    queued_bytes: Arc<AtomicUsize>,
    pending_input_bytes: Arc<AtomicUsize>,
    shutdown: Arc<UnixStream>,
    closed: Arc<AtomicBool>,
}

impl TerminalWriter {
    fn start(mut stream: UnixStream) -> std::io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let shutdown = Arc::new(stream.try_clone()?);
        let (tx, rx) = smol::channel::bounded(TERMINAL_WRITE_QUEUE_CAPACITY);
        let writer = Self {
            tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            shutdown,
            closed: Arc::new(AtomicBool::new(false)),
        };
        let worker = writer.clone();
        std::thread::Builder::new()
            .name("smelt-terminal-writer".into())
            .spawn(move || {
                while let Ok(write) = rx.recv_blocking() {
                    let result = match write {
                        TerminalWrite::Frame { ty, payload } => {
                            debug_assert!(payload.len() <= TERMINAL_FRAME_MAX_BYTES);
                            let byte_len = payload.len();
                            let result = write_frame(&mut stream, ty, &payload);
                            worker.queued_bytes.fetch_sub(byte_len, Ordering::AcqRel);
                            result
                        }
                        TerminalWrite::Input(bytes) => {
                            let byte_len = bytes.len();
                            let mut result = Ok(());
                            for payload in bytes.chunks(TERMINAL_FRAME_MAX_BYTES) {
                                if let Err(error) = write_frame(&mut stream, 0, payload) {
                                    result = Err(error);
                                    break;
                                }
                            }
                            worker
                                .pending_input_bytes
                                .fetch_sub(byte_len, Ordering::AcqRel);
                            result
                        }
                    };
                    if result.is_err() {
                        worker.close();
                        return;
                    }
                }
            })?;
        Ok(writer)
    }

    fn reserve_bytes(&self, byte_len: usize) -> bool {
        if byte_len == 0 || byte_len > TERMINAL_WRITE_QUEUE_MAX_BYTES {
            return byte_len == 0;
        }
        let mut queued = self.queued_bytes.load(Ordering::Acquire);
        loop {
            if self.closed.load(Ordering::Acquire)
                || queued > TERMINAL_WRITE_QUEUE_MAX_BYTES.saturating_sub(byte_len)
            {
                return false;
            }
            match self.queued_bytes.compare_exchange_weak(
                queued,
                queued + byte_len,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => queued = current,
            }
        }
    }

    /// 按帧占用控制帧队列预算。`TERMINAL_WRITE_QUEUE_MAX_BYTES` 限制的是同时等待
    /// writer 消费的帧，而不是一次输入请求的总长度。
    fn enqueue_frames<'a, I>(&self, ty: u8, payloads: I) -> bool
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        for payload in payloads {
            debug_assert!(payload.len() <= TERMINAL_FRAME_MAX_BYTES);
            let payload_len = payload.len();
            if !self.reserve_bytes(payload_len) {
                self.close();
                return false;
            }
            match self.tx.try_send(TerminalWrite::Frame {
                ty,
                payload: payload.to_vec(),
            }) {
                Ok(()) => {}
                Err(_) => {
                    self.queued_bytes.fetch_sub(payload_len, Ordering::AcqRel);
                    self.close();
                    return false;
                }
            }
        }
        true
    }

    fn send_input(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        if bytes.len() > TERMINAL_PENDING_INPUT_MAX_BYTES {
            // `false` 的语义必须是「这一整段输入没有进入旧 attachment」。
            // 否则上层在 reattach 后无法安全补发，用户就会在断线窗口里丢键。
            self.close();
            return false;
        }
        let mut pending = self.pending_input_bytes.load(Ordering::Acquire);
        loop {
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            if pending > TERMINAL_PENDING_INPUT_MAX_BYTES.saturating_sub(bytes.len()) {
                // 不把新输入塞到一个已经无法及时排空的旧连接后面。关闭 attachment
                // 让上层走同一条 reattach + 保序补发路径，避免新旧两条流乱序。
                self.close();
                return false;
            }
            match self.pending_input_bytes.compare_exchange_weak(
                pending,
                pending + bytes.len(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => pending = current,
            }
        }
        if self
            .tx
            .try_send(TerminalWrite::Input(bytes.to_vec()))
            .is_err()
        {
            self.pending_input_bytes
                .fetch_sub(bytes.len(), Ordering::AcqRel);
            self.close();
            return false;
        }
        true
    }

    fn send_resize(&self, payload: &[u8]) -> bool {
        if payload.len() > TERMINAL_FRAME_MAX_BYTES {
            self.close();
            return false;
        }
        self.enqueue_frames(1, std::iter::once(payload))
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.tx.close();
            // `shutdown` 也是系统调用。这里可能由 GPUI 回调（队列满 / 视图销毁）触发，
            // 所以同样交给后台，不能为了打断一个卡住的 writer 又把 UI 拖进内核。
            let shutdown = Arc::clone(&self.shutdown);
            let _ = std::thread::Builder::new()
                .name("smelt-terminal-close".into())
                .spawn(move || {
                    let _ = shutdown.shutdown(Shutdown::Both);
                });
        }
    }
}

/// 事件代理：alacritty 的 EventListener。BEL 只产生普通终端通知，不参与 Agent 状态；PtyWrite /
/// Clipboard* / TextAreaSizeRequest → 写回 PTY 或系统剪贴板；颜色查询由新守护处理，
/// 旧守护没有声明该能力时才在客户端兜底。
/// 其余事件仍忽略（重绘走 UI 定时快照）。
#[derive(Clone)]
struct EventProxy {
    bell_notify: NotifySlot,
    /// 终端标题（OSC 0/2）——Claude Code 用它报告任务名和展示装饰。
    title: Arc<Mutex<Option<String>>>,
    /// 守护连接写队列，跟 [`Terminal`] 自己发键盘输入共用同一个单消费者，保证帧不会
    /// 交叉，同时调用方不直接执行 socket I/O。
    writer: TerminalWriter,
    /// 当前网格/单元格尺寸（TextAreaSizeRequest 应答用）。
    metrics: Arc<Mutex<TermMetrics>>,
    /// 新版 smeltd 在收到 PTY 输出时就应答 OSC 颜色查询，覆盖首个 GUI attachment
    /// 还未挂上的窗口；能力位缺失说明是旧守护，继续由客户端兼容处理。
    daemon_handles_color_requests: bool,
}

impl EventProxy {
    /// 把响应字节当作「PTY 输入」帧写回守护——对 shell/CLI 来说，终端主动应答的
    /// 查询（光标位置、颜色）和用户敲键盘没有区别，都是它 stdin 收到的字节。
    fn write_pty(&self, bytes: &[u8]) {
        let _ = self.writer.send_input(bytes);
    }

    /// alacritty 自己不记「当前实际渲染色」，查询颜色时要由我们把 RGB 值喂回去。
    /// smelt 没有运行时改色的路径（无 OSC 4/10/11 set-color 场景），直接用当前主题的
    /// 默认前景 / 背景 / 16 色板作答，覆盖 CLI 常见的「查一下背景色决定用什么灰」——
    /// 取的是 `palette()`/`default_fg`/`default_bg`，跟着 `set_dark_mode` 一起切换。
    fn resolve_color(index: usize) -> Rgb {
        let to_rgb = |hex: u32| Rgb {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
        };
        let p = palette();
        if index < p.len() {
            to_rgb(p[index])
        } else if index == NamedColor::Background as usize {
            to_rgb(default_bg())
        } else {
            to_rgb(default_fg())
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Bell => {
                if let Ok(mut bell) = self.bell_notify.lock() {
                    *bell = Some("🔔 响铃".to_string());
                }
            }
            Event::Title(t) => {
                if let Ok(mut g) = self.title.lock() {
                    *g = Some(t);
                }
            }
            // 光标位置 / 设备属性等查询-应答协议：不回应会让依赖精确光标位置渲染
            // 的 TUI（如 Claude Code 的输入框 ghost-text 补全）拿不到定位信息。
            Event::PtyWrite(text) => self.write_pty(text.as_bytes()),
            Event::ColorRequest(index, format) if !self.daemon_handles_color_requests => {
                self.write_pty(format(Self::resolve_color(index)).as_bytes())
            }
            Event::ColorRequest(_, _) => {}
            // OSC 52：应用把文本写到系统剪贴板 / 从剪贴板读回。远程会话、嵌套
            // tmux、部分 CLI 复制都靠它。读写走系统工具（见 os_clipboard_*），不必
            // 绕到 UI 线程——EventProxy 跑在 PTY 读线程上。
            Event::ClipboardStore(_ty, data) => os_clipboard_write(&data),
            Event::ClipboardLoad(_ty, format) => {
                let text = os_clipboard_read();
                self.write_pty(format(&text).as_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let m = self.metrics.lock().ok().map(|g| *g).unwrap_or(TermMetrics {
                    rows: 24,
                    cols: 80,
                    cell_w: 0,
                    cell_h: 0,
                });
                let ws = WindowSize {
                    num_lines: m.rows,
                    num_cols: m.cols,
                    cell_width: m.cell_w,
                    cell_height: m.cell_h,
                };
                self.write_pty(format(ws).as_bytes());
            }
            _ => {}
        }
    }
}

/// OSC 52 写系统剪贴板。macOS 用 `pbcopy`（任意线程可调）；其它平台静默忽略。
fn os_clipboard_write(text: &str) {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
    }
}

/// OSC 52 读系统剪贴板。macOS 用 `pbpaste`；失败 / 其它平台返回空串（format 仍会写出
/// 空应答，对端不至于卡死等回包）。
fn os_clipboard_read() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("pbpaste").output() {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
    }
    String::new()
}

// OSC 9/99/777 扫描（`OscScan`）在 crate::osc，跟守护共用；含 Kitty OSC 99。

// ===================== smeltd 守护连接层 =====================

fn sock_path() -> std::path::PathBuf {
    smelt_core::daemon_state::smeltd_sock_path()
}

/// 守护常驻路径：与 `.app` 解耦。装 DMG / 覆盖 App 不会 cp 到正在跑的二进制，
/// 避免 macOS 对签名文件覆盖时 SIGKILL smeltd → 会话全灭 → 对话被「重新初始化」。
///
/// **硬约束**：smeltd 进程的 `current_exe` 必须是 `~/.smelt/bin/smeltd`，绝不能是
/// `Smelt.app/Contents/MacOS/smeltd`。否则用户用 Finder 拖 DMG 覆盖 App 时内核会
/// 干掉守护，所有 Claude/Grok PTY 死掉，GUI 重开只能 spawn 新进程 → 对话「重新初始化」。
fn managed_daemon_dir() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| "/tmp/.smelt".into())
        .join("bin")
}

pub fn managed_daemon_path() -> std::path::PathBuf {
    managed_daemon_dir().join("smeltd")
}

fn staged_daemon_path() -> std::path::PathBuf {
    managed_daemon_dir().join("smeltd.next")
}

/// 路径是否落在某个 `.app` 包内（装 DMG 会被覆盖/删除）。
fn path_inside_app_bundle(p: &std::path::Path) -> bool {
    p.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.ends_with(".app")))
}

/// 规范化路径后比较是否指向同一文件（symlink / 相对路径）。
fn same_daemon_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// 进程是否已住在 managed 目录（正式名 smeltd 或交接中的 smeltd.next 都算）。
/// rename next→smeltd 后 macOS 上 current_exe 可能仍报旧路径名，不能只比文件名。
fn exe_is_managed(exe: &std::path::Path) -> bool {
    let managed = managed_daemon_path();
    if same_daemon_path(exe, &managed) {
        return true;
    }
    let dir = managed_daemon_dir();
    if exe.starts_with(&dir) {
        return true;
    }
    // current_exe 仍写 smeltd.next 但文件已 rename：比 inode
    if let (Ok(em), Ok(mm)) = (std::fs::metadata(exe), std::fs::metadata(&managed)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if em.dev() == mm.dev() && em.ino() == mm.ino() {
                return true;
            }
        }
    }
    false
}

/// 等 smeltd 监听 socket 的上限。
///
/// 生产里 daemon 基本已常驻，5s 足够且不宜更久——GUI 卡在这里用户什么也做不了。
/// 测试二进制跑在自己的沙箱里，每个进程都要先把 debug 版 smeltd（数百 MB）复制进去
/// 再冷启动，5s 必然不够。放宽超时，而不是让用例回头去捞开发者真实安装的 daemon。
fn daemon_ready_timeout() -> Duration {
    if smelt_paths::running_under_test() {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(5)
    }
}

/// 查找分发物（smeltd、bundled 插件包）时应当参照的可执行文件位置。
///
/// cargo 把测试二进制放在 `target/<profile>/deps/`，而分发物都在上一级的 profile
/// 目录。不抹掉 `deps` 这一层，用例就只能去捞开发者真实安装的 `~/.smelt`，既污染
/// 个人数据，在 CI 上也必然抓瞎。
fn distribution_exe() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if smelt_paths::running_under_test()
        && let Some(profile_dir) = exe.parent().and_then(std::path::Path::parent)
    {
        return Some(profile_dir.join(exe.file_name()?));
    }
    Some(exe)
}

/// App 包 / cargo 同目录下的 smeltd（分发物，不是常驻运行路径）。
fn bundled_daemon_path() -> Option<std::path::PathBuf> {
    distribution_exe()
        .map(|e| e.with_file_name("smeltd"))
        .filter(|p| p.is_file())
}

fn bundled_plugin_root() -> Option<std::path::PathBuf> {
    bundled_plugin_root_for(&distribution_exe()?)
}

/// 已安装或不存在则现场 stage 一份 bundled 插件包，供设置页列出开关。
pub fn ensure_bundled_plugin_packages() -> Option<std::path::PathBuf> {
    bundled_plugin_root()
}

fn bundled_plugin_root_for(executable: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Some(root) = app_bundle_plugin_root(executable) {
        return Some(root);
    }
    stage_workspace_plugins(executable.parent()?).ok()
}

/// 已安装 bundled 插件包的根目录，只发现不重新 stage。
pub fn bundled_plugin_package_root() -> Option<std::path::PathBuf> {
    bundled_plugin_package_root_for(&distribution_exe()?)
}

/// GUI 侧统一发现应用自带与用户安装的插件。tab 注册表、设置页清单必须走同一条，
/// 否则会出现“设置里装上了，但 tab 看不见”这种双真相。
pub fn discover_installed_plugin_packages()
-> Vec<Result<smelt_plugin_host::PluginPackage, smelt_plugin_host::HostError>> {
    let Some(root) = smelt_paths::smelt_home() else {
        return vec![Err(smelt_plugin_host::HostError::new(
            "cannot determine home directory",
        ))];
    };
    let bundled = bundled_plugin_package_root().or_else(ensure_bundled_plugin_packages);
    smelt_plugin_host::discover_all_plugins(bundled.as_deref(), &root)
}

fn bundled_plugin_package_root_for(executable: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Some(root) = app_bundle_plugin_root(executable) {
        return root.is_dir().then_some(root);
    }
    let packages = executable.parent()?.join("plugin-packages");
    packages.is_dir().then_some(packages)
}

/// 把设置页的插件开关同步给守护：关掉则停进程，打开则拉起。
pub fn plugin_set_enabled(plugin_id: &str, enabled: bool) -> Result<(), String> {
    let Ok(mut stream) = connect_daemon_control() else {
        return Ok(());
    };
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::PluginSetEnabled,
            "plugin_id": plugin_id,
            "enabled": enabled,
            "auth": { "type": "first_party", "kind": "desktop" },
        })
    )
    .map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_str(response.trim()).unwrap_or_default();
    if value["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(value["error"]
            .as_str()
            .unwrap_or("设置插件开关失败")
            .to_string())
    }
}

/// 通知守护重新发现磁盘上的插件包。安装/卸载本身由 GUI 做，进程生命周期仍只归
/// 守护管理，避免同一个插件跑出两个实例。
pub fn plugin_reload() -> Result<(), String> {
    let mut stream = connect_daemon_control().map_err(|error| error.to_string())?;
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::PluginReload,
            "auth": { "type": "first_party", "kind": "desktop" },
        })
    )
    .map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|error| error.to_string())?;
    if value["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(value["error"]
            .as_str()
            .unwrap_or("重新加载插件失败")
            .to_string())
    }
}

/// 查询守护里各插件的运行状态。阻塞 IO，调用方放后台执行器。
pub fn plugin_statuses() -> Result<Vec<smelt_plugin_host::PluginStatus>, String> {
    let mut stream = connect_daemon_control().map_err(|error| error.to_string())?;
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::PluginStatuses,
            "auth": { "type": "first_party", "kind": "desktop" },
        })
    )
    .map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|error| error.to_string())?;
    if value["ok"].as_bool() != Some(true) {
        return Err(value["error"]
            .as_str()
            .unwrap_or("查询插件状态失败")
            .to_string());
    }
    serde_json::from_value(value["statuses"].clone()).map_err(|error| error.to_string())
}

/// 把一次面板 invocation 转给守护里的插件进程。
///
/// GUI 不自己拉起插件进程：那会和守护各管一份，同一个插件跑出两个实例。
/// 这是阻塞 IO，调用方必须放在后台执行器上。
pub fn plugin_invoke(
    plugin_id: &str,
    request: &smelt_plugin_api::InvocationRequest,
) -> Result<serde_json::Value, String> {
    let mut stream = connect_daemon_control().map_err(|error| error.to_string())?;
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::PluginInvoke,
            "plugin_id": plugin_id,
            "request": request,
            "auth": { "type": "first_party", "kind": "desktop" },
        })
    )
    .map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|error| error.to_string())?;
    if value["ok"].as_bool() != Some(true) {
        return Err(value["error"]
            .as_str()
            .unwrap_or("插件调用失败")
            .to_string());
    }
    // 守护把插件的应答原样回传：Success 取 result，Error 转成 Err。
    match &value["response"] {
        serde_json::Value::Object(map) if map.contains_key("result") => Ok(map["result"].clone()),
        serde_json::Value::Object(map) if map.contains_key("message") => Err(map["message"]
            .as_str()
            .unwrap_or("插件拒绝了这次调用")
            .to_string()),
        other => Err(format!("插件应答无法识别: {other}")),
    }
}

/// `.app` 内 first-party 插件包目录，相对 Contents。
///
/// 不能用 `PlugIns`：codesign 把那里的子目录当成嵌套 bundle，没有 Info.plist
/// 会直接 "bundle format unrecognized"。
const APP_BUNDLE_PLUGIN_PACKAGES: &str = "Resources/plugin-packages";

fn app_bundle_plugin_root(executable: &std::path::Path) -> Option<std::path::PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    Some(contents.join(APP_BUNDLE_PLUGIN_PACKAGES))
}

/// 开发模式下把 workspace 里的插件源码目录 stage 成插件包。
///
/// 扫盘而不是写死名单：新增一个插件只要在 `plugins/<name>/` 放好
/// `plugin.json` 与它声明的入口，就会自动出现在应用里——宿主不需要为此改任何代码。
/// Shared Bun 入口直接取 package 数据。
fn stage_workspace_plugins(bin_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dest = bin_dir.join("plugin-packages");
    // target/debug -> target -> workspace root
    let Some(sources) = bin_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map(|root| root.join("plugins"))
        .filter(|path| path.is_dir())
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "找不到 workspace 的 plugins 目录",
        ));
    };

    let mut staged = 0usize;
    for entry in std::fs::read_dir(&sources)? {
        let source = entry?.path();
        let manifest_path = source.join("plugin.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest_json = std::fs::read_to_string(&manifest_path)?;
        let Ok(manifest) = serde_json::from_str::<smelt_plugin_api::PluginManifest>(&manifest_json)
        else {
            eprintln!(
                "[plugin] 跳过无法解析的 manifest：{}",
                manifest_path.display()
            );
            continue;
        };
        if !manifest.bundled {
            // 只为测试存在的 package 不进产物，也不在开发期占一个运行时槽位。
            continue;
        }
        let entrypoint = source.join(&manifest.entrypoint);
        if !entrypoint.is_file() {
            continue;
        }
        // sidecar 和 web 目录都一并装进包里，声明与资源缺一不可。
        let web = source.join("web");
        let package_assets = source.join("assets");
        let ui_manifest = source.join(smelt_plugin_api::PLUGIN_UI_MANIFEST_FILE);
        let input_manifest = source.join(smelt_plugin_api::PLUGIN_INPUT_MANIFEST_FILE);
        let agent_manifest = source.join(smelt_plugin_api::PLUGIN_AGENT_MANIFEST_FILE);
        let mut assets: Vec<(&str, &std::path::Path)> = Vec::new();
        if web.is_dir() {
            assets.push(("web", web.as_path()));
        }
        if package_assets.is_dir() {
            assets.push(("assets", package_assets.as_path()));
        }
        if ui_manifest.is_file() {
            assets.push((
                smelt_plugin_api::PLUGIN_UI_MANIFEST_FILE,
                ui_manifest.as_path(),
            ));
        }
        if input_manifest.is_file() {
            assets.push((
                smelt_plugin_api::PLUGIN_INPUT_MANIFEST_FILE,
                input_manifest.as_path(),
            ));
        }
        if agent_manifest.is_file() {
            assets.push((
                smelt_plugin_api::PLUGIN_AGENT_MANIFEST_FILE,
                agent_manifest.as_path(),
            ));
        }
        match smelt_plugin_host::stage_plugin_package_with_assets(
            &dest,
            &manifest_json,
            &entrypoint,
            &assets,
        ) {
            Ok(_) => staged += 1,
            Err(error) => eprintln!("[plugin] stage {} 失败：{error}", manifest.id),
        }
    }

    if staged == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "没有可用的插件包（package entrypoint 缺失）",
        ));
    }
    Ok(dest)
}

fn sync_bundled_plugins(daemon: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let smelt_root = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    let source = bundled_plugin_root().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "找不到 bundled 插件包；请先构建 bundled plugins",
        )
    })?;
    if !source.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("bundled 插件目录不存在：{}", source.display()),
        ));
    }
    let dest = smelt_plugin_host::sync_bundled_plugin_set(Some(&source), &smelt_root, daemon)
        .map_err(std::io::Error::other)?;
    // 只对齐磁盘。这里绝不能 plugin_reload：冷启动 restore 紧接着 Open 终端，
    // 而 reload 会杀掉并重启 shared bun，控制通道只有 5s 超时，失败还被忽略。
    // 会话挂上之后由 workspace 再发 reload。
    Ok(dest)
}

fn prepare_bundled_release(
    app: &std::path::Path,
    smelt_root: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let daemon = app.join("Contents/MacOS/smeltd");
    if !daemon.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("候选 App 缺少守护程序：{}", daemon.display()),
        ));
    }
    let plugin_root = app.join("Contents").join(APP_BUNDLE_PLUGIN_PACKAGES);
    if !plugin_root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("候选 App 缺少插件目录：{}", plugin_root.display()),
        ));
    }
    smelt_plugin_host::sync_bundled_plugin_set(Some(&plugin_root), smelt_root, &daemon)
        .map_err(std::io::Error::other)?;
    Ok(daemon)
}

fn file_mtime_secs(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// 两份 smeltd 是否是同一份构建。
///
/// 只比较文件大小会漏掉“新旧构建恰好同大小”的升级；只比较 mtime 又会把
/// 安装时一次普通的 copy 误判成新版本。这里先做便宜的 metadata 检查，再逐块
/// 比较内容，兼顾正确性和后台探测成本。
fn same_daemon_binary(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let mut left = [0u8; 128 * 1024];
    let mut right = [0u8; 128 * 1024];
    loop {
        let (Ok(na), Ok(nb)) = (fa.read(&mut left), fb.read(&mut right)) else {
            return false;
        };
        if na != nb || left[..na] != right[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

/// 守护没在跑时把 `src` 装到正式路径 `~/.smelt/bin/smeltd`。
/// 先写 `smeltd.next` 再 rename，避免半截文件。
fn install_managed_daemon_from(src: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    let managed = managed_daemon_path();
    let need = !managed.is_file() || !same_daemon_binary(src, &managed);
    if need {
        let staged = staged_daemon_path();
        stage_daemon_binary(src, &staged)?;
        // Unix rename 会原子替换目标；先 remove 会制造一个路径不存在的窗口，
        // 此时若硬重启恰好发生，新守护将无文件可执行。
        std::fs::rename(&staged, &managed)?;
        eprintln!(
            "[workspace] 已同步守护 {} → {}",
            src.display(),
            managed.display()
        );
    }
    if managed.is_file() {
        Ok(managed)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("无法安装 smeltd 到 {}", managed.display()),
        ))
    }
}

/// 守护在跑时只把新映像写到 `smeltd.next`，绝不覆盖正在 exec 的 `smeltd`。
/// ACP host 从 `current_exe()` 派生，那条路径必须始终是运行中的映像。
fn stage_managed_daemon_update(src: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    let staged = staged_daemon_path();
    stage_daemon_binary(src, &staged)?;
    eprintln!(
        "[workspace] 已暂存守护更新 {} → {}",
        src.display(),
        staged.display()
    );
    Ok(staged)
}

fn pending_upgrade_exe() -> Option<std::path::PathBuf> {
    let staged = staged_daemon_path();
    if staged.is_file() {
        return Some(staged);
    }
    bundled_daemon_path()
}

fn stage_daemon_binary(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if same_daemon_path(src, dest) {
        return Ok(());
    }
    let _ = std::fs::remove_file(dest);
    std::fs::copy(src, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dest, perms)?;
    }
    Ok(())
}

/// 完成暂存映像到正式路径的安装。新版 smeltd 会在 handoff 启动最早期自行完成
/// `next -> smeltd` 并以正式路径继续 exec；旧版仍由 GUI 在确认升级后执行 rename。
fn finish_staged_managed_install(
    staged: &std::path::Path,
    managed: &std::path::Path,
) -> std::io::Result<()> {
    if staged == managed {
        return Ok(());
    }
    match std::fs::rename(staged, managed) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && managed.is_file() => Ok(()),
        Err(error) => Err(error),
    }
}

fn handoff_daemon_to_managed(src: &std::path::Path) -> UpgradeOutcome {
    handoff_daemon_to_managed_once(src)
}

/// App 安装不能在持有安装锁时无限等 ACP 回合；交给 UI 显示等待状态并在安全边界后重试。
fn try_handoff_daemon_to_managed(src: &std::path::Path) -> UpgradeOutcome {
    handoff_daemon_to_managed_once(src)
}

/// 把正在跑的守护用一次 handoff 迁到 `~/.smelt/bin/smeltd`（会话 PTY 保留）。
///
/// 若目标路径正是当前 running 的文件，先落到 `smeltd.next`。新映像确认自己已通过
/// exec 后会在启动任何线程前自行 rename，并以正式路径做一次轻量 exec；这一步不重复
/// 会话快照。旧映像不支持自行提升时，仍由本函数在确认 handoff 后完成 rename。
///
/// 一次尝试：Busy 原样返回，由 GUI 在回合结束后再发一次 upgrade。
fn handoff_daemon_to_managed_once(src: &std::path::Path) -> UpgradeOutcome {
    let managed = managed_daemon_path();
    let dir = managed_daemon_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[workspace] 创建 managed 目录失败：{e}");
        return UpgradeOutcome::Failed;
    }

    // 正在跑的是否已在 managed 路径上（覆盖同路径必须用 .next）。
    let running_on_managed = match probe_daemon_detail() {
        DaemonProbe::Running {
            exe_path: Some(ref p),
            ..
        } => exe_is_managed(std::path::Path::new(p)),
        _ => false,
    };

    let target = if running_on_managed {
        dir.join("smeltd.next")
    } else {
        managed.clone()
    };
    if let Err(e) = stage_daemon_binary(src, &target) {
        eprintln!("[workspace] 暂存守护失败（{}）：{e}", target.display());
        return UpgradeOutcome::Failed;
    }

    let outcome = upgrade_daemon_exe(Some(&target));
    match outcome {
        UpgradeOutcome::Upgraded => {
            if let Err(e) = finish_staged_managed_install(&target, &managed) {
                eprintln!(
                    "[workspace] 完成 managed 安装失败：{e}（目标：{}）",
                    managed.display()
                );
            }
            eprintln!("[workspace] 守护已迁入 managed：{}", managed.display());
            UpgradeOutcome::Upgraded
        }
        UpgradeOutcome::Busy => UpgradeOutcome::Busy,
        other => {
            // 失败时把候选留在 smeltd.next。不能扶正到正在跑的 smeltd：
            // ACP host 从那条路径派生，覆盖它就是混版本。
            eprintln!(
                "[workspace] handoff 到 managed 失败（{other:?}），候选：{}",
                target.display()
            );
            other
        }
    }
}

/// 多 pane 同时 connect 时串行化迁移，避免并发 upgrade 踩踏。
static MANAGED_DAEMON_GATE: Mutex<()> = Mutex::new(());

struct ManagedDaemonFileLock {
    _file: std::fs::File,
}

fn acquire_file_lock(path: &std::path::Path) -> std::io::Result<ManagedDaemonFileLock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(ManagedDaemonFileLock { _file: file })
}

/// 单次更新尝试不能排队等别的进程的 handoff；拿不到锁时交还给 UI 的可取消等待态。
fn try_acquire_file_lock(path: &std::path::Path) -> std::io::Result<Option<ManagedDaemonFileLock>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        };
    }
    Ok(Some(ManagedDaemonFileLock { _file: file }))
}

fn acquire_managed_daemon_file_lock() -> std::io::Result<ManagedDaemonFileLock> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    acquire_file_lock(&dir.join("smeltd.install.lock"))
}

fn try_acquire_managed_daemon_file_lock() -> std::io::Result<Option<ManagedDaemonFileLock>> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    try_acquire_file_lock(&dir.join("smeltd.install.lock"))
}

/// 确保守护跑在 `~/.smelt/bin/smeltd` 且不旧于 App 内分发物。
///
/// GUI 启动 / 连守护 / 装包后 **必须** 调用。返回 managed 路径（可能尚未有进程，
/// 但文件应已就位）。
pub fn ensure_managed_daemon_current() -> std::io::Result<std::path::PathBuf> {
    let _gate = MANAGED_DAEMON_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _file_gate = acquire_managed_daemon_file_lock()?;
    remember_managed_daemon_ensure(
        ensure_managed_daemon_current_locked(),
        &CONNECT_MANAGED_ENSURED,
    )
}

fn remember_managed_daemon_ensure<T>(
    result: std::io::Result<T>,
    ensured: &AtomicBool,
) -> std::io::Result<T> {
    if result.is_ok() {
        ensured.store(true, Ordering::Relaxed);
    }
    result
}

fn ensure_managed_daemon_for_connect<F>(
    ensured: &AtomicBool,
    managed: std::path::PathBuf,
    ensure: F,
) -> std::io::Result<std::path::PathBuf>
where
    F: FnOnce() -> std::io::Result<std::path::PathBuf>,
{
    // 这里只能缓存昂贵的版本/迁移检查，不能缓存文件存在性。安装、升级或异常
    // 重启可能在 GUI 生命周期内替换/移走该文件，命中缓存后仍须自愈。
    if ensured.load(Ordering::Relaxed) && managed.is_file() {
        Ok(managed)
    } else {
        remember_managed_daemon_ensure(ensure(), ensured)
    }
}

fn ensure_managed_daemon_current_locked() -> std::io::Result<std::path::PathBuf> {
    let bundled = bundled_daemon_path();
    let managed = managed_daemon_path();

    // 无分发物：仅依赖已有 managed（开发机偶发）。
    let Some(bundled) = bundled else {
        return if managed.is_file() {
            // 仍可能需要把「跑在 .app 里」的旧守护迁走
            let _ = migrate_running_daemon_off_app(&managed);
            Ok(managed)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "找不到 smeltd（App 包与 ~/.smelt/bin 皆无）",
            ))
        };
    };

    sync_bundled_plugins(&bundled)?;

    match probe_daemon_detail() {
        DaemonProbe::NotRunning => {
            install_managed_daemon_from(&bundled)?;
            Ok(managed_daemon_path())
        }
        DaemonProbe::Unresponsive => {
            // 连得上但无 version：进程可能还活着，只 stage 下一份，不覆盖正在跑的文件。
            let _ = stage_managed_daemon_update(&bundled);
            Ok(managed_daemon_path())
        }
        DaemonProbe::Running {
            exe_mtime: _,
            exe_path,
            ..
        } => {
            let on_managed = exe_path
                .as_ref()
                .map(|p| exe_is_managed(std::path::Path::new(p)))
                .unwrap_or(false);
            let inside_app = exe_path
                .as_ref()
                .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
                .unwrap_or(!on_managed); // 老守护无 exe：若已在 managed 目录则别瞎迁
            // 路径不对（仍在 .app / 非 managed）→ 必须迁。
            // 二进制升级：仅当「跑的比 App 旧」且**文件内容实质不同**——
            // 禁止仅因 cp 造成 mtime+1s 就 handoff（会清空 Term → 对话像被重初始化）。
            let content_differs = !managed.is_file() || !same_daemon_binary(&managed, &bundled);
            let must_relocate = inside_app || !on_managed;
            if must_relocate {
                // **安全关键**：守护还住在 .app 里 / 不在 managed 目录，随时可能被
                // Finder 拖 DMG 覆盖 App 时误杀，必须立即迁出——这条不能等空闲。
                eprintln!(
                    "[workspace] 迁移守护 → managed（inside_app={inside_app} on_managed={on_managed} exe={exe_path:?}）"
                );
                let outcome = handoff_daemon_to_managed(&bundled);
                if !matches!(outcome, UpgradeOutcome::Upgraded) {
                    let _ = stage_managed_daemon_update(&bundled);
                }
            } else if content_differs {
                // 守护已在 managed（含版本旧）：只把新映像写到 smeltd.next，**不
                // 覆盖正在跑的 smeltd，也不 exec**。连接路径偷偷换代正是「用着用着
                // 终端全卡」的根源。真正的换代交给 GUI 空闲升级或用户手动点升级。
                let _ = stage_managed_daemon_update(&bundled);
            }
            Ok(managed_daemon_path())
        }
    }
}

/// 守护已在跑但路径未知/在 App 内时，用已有 managed 文件尝试迁出。
fn migrate_running_daemon_off_app(managed: &std::path::Path) -> UpgradeOutcome {
    if !managed.is_file() {
        return UpgradeOutcome::Failed;
    }
    match probe_daemon_detail() {
        DaemonProbe::Running { exe_path, .. } => {
            let on_managed = exe_path
                .as_ref()
                .map(|p| exe_is_managed(std::path::Path::new(p)))
                .unwrap_or(false);
            let inside_app = exe_path
                .as_ref()
                .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
                .unwrap_or(!on_managed);
            if on_managed && !inside_app {
                return UpgradeOutcome::Upgraded;
            }
            handoff_daemon_to_managed(managed)
        }
        DaemonProbe::NotRunning | DaemonProbe::Unresponsive => UpgradeOutcome::Failed,
    }
}

/// 进程内 connect 路径是否已 ensure 过（多 pane 只付一次代价）。
/// 冷启动完整 ensure 由 `schedule_session_restore` 串行完成，不与此竞态升级。
static CONNECT_MANAGED_ENSURED: AtomicBool = AtomicBool::new(false);

/// 连接守护。进程内**首次**连上时 ensure 一次 managed 路径；之后只 connect。
/// 连不上则拉起 `~/.smelt/bin/smeltd`（独立进程组）再重试。
///
/// **这里绝不删 sock 文件。**（僵尸 sock 由 smeltd bind 方清理，见 smeltd。）
fn connect_daemon() -> std::io::Result<UnixStream> {
    let path = sock_path();

    // 已有守护：进程内只 ensure 一次。
    if UnixStream::connect(&path).is_ok() {
        if !CONNECT_MANAGED_ENSURED.load(Ordering::Relaxed) {
            let _ = ensure_managed_daemon_for_connect(
                &CONNECT_MANAGED_ENSURED,
                managed_daemon_path(),
                ensure_managed_daemon_current,
            );
        }
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
        // handoff 后短暂窗口：下面走拉起/轮询
    }

    // 优先 managed 路径；没有则从 App/cargo 同目录同步一份。
    let daemon = match ensure_managed_daemon_for_connect(
        &CONNECT_MANAGED_ENSURED,
        managed_daemon_path(),
        ensure_managed_daemon_current,
    ) {
        Ok(p) => p,
        Err(e) => {
            let exe = std::env::current_exe()?;
            let fallback = exe.with_file_name("smeltd");
            if fallback.is_file() {
                eprintln!(
                    "[plugin-host] managed daemon sync failed ({e}); falling back to {}",
                    fallback.display()
                );
                fallback
            } else {
                return Err(std::io::Error::new(e.kind(), format!("smeltd 不可用：{e}")));
            }
        }
    };

    // 二次确认：若 sock 已因 handoff 起来，直接连
    if let Ok(s) = UnixStream::connect(&path) {
        return Ok(s);
    }

    let spawn_err: Option<String> = {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let exe = std::env::current_exe().ok();
        let in_app_bundle = exe.as_ref().is_some_and(|e| path_inside_app_bundle(e));
        let mut cmd = std::process::Command::new(&daemon);
        smelt_paths::export_to(&mut cmd);
        smelt_core::tty_color::clear_command(&mut cmd);
        cmd.process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if in_app_bundle {
            cmd.env("SMELT_MENUBAR", "1");
        }
        // 把 spawn 目标的指纹传给子进程：它启动时直接钉死，不用再哈希磁盘——
        // 关闭“spawn 与守护启动哈希之间磁盘被替换”的竞态窗口（与 handoff 经
        // SMELTD_PLUGIN_DAEMON_FINGERPRINT 传指纹同一机制）。
        if let Ok(fingerprint) = smelt_plugin_host::executable_fingerprint(&daemon) {
            cmd.env("SMELTD_PLUGIN_DAEMON_FINGERPRINT", fingerprint);
        }
        match cmd.spawn() {
            Ok(child) => {
                eprintln!(
                    "[workspace] 已拉起 smeltd pid={} ({}) menubar={}",
                    child.id(),
                    daemon.display(),
                    in_app_bundle
                );
                None
            }
            Err(e) => Some(format!("拉起 smeltd 失败（{}）：{e}", daemon.display())),
        }
    };

    let timeout = daemon_ready_timeout();
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(&path) {
            return Ok(s);
        }
    }
    let secs = timeout.as_secs();
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        match spawn_err {
            Some(why) => format!("smeltd 未就绪：{why}；{secs}s 内未监听 {}", path.display()),
            None => format!(
                "smeltd 未就绪（已拉起 {}，{secs}s 内未监听 {}）",
                daemon.display(),
                path.display()
            ),
        },
    ))
}

/// 探测正在跑的守护：连不上（守护没起，`connect_daemon` 会自己拉起磁盘上最新的）
/// 判 `NotRunning`；连上了但读不出合法的 "version" 响应——老到连这个探测本身都不
/// 认识——判 `Unresponsive`；否则带 mtime + 可选 exe 路径。
enum DaemonProbe {
    NotRunning,
    Unresponsive,
    Running {
        exe_mtime: u64,
        /// 守护自报的 current_exe；老守护无此字段则为 None。
        exe_path: Option<String>,
        /// 守护启动时钉死的进程指纹；老守护无此字段则为 None。
        daemon_fingerprint: Option<String>,
    },
}

fn probe_daemon() -> DaemonProbe {
    probe_daemon_detail()
}

/// 面向守护的短控制请求共用时限。它们全都应在后台运行，但超时仍必须存在：守护
/// 某个连接线程失活时，不能无限占住 executor worker，也不能让重试任务无限堆积。
const DAEMON_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

fn connect_daemon_control() -> std::io::Result<UnixStream> {
    let s = UnixStream::connect(sock_path())?;
    s.set_read_timeout(Some(DAEMON_CONTROL_TIMEOUT))?;
    s.set_write_timeout(Some(DAEMON_CONTROL_TIMEOUT))?;
    Ok(s)
}

fn probe_daemon_detail() -> DaemonProbe {
    let Ok(mut s) = connect_daemon_control() else {
        return DaemonProbe::NotRunning;
    };
    let parsed = (|| -> Option<(u64, Option<String>, Option<String>)> {
        writeln!(
            s,
            "{}",
            serde_json::json!({ "op": DaemonOperation::Version })
        )
        .ok()?;
        let mut resp = String::new();
        BufReader::new(s).read_line(&mut resp).ok()?;
        let v: serde_json::Value = serde_json::from_str(resp.trim()).ok()?;
        let mtime = v["exe_mtime"].as_u64()?;
        let exe = v["exe"].as_str().map(str::to_string);
        let fingerprint = v["daemon_fingerprint"].as_str().map(str::to_string);
        Some((mtime, exe, fingerprint))
    })();
    match parsed {
        Some((m, exe, fingerprint)) => DaemonProbe::Running {
            exe_mtime: m,
            exe_path: exe,
            daemon_fingerprint: fingerprint,
        },
        None => DaemonProbe::Unresponsive,
    }
}

/// 守护自报的运行信息，设置页展示用。
///
/// 字段全是 `Option`：`pid`/`started_at`/`session_count` 是后加的，老守护（还没换代的
/// 那个进程）只回 `version`/`exe_mtime`，读不到就显示不出来，不能因此判失败。
#[derive(Clone, Debug, Default)]
pub struct DaemonInfo {
    pub version: Option<String>,
    pub pid: Option<u32>,
    /// 守护进程启动时刻（unix 秒）。无缝升级 exec 后会重置，见 `smeltd::session_state`。
    pub started_at: Option<u64>,
    pub session_count: Option<u64>,
}

/// 查正在跑的守护的运行信息；守护没起 / 不响应 → None。
///
/// **阻塞 IO，只能在后台跑**（见 main.rs::refresh_daemon_status）。故意不走
/// `connect_daemon`：那个连不上会顺手拉起守护，而这里只是"看一眼现状"，
/// 不该有副作用——没起就是没起。
pub fn daemon_info() -> Option<DaemonInfo> {
    let mut s = connect_daemon_control().ok()?;
    writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::Version })
    )
    .ok()?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).ok()?;
    let v: serde_json::Value = serde_json::from_str(&resp).ok()?;
    Some(DaemonInfo {
        version: v["version"].as_str().map(str::to_string),
        pid: v["pid"].as_u64().map(|n| n as u32),
        started_at: v["started_at"].as_u64(),
        session_count: v["session_count"].as_u64(),
    })
}

/// 磁盘上「期望」的 smeltd mtime：优先 App/cargo 同目录分发物，其次 managed。
/// 用于判断正在跑的守护是否落后（装 DMG 后 App 更新了，但 ~/.smelt/bin 还旧）。
fn disk_smeltd_mtime() -> Option<u64> {
    bundled_daemon_path()
        .and_then(|p| file_mtime_secs(&p))
        .or_else(|| file_mtime_secs(&managed_daemon_path()))
}

/// 守护是否落后于磁盘上的 smeltd 二进制（重装/重编译后常见：旧守护不会自动重启，
/// 新代码要等手动重启守护才生效）。守护没起 → false（没什么可重启的，交给
/// `connect_daemon` 按需拉起最新的）；守护活着但连 "version" op 都不认识 → true
/// （老到必然过期）；查得到 mtime 但磁盘那份查不到 → false（避免误报打扰用户）。
pub fn daemon_outdated() -> bool {
    match probe_daemon() {
        DaemonProbe::NotRunning => false,
        DaemonProbe::Unresponsive => true,
        DaemonProbe::Running {
            exe_mtime,
            exe_path,
            daemon_fingerprint,
        } => daemon_outdated_from_probe(
            exe_mtime,
            exe_path.as_deref(),
            daemon_fingerprint.as_deref(),
        ),
    }
}

/// outdated 纯判定（可单测）：指纹主键 + 老守护回退。
///
/// 主键是指纹：守护自报启动时钉死的进程指纹，与磁盘期望二进制（bundled 优先，
/// 其次 managed）的指纹比。内容不同=旧，不受“cp 造成 mtime+1s”误判，也不被
/// StageDiskOnly 骗（磁盘新了、进程指纹还是老的——之前 `same_daemon_binary(
/// running_exe_path, bundled)` 比的是两个磁盘新文件，直接误判“不旧”，升级永不触发）。
fn daemon_outdated_from_probe(
    exe_mtime: u64,
    exe_path: Option<&str>,
    daemon_fingerprint: Option<&str>,
) -> bool {
    // 仍住在 .app 内 / 不在 managed = 必须迁（装 DMG 会死）
    let inside_app = exe_path
        .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
        .unwrap_or(false);
    let on_managed = exe_path
        .map(|p| exe_is_managed(std::path::Path::new(p)))
        .unwrap_or(false);
    if inside_app || !on_managed {
        return true;
    }
    // 主键：钉死指纹 vs 磁盘期望。
    if let Some(pinned) = daemon_fingerprint {
        let expected = bundled_daemon_path().or_else(|| {
            let managed = managed_daemon_path();
            managed.is_file().then_some(managed)
        });
        return outdated_by_fingerprint(pinned, expected.as_deref());
    }
    // 老守护无指纹：退回旧 heuristic（exe 路径内容比对 + mtime）。
    // 注意这条在 StageDiskOnly 后会误判“不旧”，但老守护熬过一次升级就换成新
    // 守护了——一次性窗口，可接受。
    if let (Some(exe), Some(bundled)) = (exe_path, bundled_daemon_path()) {
        let running_exe = std::path::Path::new(exe);
        let path_was_renamed =
            !running_exe.is_file() && same_daemon_binary(&managed_daemon_path(), &bundled);
        if same_daemon_binary(running_exe, &bundled) || path_was_renamed {
            return false;
        }
    }
    disk_smeltd_mtime().is_some_and(|disk| disk > exe_mtime)
}

/// 指纹主键判定（纯函数，可单测）：内容不同=旧。期望文件缺失/读不出→保守不旧，
/// 避免误报打扰用户；下一轮（或 headless 自升级）再看。
fn outdated_by_fingerprint(pinned: &str, expected: Option<&std::path::Path>) -> bool {
    let Some(path) = expected else {
        return false;
    };
    match smelt_plugin_host::executable_fingerprint(path) {
        Ok(disk) => disk != pinned,
        Err(_) => false,
    }
}

/// 新守护是否已经用 version 握手接替了旧进程。pid / started_at 在 exec 后都会变。
fn is_successor_daemon(before: Option<&DaemonInfo>, now: Option<&DaemonInfo>) -> bool {
    let Some(now) = now else {
        return false;
    };
    let Some(before) = before else {
        return true;
    };
    match (before.pid, now.pid) {
        (Some(old), Some(new)) if old != new => return true,
        _ => {}
    }
    matches!(
        (before.started_at, now.started_at),
        (Some(old), Some(new)) if old != new
    )
}

fn wait_for_successor_daemon(before: Option<DaemonInfo>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_successor_daemon(before.as_ref(), daemon_info().as_ref()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// 无缝升级的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// 交接完成，会话全部保留；或守护本来没跑、直接拉起了最新版。
    Upgraded,
    /// 正在跑的守护太旧，不认识 "upgrade" op（静默断连），只能走硬重启。
    Unsupported,
    /// ACP 仍有未完成 RPC；守护未执行 exec，现有连接完全未受影响。
    Busy,
    /// 守护接了单但升级没生效（exec 失败等），版本还是旧的。
    Failed,
}

/// 单次 App 安装尝试的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppInstallOutcome {
    Installed,
    /// 当前 ACP 回合或另一条 handoff 正在占用安全边界；App 包尚未替换。
    WaitingForSafeHandoff,
    /// 暂存包在最终校验时已失效并被 updater 作废，需要重新下载。
    UpdateInvalidated,
}

/// 无缝升级守护：发 "upgrade" op，守护 exec 磁盘上的新二进制、PTY fd 原地交接，
/// **所有会话不中断**（协议与流程见 smeltd 升级设计）。调用方在成功后应对每个
/// 终端调 reconnect()——会话 id 都还在，走的是正常 reattach + 重放恢复。
///
/// `read_line` 前设了读超时：守护万一卡住（比如某个 out 锁被冻结客户端占住），不能
/// 让这次调用永久挂起——上层 `daemon_upgrading` 标志会跟着卡死，整个功能失效。
pub fn upgrade_daemon() -> UpgradeOutcome {
    upgrade_daemon_exe(pending_upgrade_exe().as_deref())
}

/// 无缝升级守护。`new_exe` 为 `Some` 时让守护 **exec 指定路径**（装 DMG：先 exec
/// 暂存包里的 smeltd，会话不丢，再替换 .app）；`None` 则 exec `current_exe`。
pub fn upgrade_daemon_exe(new_exe: Option<&std::path::Path>) -> UpgradeOutcome {
    let predecessor = daemon_info();
    let Ok(mut s) = UnixStream::connect(sock_path()) else {
        // 守护没跑：拉起磁盘上最新的等于升级完成，但要探测确认它真的起来了再报
        // 成功——ensure_daemon_running 的失败是静默的，不确认就报 Upgraded 会让
        // UI 显示"已升级"而守护其实没起来。
        ensure_daemon_running();
        return if matches!(probe_daemon(), DaemonProbe::Running { .. }) {
            UpgradeOutcome::Upgraded
        } else {
            UpgradeOutcome::Failed
        };
    };
    let msg = match new_exe {
        Some(p) => serde_json::json!({
            "op": DaemonOperation::Upgrade,
            "exe": p.to_string_lossy()
        }),
        None => serde_json::json!({ "op": DaemonOperation::Upgrade }),
    };
    if writeln!(s, "{msg}").is_err() {
        return UpgradeOutcome::Failed;
    }
    // 守护 upgrade 在回 ok 前要逐会话拿 out 锁；每个冻结 client 最多卡
    // CLIENT_WRITE_TIMEOUT(3s)。会话多时 5s 不够——会误报 Failed 而守护稍后仍 exec。
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    // 三种读结果分开判断，不能混为一谈：
    // - Ok(0)（EOF，没读到任何字节）＝老守护完全不认识这个 op，直接断连 → Unsupported；
    // - Err（超时/IO 错误）＝守护接了但迟迟不回（可能卡住），不代表版本问题 → Failed；
    // - Ok(n>0) 但解析不出 JSON，或解析出来 ok!=true（比如 current_exe/写交接文件
    //   失败的显式回执）＝守护是新版本、只是这次没成功，同样是 Failed，不能引导
    //   用户去做「版本过旧只能硬重启」这种更破坏性的操作。
    let mut resp = String::new();
    match BufReader::new(s).read_line(&mut resp) {
        Ok(0) => return UpgradeOutcome::Unsupported,
        Ok(_) => {}
        Err(_) => return UpgradeOutcome::Failed,
    }
    let parsed = serde_json::from_str::<serde_json::Value>(resp.trim()).ok();
    if parsed
        .as_ref()
        .is_some_and(|v| v["busy"].as_bool() == Some(true))
    {
        return UpgradeOutcome::Busy;
    }
    let acked = parsed.is_some_and(|v| v["ok"].as_bool() == Some(true));
    if !acked {
        return UpgradeOutcome::Failed;
    }
    if wait_for_successor_daemon(predecessor) {
        UpgradeOutcome::Upgraded
    } else {
        UpgradeOutcome::Failed
    }
}

/// 候选 `.app` 安装的公共前半段：先把 bundled 插件目录绑到该 smeltd 指纹，
/// 再把守护交到 `~/.smelt/bin`。在线更新、DMG 后的 GUI ensure、`make install`
/// 都必须是这个顺序；先 exec 再写映射会让插件永远起不来。
/// 拷贝不跑当前版插件 schema：新字段由新守护 load。
fn prepare_candidate_daemon(candidate_app: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let smelt_root = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    prepare_bundled_release(candidate_app, &smelt_root).map_err(anyhow::Error::from)
}

/// 安装时对运行中守护的处置决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallCommitAction {
    /// 守护已在 managed 路径：只把候选二进制写到 `smeltd.next`，不覆盖正在跑的
    /// `smeltd`，不 handoff。版本升级交给空闲无缝升级；安装永不因此阻塞。
    StageDiskOnly,
    /// 守护仍在 .app 里（或路径未知）：必须 handoff 迁出，否则换 App 会 SIGKILL 它。
    RelocateHandoff,
}

/// 安装处置决策（纯函数，可单测）：managed 不变量已成立就只 stage 磁盘。
///
/// 历史上这里无条件 handoff，把"换 App"和"换守护版本"绑成一次原子事务——
/// ACP 忙就 exit 75 半安装（映射写了、App 没换）。解耦后安装只保证 managed
/// 不变量；版本升级由空闲无缝升级接管（restore 后检查 + 60s watch + 回合结束
/// flush），skew 窗口的新 GUI + 老守护由启动路径的 op 容忍覆盖。
fn install_commit_action_for_running_daemon(
    exe_path: Option<&std::path::Path>,
) -> InstallCommitAction {
    let managed = exe_path.map(exe_is_managed).unwrap_or(false);
    // 老守护无 exe 字段：无法证明已迁出，保守迁出（一次性迁移，此后永久跳过）。
    let inside_app = exe_path.map(path_inside_app_bundle).unwrap_or(true);
    if managed && !inside_app {
        InstallCommitAction::StageDiskOnly
    } else {
        InstallCommitAction::RelocateHandoff
    }
}

/// 安装提交：守护没在跑就只落盘 managed；在跑且已在 managed 路径同样只 stage
/// 磁盘（版本升级交给空闲无缝升级，安装不等 ACP）；只在守护仍住在 .app 里时
/// 才强制 handoff 迁出——此时 ACP 忙仍返回 WaitingForSafeHandoff、不替换 App。
fn commit_candidate_daemon(
    candidate_smeltd: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    match probe_daemon_detail() {
        DaemonProbe::NotRunning | DaemonProbe::Unresponsive => {
            install_managed_daemon_from(candidate_smeltd)?;
            Ok(AppInstallOutcome::Installed)
        }
        DaemonProbe::Running { exe_path, .. } => {
            let exe = exe_path.as_deref().map(std::path::Path::new);
            match install_commit_action_for_running_daemon(exe) {
                InstallCommitAction::StageDiskOnly => {
                    stage_managed_daemon_update(candidate_smeltd)?;
                    Ok(AppInstallOutcome::Installed)
                }
                InstallCommitAction::RelocateHandoff => {
                    match try_handoff_daemon_to_managed(candidate_smeltd) {
                        UpgradeOutcome::Upgraded => Ok(AppInstallOutcome::Installed),
                        UpgradeOutcome::Busy => Ok(AppInstallOutcome::WaitingForSafeHandoff),
                        UpgradeOutcome::Unsupported | UpgradeOutcome::Failed => {
                            anyhow::bail!(
                                "装包前 handoff→managed 未成功，已停止替换 App 以保护现有会话"
                            )
                        }
                    }
                }
            }
        }
    }
}

fn prepare_and_commit_candidate_app(
    candidate_app: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    let candidate_smeltd = prepare_candidate_daemon(candidate_app)?;
    commit_candidate_daemon(&candidate_smeltd)
}

/// 装新版 `.app`（在线更新）时保留 smeltd 会话：
/// 1. updater 先把候选包复制到正式 App 同卷并完成签名/指纹校验
/// 2. 再把候选包里的 smeltd handoff 到 `~/.smelt/bin/smeltd`（离开即将被删的 .app）
/// 3. 再替换 `/Applications/Smelt.app`
/// 4. 再 ensure managed 与新 App 内 smeltd 对齐
///
/// 顺序绝不能反：若先整包覆盖，App 内 smeltd 会被 SIGKILL → 会话全灭 → 对话「重新初始化」。
pub fn install_app_preserving_sessions(
    update: &crate::updater::StagedUpdate,
) -> anyhow::Result<AppInstallOutcome> {
    let _gate = match MANAGED_DAEMON_GATE.try_lock() {
        Ok(gate) => gate,
        Err(TryLockError::WouldBlock) => return Ok(AppInstallOutcome::WaitingForSafeHandoff),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let Some(_file_gate) = try_acquire_managed_daemon_file_lock()? else {
        return Ok(AppInstallOutcome::WaitingForSafeHandoff);
    };

    let finalized = crate::updater::finalize_pending_update(update, |candidate_app| {
        match prepare_and_commit_candidate_app(candidate_app)? {
            AppInstallOutcome::Installed => Ok(crate::updater::InstallPreparation::Proceed),
            AppInstallOutcome::WaitingForSafeHandoff => {
                Ok(crate::updater::InstallPreparation::RetryLater)
            }
            AppInstallOutcome::UpdateInvalidated => {
                anyhow::bail!("候选包在守护交接前已失效")
            }
        }
    })?;
    match finalized {
        crate::updater::FinalizeOutcome::Installed => {}
        crate::updater::FinalizeOutcome::RetryLater => {
            return Ok(AppInstallOutcome::WaitingForSafeHandoff);
        }
        crate::updater::FinalizeOutcome::Invalidated => {
            return Ok(AppInstallOutcome::UpdateInvalidated);
        }
    }

    // 新包落盘后：managed 与 App 内 smeltd 对齐
    if let Ok(app) = crate::updater::current_app_bundle_path() {
        let app_smeltd = app.join("Contents/MacOS/smeltd");
        if app_smeltd.is_file() {
            match remember_managed_daemon_ensure(
                ensure_managed_daemon_current_locked(),
                &CONNECT_MANAGED_ENSURED,
            ) {
                Ok(p) => eprintln!("[workspace] 装包后 managed 守护：{}", p.display()),
                Err(e) => eprintln!("[workspace] 装包后同步 managed 失败：{e}"),
            }
        }
    }
    Ok(AppInstallOutcome::Installed)
}

/// 本地 `make install` 入口：与在线更新同一套「插件映射 → 守护交接 → 换 App」。
pub fn install_local_app_bundle(
    source: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    let _gate = match MANAGED_DAEMON_GATE.try_lock() {
        Ok(gate) => gate,
        Err(TryLockError::WouldBlock) => return Ok(AppInstallOutcome::WaitingForSafeHandoff),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let Some(_file_gate) = try_acquire_managed_daemon_file_lock()? else {
        return Ok(AppInstallOutcome::WaitingForSafeHandoff);
    };

    match prepare_and_commit_candidate_app(source)? {
        AppInstallOutcome::Installed => {}
        other => return Ok(other),
    }

    atomic_install_app_bundle(source, target)?;
    sync_managed_helpers_from_app(source)?;
    Ok(AppInstallOutcome::Installed)
}

/// `smelt --install-app <src.app> [dst.app]`：给 Makefile 用，避免再写一套 shell 交接。
pub fn maybe_run_install_app<I, S>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    if args.get(1).map(String::as_str) != Some("--install-app") {
        return None;
    }
    let Some(source) = args.get(2).map(std::path::PathBuf::from) else {
        eprintln!("用法：smelt --install-app <Smelt.app> [/Applications/Smelt.app]");
        return Some(2);
    };
    let target = args
        .get(3)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/Applications/Smelt.app"));
    match install_local_app_bundle(&source, &target) {
        Ok(AppInstallOutcome::Installed) => {
            eprintln!(
                "✅ 已安装 {}（守护交接与插件映射与在线更新相同）",
                target.display()
            );
            Some(0)
        }
        Ok(AppInstallOutcome::WaitingForSafeHandoff) => {
            eprintln!(
                "⏸ ACP 会话仍在运行，未替换 App（与在线更新一致）。回合结束后再 make install。"
            );
            Some(75)
        }
        Ok(AppInstallOutcome::UpdateInvalidated) => {
            eprintln!("✗ 候选包已失效");
            Some(1)
        }
        Err(error) => {
            eprintln!("✗ 安装失败：{error:#}");
            Some(1)
        }
    }
}

fn atomic_install_app_bundle(
    source: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    let script = std::env::current_dir()
        .map_err(anyhow::Error::from)?
        .join("scripts/install-mac-app-atomically.sh");
    if !script.is_file() {
        anyhow::bail!(
            "找不到 {}（请在仓库根目录执行 make install）",
            script.display()
        );
    }
    let status = std::process::Command::new(&script)
        .arg(source)
        .arg(target)
        .status()
        .map_err(anyhow::Error::from)?;
    if !status.success() {
        anyhow::bail!("原子安装 App 失败，退出码 {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn sync_managed_helpers_from_app(app: &std::path::Path) -> std::io::Result<()> {
    let macos = app.join("Contents/MacOS");
    let dest_dir = managed_daemon_dir();
    std::fs::create_dir_all(&dest_dir)?;
    for name in ["smelt-agent-mcp", "smelt-notify"] {
        let src = macos.join(name);
        if !src.is_file() {
            continue;
        }
        let staged = dest_dir.join(format!("{name}.next"));
        let dest = dest_dir.join(name);
        let _ = std::fs::remove_file(&staged);
        std::fs::copy(&src, &staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&staged)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&staged, permissions)?;
        }
        std::fs::rename(&staged, dest)?;
    }
    Ok(())
}

/// 让守护自己退出。**会杀掉它托管的所有 PTY 会话**（进程一死子进程 EOF/SIGHUP），
/// 调用前必须先让用户明确知情确认。退出后调 ensure_daemon_running() 拉起磁盘上
/// 最新的 smeltd 二进制。
///
/// **必须在后台线程调用**：内部有 sleep / 可能跑 lsof，禁止在 UI 线程直接跑。
///
/// "shutdown" op 本身也是新加的——老到连它都不认识的守护会照单全收地忽略这条消息，
/// 连接照旧开着，优雅关闭形同没发生。等一小段时间探测它是否真的死了，没死就按
/// 监听 socket 的进程直接 SIGKILL，这条路径不依赖守护认不认识任何协议。
pub fn restart_daemon() {
    let path = sock_path();
    let daemon_pid = daemon_pid_with_timeout(&path);
    // 硬重启后必须重新验证托管文件；此前的成功缓存不能跨 daemon 生命周期。
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
    if let Ok(s) = UnixStream::connect(&path) {
        // 无超时 read_line 会在守护卡死/不回包时永久挂起（设置页「重启守护」假死根因）。
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
        let mut s = s;
        let _ = writeln!(
            s,
            "{}",
            serde_json::json!({ "op": DaemonOperation::Shutdown })
        );
        let mut resp = String::new();
        let _ = BufReader::new(s).read_line(&mut resp);
    }
    // 等守护退出（最多 ~1s）；socket 仍能连上再强杀，**不要**提前 return 漏掉残留 sock。
    for _ in 0..10 {
        if UnixStream::connect(&path).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    // shutdown 会先回包再清理 sidecar；若清理卡住，另一个启动者可能已经删掉
    // socket 目录项，此时 lsof(path) 再也找不到旧守护。按重启前记录的 PID 兜底。
    if let Some(pid) = daemon_pid {
        if process_is_alive(pid) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            for _ in 0..10 {
                if !process_is_alive(pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    } else if UnixStream::connect(&path).is_ok() {
        // 老守护的 version 响应没有 pid，只能保留按 socket 反查的兼容兜底。
        force_kill_socket_owner(&path);
        thread::sleep(Duration::from_millis(150));
    }
    // 残留 sock 文件会让下一次 connect 误判/卡住；尽量清掉。
    if UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
}

fn daemon_pid_with_timeout(path: &std::path::Path) -> Option<u32> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(500))).ok()?;
    writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::Version })
    )
    .ok()?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).ok()?;
    let value: serde_json::Value = serde_json::from_str(resp.trim()).ok()?;
    value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 兜底：找到正监听着 `path` 的进程并 SIGKILL，再清掉残留 socket 文件。用于优雅
/// shutdown 对老守护不生效的情况——`lsof -t` 直接按 socket 文件反查 pid，不经过
/// 应用层协议，多老的守护都杀得掉。
///
/// `lsof` 在 macOS 上偶发长时间无响应：用 spawn + 轮询 wait，超时就 kill 掉 lsof，
/// 避免「重启守护」整条链路跟着卡死。
fn force_kill_socket_owner(path: &std::path::Path) {
    let mut child = match std::process::Command::new("lsof")
        .arg("-t")
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(path);
            return;
        }
    };
    // 最多等 ~2s
    let mut done = false;
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(_)) => {
                done = true;
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    if !done {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(out) = child.wait_with_output() {
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid)
                .status();
        }
    }
    let _ = std::fs::remove_file(path);
}

/// 确保守护活着，没有就拉起来（复用 connect_daemon 的探测+拉起+轮询逻辑）。
/// 调用方（重启守护后想立刻刷新状态）负责扔到后台线程，避免卡 UI（最坏等 5s）。
pub fn ensure_daemon_running() {
    let _ = connect_daemon();
}

/// 让守护杀掉某会话（用户主动关 pane 时调用；GUI 退出不调 → 会话持久活着）。
pub fn kill_remote(id: &str) {
    let Ok(mut s) = connect_daemon_control() else {
        return;
    };
    let _ = writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::Kill, "id": id })
    );
    // 等守护回执，确保 kill 落地后再继续（避免关 pane 后立刻退出时丢命令）。
    let mut resp = String::new();
    let _ = BufReader::new(s).read_line(&mut resp);
}

/// 内嵌远程网关（见 smeltd「内嵌远程网关」一节）的最小运行状态。
#[derive(Clone, Debug, Default)]
pub struct RemoteStatus {
    pub running: bool,
    pub token: Option<String>,
}

/// 让守护开启内嵌远程网关（幂等：已经开着直接回现状原 token/write，不重启不换
/// token——`write` 传入值在这种情况下会被忽略，见 smeltd `start_remote_gateway`）。
/// 绑定非法地址/端口绑不上时把守护回的错误原样透传出去。
pub fn remote_start(bind: &str, write: bool) -> Result<RemoteStatus, String> {
    let Ok(mut s) = connect_daemon_control() else {
        return Err("连不上守护".to_string());
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::RemoteStart,
            "bind": bind,
            "write": write
        })
    )
    .is_err()
    {
        return Err("发送请求失败".to_string());
    }
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return Err("守护没有响应".to_string());
    }
    let v: serde_json::Value = serde_json::from_str(resp.trim()).map_err(|e| e.to_string())?;
    if v["ok"].as_bool() == Some(true) {
        Ok(RemoteStatus {
            running: true,
            token: v["token"].as_str().map(String::from),
        })
    } else {
        Err(v["err"].as_str().unwrap_or("未知错误").to_string())
    }
}

/// 关掉内嵌远程网关。
pub fn remote_stop() {
    let Ok(mut s) = connect_daemon_control() else {
        return;
    };
    let _ = writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::RemoteStop })
    );
    let mut resp = String::new();
    let _ = BufReader::new(s).read_line(&mut resp);
}

/// 热更新内嵌远程网关的 ACP/终端写权限，不断开已经建立的 WebSocket。
pub fn remote_set_write(write: bool) -> Result<(), String> {
    let Ok(mut s) = connect_daemon_control() else {
        return Err("连不上守护".to_string());
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::RemoteSetWrite, "write": write })
    )
    .is_err()
    {
        return Err("发送请求失败".to_string());
    }
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return Err("守护没有响应".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(resp.trim()).map_err(|e| e.to_string())?;
    if value["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(value["err"]
            .as_str()
            .unwrap_or("更新远程写权限失败")
            .to_string())
    }
}

/// 显式轮换持久化的远程配对 Token。守护会先停止 iroh 和本机网关，保证旧配对
/// 立即失效；调用方成功后需按当前配置重新启动两者。
pub fn remote_rotate_token() -> Result<(), String> {
    let Ok(mut s) = connect_daemon_control() else {
        return Err("连不上守护".to_string());
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::RemoteRotateToken })
    )
    .is_err()
    {
        return Err("发送请求失败".to_string());
    }
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return Err("守护没有响应".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_str(resp.trim()).map_err(|error| error.to_string())?;
    if value["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(value["err"]
            .as_str()
            .unwrap_or("刷新远程配对 Token 失败")
            .to_string())
    }
}

/// 查当前内嵌远程网关的状态——GUI 刚启动时用它对齐"设置里记的开关"和"守护实际
/// 是不是真开着"（比如上次异常退出、守护单独重启过）。
pub fn remote_status() -> RemoteStatus {
    let Ok(mut s) = connect_daemon_control() else {
        return RemoteStatus::default();
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::RemoteStatus })
    )
    .is_err()
    {
        return RemoteStatus::default();
    }
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return RemoteStatus::default();
    }
    let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap_or_default();
    RemoteStatus {
        running: v["running"].as_bool().unwrap_or(false),
        token: v["token"].as_str().map(String::from),
    }
}

/// iroh 隧道（见 smeltd「iroh 隧道」一节）的运行状态。
///
/// `endpoint_id` **重启不变**，所以基于它生成的配对二维码可以一次扫、长期用。
#[derive(Clone, Debug, Default)]
pub struct IrohStatus {
    pub endpoint_id: Option<String>,
    /// 网关 token。`endpoint_id` 只让人连得上，能不能操作仍由它决定，
    /// 所以配对码必须两者一起给。
    pub token: Option<String>,
    pub relay: Option<String>,
    pub write: bool,
}

/// 让守护开启 iroh 隧道（幂等）。绑定要连接用户配置的 relay，**可能耗时数秒**，
/// 跟 `tunnel_start` 一样必须扔进后台任务，别在 UI 线程同步调。
pub fn iroh_start(write: bool, relay: &str) -> Result<IrohStatus, String> {
    let Ok(mut s) = connect_daemon_control() else {
        return Err("连不上守护".to_string());
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({
            "op": DaemonOperation::IrohStart, "write": write,
            "relay": relay
        })
    )
    .is_err()
    {
        return Err("发送请求失败".to_string());
    }
    // 守护那边就绪超时 30s，这里留余量。
    let _ = s.set_read_timeout(Some(Duration::from_secs(35)));
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return Err("守护没有响应（等 iroh 绑定超时）".to_string());
    }
    let v: serde_json::Value = serde_json::from_str(resp.trim()).map_err(|e| e.to_string())?;
    if v["ok"].as_bool() == Some(true) {
        Ok(IrohStatus {
            endpoint_id: v["endpoint_id"].as_str().map(String::from),
            token: v["token"].as_str().map(String::from),
            relay: v["relay"].as_str().map(String::from),
            write: v["write"].as_bool().unwrap_or(false),
        })
    } else {
        Err(v["err"].as_str().unwrap_or("未知错误").to_string())
    }
}

/// 关掉 iroh 隧道（不影响本机远程网关本身）。
pub fn iroh_stop() {
    let Ok(mut s) = connect_daemon_control() else {
        return;
    };
    let _ = writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::IrohStop })
    );
    let mut resp = String::new();
    let _ = BufReader::new(s).read_line(&mut resp);
}

/// 查 iroh 隧道当前是否真的在守护里跑着。
///
/// 存在的理由：GUI 里的 `IrohRuntimeState` 只是一次 `iroh_start` 的结果快照，
/// 守护换进程后它照样显示着一个早已失效的二维码。看门狗靠这条 op 拿到事实。
/// 返回 `None` = 没跑（含守护根本连不上）。
pub fn iroh_status() -> Option<IrohStatus> {
    let Ok(mut s) = connect_daemon_control() else {
        return None;
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::IrohStatus })
    )
    .is_err()
    {
        return None;
    }
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap_or_default();
    if v["running"].as_bool() != Some(true) {
        return None;
    }
    Some(IrohStatus {
        endpoint_id: v["endpoint_id"].as_str().map(String::from),
        token: v["token"].as_str().map(String::from),
        relay: v["relay"].as_str().map(String::from),
        write: v["write"].as_bool().unwrap_or(false),
    })
}

/// 单个已连接的移动端设备信息。
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct IrohConnection {
    /// iroh 节点 ID（公钥的十六进制表示）。
    pub remote_id: String,
    /// 连接建立的时间戳（Unix 秒）。
    pub connected_at: u64,
}

/// 查询当前通过 iroh 隧道连接的移动端设备列表。
pub fn iroh_connections() -> Vec<IrohConnection> {
    let Ok(mut s) = connect_daemon_control() else {
        return Vec::new();
    };
    if writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::IrohConnections })
    )
    .is_err()
    {
        return Vec::new();
    }
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp).is_err() {
        return Vec::new();
    }
    let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap_or_default();
    v["connections"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

// ===================== 状态通道（见 docs/archive/state-channel-plan.md） =====================
//
// 纯数据结构 + 阻塞 socket 通信，已经搬进 smelt-core（本身不碰 GPUI，未来 ACP
// 视图独立成 crate 后也要用同一份），这里重导出成原来的裸名字。
pub(crate) use smelt_core::daemon_state::{
    DaemonPhase, DaemonSessionState, DaemonStateEvent, daemon_reconnect_backoff,
    subscribe_daemon_states_blocking,
};

/// alacritty Term 的统一配置（生产 spawn 与测试共用，防两边漂移）：
/// - kitty_keyboard：默认 false 时 alacritty 会把 `CSI > 1 u` 静默丢掉
///   （push_keyboard_mode 里直接 return），DISAMBIGUATE_ESC_CODES 永远置不上，
///   Shift+Enter 也就永远退化成裸 Enter。见 kitty_keyboard_mode / keystroke_to_bytes。
/// - semantic_escape_chars：双击选词的断词字符。默认集合只有半角标点，中文场景下
///   全角标点也该断词（双击「数据层：字段」不该整段连选），追加常用全角标点。
fn term_config() -> Config {
    Config {
        kitty_keyboard: true,
        semantic_escape_chars: format!(
            "{SEMANTIC_ESCAPE_CHARS}：，。；！？、（）「」『』【】《》“”‘’"
        ),
        ..Config::default()
    }
}

/// 客户端 → 守护的帧：`[type:u8][len:u32 BE][payload]`。type 0=输入，1=resize。
fn write_frame(w: &mut UnixStream, ty: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(ty);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    w.write_all(&frame)
}

/// 编码一帧鼠标事件。`button`：0=左键、3=X10 松开、32=左键拖动等（xterm 约定）。
/// `pressed` 只对 SGR 有意义（`M` vs `m`）；X10 松开时调用方应传 button=3。
fn encode_mouse(mode: TermMode, button: u8, pressed: bool, row: usize, col: usize) -> Vec<u8> {
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    if mode.contains(TermMode::SGR_MOUSE) {
        format!(
            "\x1b[<{button};{cx};{cy}{}",
            if pressed { 'M' } else { 'm' }
        )
        .into_bytes()
    } else {
        // X10：各值偏移 32，坐标裁到 223。
        let cb = button.min(223);
        let bx = 32u8.saturating_add(cx.min(223) as u8);
        let by = 32u8.saturating_add(cy.min(223) as u8);
        vec![0x1b, b'[', b'M', 32 + cb, bx, by]
    }
}

/// `scroll_wheel` 的分流结果：要么把字节喂给应用，要么滚本地 history。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScrollWheelPlan {
    Send(Vec<u8>),
    LocalHistory(i32),
}

/// 编码滚轮 SGR 事件（button 64/65，坐标 1 基）。
fn encode_wheel_sgr(up: bool, count: usize, row: usize, col: usize) -> Vec<u8> {
    let cb: u8 = if up { 64 } else { 65 };
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    let one = format!("\x1b[<{cb};{cx};{cy}M");
    one.repeat(count).into_bytes()
}

/// 编码滚轮 X10 事件。
fn encode_wheel_x10(up: bool, count: usize, row: usize, col: usize) -> Vec<u8> {
    let cb: u8 = if up { 64 } else { 65 };
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    let bx = 32u8.saturating_add(cx.min(223) as u8);
    let by = 32u8.saturating_add(cy.min(223) as u8);
    let mut buf = Vec::with_capacity(6 * count);
    for _ in 0..count {
        buf.extend_from_slice(&[0x1b, b'[', b'M', 32 + cb, bx, by]);
    }
    buf
}

/// 纯函数：按 TermMode 决定滚轮是转发应用还是滚本地 history。
///
/// **回归点**：alacritty 默认 `TermMode` 含 `ALTERNATE_SCROLL`。主屏上绝不能把它
/// 当成「发方向键」——否则 shell / `make dev` 会打印 `^[[A`，滚动条却仍正常
/// （滚动条走 `set_scroll_offset`，不经过这里）。
///
/// **备用屏（Grok/Claude）**：始终发 SGR 滚轮给进程。本地 `MOUSE_MODE` 位 reattach
/// 后常丢，但进程侧鼠标跟踪通常仍开着；方向键多数 chat TUI 用来改输入/历史，
/// **不**滚 transcript。主屏无 mouse → 本地 history。
pub(crate) fn scroll_wheel_plan(
    mode: TermMode,
    lines: i32,
    row: usize,
    col: usize,
) -> ScrollWheelPlan {
    let count = (lines.unsigned_abs() as usize).clamp(1, 8);
    let up = lines > 0;
    // intersects：很多 TUI 只开 MOUSE_MOTION 一位，contains(MOUSE_MODE) 会漏。
    let mouse_on = mode.intersects(TermMode::MOUSE_MODE);
    let alt_screen = mode.contains(TermMode::ALT_SCREEN);

    if alt_screen || mouse_on {
        // 备用屏即使本地丢了 SGR_MOUSE 位也用 SGR 编码（进程侧常见 1006）。
        if alt_screen || mode.contains(TermMode::SGR_MOUSE) {
            ScrollWheelPlan::Send(encode_wheel_sgr(up, count, row, col))
        } else {
            ScrollWheelPlan::Send(encode_wheel_x10(up, count, row, col))
        }
    } else {
        // 主屏：本地 history。不要用 ALTERNATE_SCROLL 发方向键（默认位常开）。
        ScrollWheelPlan::LocalHistory(lines)
    }
}

/// 把用户输入当成字面量塞进 RegexSearch：转义正则元字符，避免 `foo.bar` 误匹配。
pub(crate) fn escape_regex_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 收集 buffer 内全部命中（上限 `SEARCH_MATCH_CAP`）。
fn collect_search_matches<T>(term: &Term<T>, query: &str) -> Vec<SearchMatch> {
    let pattern = escape_regex_literal(query);
    let Ok(mut regex) = RegexSearch::new(&pattern) else {
        return Vec::new();
    };
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, &mut regex)
        .take(SEARCH_MATCH_CAP)
        .map(|m| (*m.start(), *m.end()))
        .collect()
}

/// 绝对坐标命中 → 可视区 SearchHit；不在可视区则 None。
pub(crate) fn match_to_viewport_hit(
    start: Point,
    end: Point,
    display_offset: usize,
    cols: usize,
    active: bool,
) -> Option<SearchHit> {
    let vp = point_to_viewport(display_offset, start)?;
    let col_start = vp.column.0;
    let col_end = if end.line == start.line {
        end.column.0
    } else {
        cols.saturating_sub(1)
    };
    Some(SearchHit {
        row: vp.line,
        col_start,
        col_end,
        active,
    })
}

/// 把剪贴板文本编码成写入 PTY 的字节（见 [`Terminal::paste`]）。
/// 抽成纯函数方便单测，不依赖真 PTY。
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // 剥 ESC：bracketed 内容里若夹着转义序列，会被应用当控制命令执行。
        let cleaned = text.replace('\x1b', "");
        let mut out = Vec::with_capacity(cleaned.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(cleaned.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// 终端内搜索的一条命中（可视区坐标，0 基；跨行时只标首行起止列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize, // 含
    /// 是否为「当前」命中（下/上一个跳到的那条，画得更醒目）。
    pub active: bool,
}

/// 一次搜索操作的汇总：给搜索条显示「3/12」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchStatus {
    /// 当前命中序号（1 基）；0 表示没有命中。
    pub current: usize,
    pub total: usize,
}

/// 滚动条用：`display_offset` 越大越往历史上看；0 = 贴底。
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollInfo {
    pub offset: usize,
    /// 最大可滚 offset（= history_size）。0 表示没有 scrollback，不必画条。
    pub max_offset: usize,
    pub viewport_rows: usize,
}

/// 单次搜索最多收集的命中数，避免超大缓冲卡顿。
const SEARCH_MATCH_CAP: usize = 2000;
/// 内容变化触发的搜索重扫节流：agent 流式输出时每帧都会触发 `refresh_search_highlights`，
/// 不加节流会每帧 spawn 一个搜索线程。查询串本身变了不受此限制。
const SEARCH_RESCAN_THROTTLE: Duration = Duration::from_millis(200);

/// 一个内嵌终端：alacritty 的 Term（后台线程写、UI 线程读）+ 守护连接写队列。
pub struct Terminal {
    term: Arc<Mutex<Term<EventProxy>>>,
    /// 单一后台写端。UI 只向有界队列投递帧，绝不直接等 socket 变为可写。
    writer: TerminalWriter,
    size: TermSize,
    /// 与 EventProxy 共享的行列/单元格像素（resize 与 TextAreaSizeRequest 共用）。
    metrics: Arc<Mutex<TermMetrics>>,
    /// Geometry updates injected by smeltd into the output stream. The read
    /// thread publishes them here; the UI thread applies them to the local VT
    /// without echoing a resize frame back to the daemon.
    daemon_geometry: Arc<Mutex<DaemonGeometrySignal>>,
    daemon_geometry_generation: u64,
    remote_geometry_locked: bool,
    /// 普通 OSC 9/99/777 通知槽（UI 在 PTY 唤醒事件中取走）。
    notify: NotifySlot,
    /// BEL 普通提醒槽，与 OSC 分开，结构化 agent 激活后仍可独立提醒。
    bell_notify: NotifySlot,
    /// 终端标题（任务展示元数据；UI 读 current_title 用于总览）。
    title: Arc<Mutex<Option<String>>>,
    /// `take_damage` 用来识别「光标真的动了」——alacritty 每帧都会把当前光标格标脏，
    /// 静止时要滤掉；但光标移动后若只标了新位置那一格，也得算变化（见 take_damage）。
    last_damage_cursor: Mutex<Option<(i32, usize)>>,
    /// 当前搜索查询串；变了就重建 `search_matches`。
    search_query: Mutex<String>,
    /// 全部命中（缓冲绝对坐标 start..=end），按阅读顺序。
    search_matches: Mutex<Vec<SearchMatch>>,
    /// 当前命中在 `search_matches` 里的下标。
    search_index: Mutex<usize>,
    /// 后台搜索任务结果回传通道（UI 侧 `poll_search_results` 消费）。
    /// 元组：(代数, 查询串, 收集时的 scrollback 顶部行号, 命中)。
    /// 代数/查询串与当前不符的结果直接丢弃。
    search_result_tx: smol::channel::Sender<SearchResult>,
    search_result_rx: smol::channel::Receiver<SearchResult>,
    /// 搜索代数：每次发起重建 +1，回传结果带代数，过期结果丢弃。
    search_generation: Mutex<u64>,
    /// 缓存命中收集时的 scrollback 顶部行号（grid 坐标）。新输出推入时 grid 坐标系
    /// 整体平移（活动区顶部行进 scrollback，所有绝对坐标 -1），`viewport_search_hits`
    /// 用当前 topmost_line 与它的差值平移缓存坐标，避免每帧全量重扫。
    search_matches_top: Mutex<i32>,
    /// 上次发起重建的时间（内容变化触发的重扫节流，见 `set_search_query`）。
    last_search_rescan: Mutex<Instant>,
    /// 查询变了时记录待步进方向（后台结果落地后执行），None = 无待步进。
    pending_step: Mutex<Option<bool>>,
    /// 连接是否已断（读线程 EOF/IO 错误后置位）。守护 exec 交接、被 SIGKILL、
    /// 或 shell 退出都会走到这里——UI 侧据此决定是否自动重连（重连前还会再查
    /// 一次守护里会话还在不在，区分「守护换血」和「shell 真的退了」）。
    dead: Arc<AtomicBool>,
    /// 重绘唤醒（Zed 式事件驱动）：读线程每喂完一批字节就 `try_send(())`，UI 侧
    /// 一个 `cx.spawn` 任务 `recv().await` 后 `cx.notify()`。这样「喂内容」与「触发
    /// 重绘」是同一个动作，不再依赖 30ms 轮询去 `take_damage()` 事后发现——reattach
    /// 后 agent 空闲、只有唯一一次输出时，轮询的时序/过滤一旦漏掉就永久停帧，正是
    /// 那个「底部画不出来、一敲键盘/框选才好」的 bug。`bounded(1)` 天然合并突发：
    /// 空闲无输出＝无唤醒（保住 P0 那条空闲不重绘的优化），有输出才唤醒。
    redraw_rx: smol::channel::Receiver<()>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // 写线程和读线程各自持有 socket clone；仅 drop 队列不能让阻塞读立刻退出。
        // shutdown 会同时唤醒两边并让守护摘掉 attachment，但不杀远端 PTY 会话。
        self.writer.close();
    }
}

/// 新建/reattach 握手失败时的重试次数与间隔：守护无缝升级 exec 交接的一次性抖动是
/// 百毫秒到 1 秒量级，这个预算（5 次 × 300ms ≈ 1.2s，含首次尝试共 5 次）足够盖过去。
const HANDSHAKE_RETRIES: u32 = 5;
const HANDSHAKE_RETRY_DELAY: Duration = Duration::from_millis(300);
/// 握手回执的读超时。正常守护毫秒级就回；僵死/半退出的守护可能接受连接却永不回话，
/// 而握手在 GUI 主线程同步跑——没有这个超时就是无限 beachball（真实发生过：启动
/// 恢复会话时主线程卡死，强杀重开又 abort）。取值对齐 probe_daemon 的 5s。
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 后台终端 writer 的单帧写超时。UI 从不直接执行这次 `write_all`；守护停止消费时，
/// 最多让 writer 线程等约一秒，随后断开 attachment 并由 UI 的重连路径恢复。
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

fn open_request(
    rows: usize,
    cols: usize,
    cwd: Option<&str>,
    id: &str,
    launch: Option<&str>,
    create_if_missing: bool,
) -> serde_json::Value {
    serde_json::json!({
        "op": DaemonOperation::Open,
        "id": id,
        "cwd": cwd,
        "cols": cols,
        "rows": rows,
        "initial_launch": launch,
        "create_if_missing": create_if_missing,
    })
}

/// reattach 快照整段灌进客户端 Term 之后：贴底 + 补鼠标模式位。
///
/// 快照灌完后的 reattach 收尾。
///
/// - 贴底：避免停在 history 中间。
/// - `\x1b[0m`：清 SGR 状态机，降低「快照末半截真彩参数当正文」
///   （顶行出现 `48;2;…m` 碎片）的概率。
/// - **不再本地伪造 MOUSE_MODE**：以前补 `1006h/1002h` 只改客户端 Term，
///   进程侧若未开鼠标，滚轮按 SGR 发出去会被 Ignored →「恢复后滚不动」。
///   无 mouse 的备用屏改走方向键兜底（见 `scroll_wheel_plan`）。
fn finalize_reattach_term(term: &mut Term<EventProxy>, parser: &mut Processor) {
    term.scroll_display(Scroll::Bottom);
    let _ = catch_unwind(AssertUnwindSafe(|| {
        parser.advance(term, b"\x1b[0m");
    }));
}

impl Terminal {
    /// 打开（或重连）守护里 id 对应的会话：shell 环境由 smeltd 负责（-l / TERM /
    /// iTerm2 伪装 / LANG 兜底，见 smeltd）。id 已存在 → attach，守护先重放输出
    /// 缓冲恢复画面，再实时转发。`launch`：新建会话时要先跑的命令（编进 shell 启动
    /// 命令行，见 `smeltd::terminal_registry`），只在新建时生效，reattach 会被忽略。
    pub fn spawn(
        rows: usize,
        cols: usize,
        cwd: Option<&str>,
        id: &str,
        launch: Option<&str>,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(rows, cols, cwd, id, launch, true)
    }

    /// 只重新附着守护中仍存在的 PTY。用于实时流异常断开后的自动恢复；会话已经
    /// 正常退出时返回错误，绝不创建一个同 id 的新 shell 冒充原会话。
    pub fn reattach(rows: usize, cols: usize, cwd: Option<&str>, id: &str) -> anyhow::Result<Self> {
        Self::spawn_inner(rows, cols, cwd, id, None, false)
    }

    fn spawn_inner(
        rows: usize,
        cols: usize,
        cwd: Option<&str>,
        id: &str,
        launch: Option<&str>,
        create_if_missing: bool,
    ) -> anyhow::Result<Self> {
        // 1) 连守护（不在则自动拉起）并声明要打开的会话，握手失败带几次短重试。
        //
        // 守护无缝升级 exec 交接期间，恰好在这一瞬间新开的 pane 可能撞上这个连接
        // 被接受、但握手线程卡在守护内部的 SPAWN_GATE（跟 upgrade 互斥，见 smeltd 升级设计）
        // 上——exec 一发生，这条连接（普通客户端 fd 默认带 CLOEXEC）就被无声关闭，
        // 我们这边会读到 EOF/解析失败。整个交接是百毫秒到 1 秒量级的一次性抖动，
        // 短重试几次基本能把这个窗口盖掉，调用方不必为这种瞬时性错误崩溃整个 GUI
        // （调用方目前对失败仍是 `.expect()`，见 terminal_view.rs 的注释）。
        let (buffered, size, replay_len, geometry_token, daemon_handles_color_requests) = {
            let mut last_err = None;
            let mut result = None;
            let retries = if create_if_missing {
                HANDSHAKE_RETRIES
            } else {
                1
            };
            for attempt in 0..retries {
                if attempt > 0 {
                    thread::sleep(HANDSHAKE_RETRY_DELAY);
                }
                // connect 失败（自动拉起守护 5s 都没就绪）是不可恢复的环境问题，
                // 立即失败——重试只会把「守护起不来」放大成每会话 ~26s 的主线程
                // 阻塞（重试 × 每次再拉一遍守护），启动恢复 8 个会话就是 3 分钟
                // beachball。重试只留给握手层：连上了但读到 EOF/坏行，那才是
                // upgrade 交接的百毫秒抖动，重连一次就好。
                let writer = connect_daemon()?;
                match Self::handshake_on(writer, rows, cols, cwd, id, launch, create_if_missing) {
                    Ok(x) => {
                        result = Some(x);
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            match result {
                Some(x) => x,
                None => return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("握手失败"))),
            }
        };
        let writer = TerminalWriter::start(buffered.get_ref().try_clone()?)?;

        // 2) alacritty 终端状态机（EventProxy 维护标题，把 PTY 自动
        //    应答写回下面这个共享写端）
        let notify: NotifySlot = Arc::new(Mutex::new(None));
        let bell_notify: NotifySlot = Arc::new(Mutex::new(None));
        let title: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let metrics = Arc::new(Mutex::new(TermMetrics {
            rows: size.rows as u16,
            cols: size.cols as u16,
            cell_w: 0,
            cell_h: 0,
        }));
        let daemon_geometry = Arc::new(Mutex::new(DaemonGeometrySignal::default()));
        let term = Term::new(
            term_config(),
            &size,
            EventProxy {
                bell_notify: bell_notify.clone(),
                title: title.clone(),
                writer: writer.clone(),
                metrics: metrics.clone(),
                daemon_handles_color_requests,
            },
        );
        let term = Arc::new(Mutex::new(term));

        // 3) 后台读线程：守护转发的 PTY 字节 → vte 解析更新 Term 网格 + 扫 OSC 9/99/777。
        //    EOF = shell 退出或守护离线（网格冻结，重开会话即恢复）。
        //    复用尺寸行的 BufReader：重放字节可能已在其内部缓冲里。
        // 重绘唤醒通道：读线程 → UI。bounded(1) 合并突发（已有待处理唤醒时后续 try_send
        // 直接丢弃，不堆积）。见 Terminal::redraw_rx 字段注释。
        let (redraw_tx, redraw_rx) = smol::channel::bounded::<()>(1);
        let dead: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        let mut reader = buffered;
        let term_reader = Arc::clone(&term);
        let notify_reader = notify.clone();
        let metrics_reader = Arc::clone(&metrics);
        let daemon_geometry_reader = Arc::clone(&daemon_geometry);
        let dead_reader = Arc::clone(&dead);
        thread::spawn(move || {
            // Processor<T = StdSyncHandler>：默认类型参数不参与 ::new() 推断，需显式标注。
            let mut parser: Processor = Processor::new();
            let mut osc = crate::osc::OscScan::default();
            let mut geometry_osc = geometry_token
                .map(smelt_core::osc::TerminalGeometryOscScan::new)
                .unwrap_or_default();
            let mut buf = [0u8; 4096];
            let mut bytes_seen: usize = 0;
            // 重放缓冲里的历史字节可能藏着早就处理完的 OSC 9/99/777 通知（比如 Claude
            // 之前问过的权限确认，用户当时已经批准、任务也跑完了）——reattach 时如果
            // 原样喂给通知扫描，会把它们当成刚发生的事件重新弹出来，把明明已完成的
            // 会话错误标红（"重开 app 状态变红"那个 bug）。sink 接住落在 replay_len
            // 范围内关闭的 OSC 序列，只有真正在重放边界之后关闭的才写进 notify_reader；
            // 每个字节仍然逐一喂给 osc（状态机不断流），只是根据这个字节的绝对位置
            // 决定它触发的通知该进哪个槽，边界处不会解析错位。
            let sink: Mutex<Option<String>> = Mutex::new(None);
            // reattach 快照是否已整段喂完；越过边界时补一次「贴底 + 鼠标位兜底」。
            let mut replay_finalized = replay_len == 0;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF：shell 退出
                    Ok(n) => {
                        let mut geometry_updates = Vec::new();
                        // OSC 9/99/777 通知：alacritty 不解析，自己扫字节提取
                        for (i, &b) in buf[..n].iter().enumerate() {
                            let target = if bytes_seen + i < replay_len {
                                &sink
                            } else {
                                &notify_reader
                            };
                            if let Some(msg) = osc.feed(b)
                                && let Ok(mut g) = target.lock()
                            {
                                *g = Some(msg);
                            }
                            if let Some(geometry) = geometry_osc.feed(b)
                                && let Ok(mut signal) = daemon_geometry_reader.lock()
                            {
                                signal.generation = signal.generation.wrapping_add(1);
                                signal.geometry = Some(geometry);
                                geometry_updates.push((i + 1, geometry));
                            }
                        }
                        bytes_seen += n;
                        if let Ok(mut term) = term_reader.lock() {
                            // A geometry marker is serialized before PTY
                            // output at the new size. Split this read at each
                            // marker so cursor-addressed bytes after it are
                            // never parsed against the previous grid.
                            let mut start = 0;
                            for (end, geometry) in geometry_updates {
                                parser.advance(&mut *term, &buf[start..end]);
                                if let Ok(mut metrics) = metrics_reader.lock() {
                                    metrics.rows = geometry.rows;
                                    metrics.cols = geometry.cols;
                                    if geometry.cell_width > 0 {
                                        metrics.cell_w = geometry.cell_width;
                                    }
                                    if geometry.cell_height > 0 {
                                        metrics.cell_h = geometry.cell_height;
                                    }
                                }
                                term.resize(TermSize {
                                    rows: usize::from(geometry.rows),
                                    cols: usize::from(geometry.cols),
                                });
                                start = end;
                            }
                            parser.advance(&mut *term, &buf[start..n]);
                            // 快照刚灌完：贴底 + 清 SGR。不再为「jolt 半屏」再贴一次。
                            if !replay_finalized && bytes_seen >= replay_len {
                                replay_finalized = true;
                                finalize_reattach_term(&mut term, &mut parser);
                            }
                        }
                        // 喂完这批立刻请求一次重绘（Zed 式：内容生产者驱动重绘）。
                        // bounded(1) + try_send：已有待处理唤醒就丢弃，天然合并。
                        // 关键性质：最后一批喂完后必有一次待处理唤醒 → UI 必定再画一帧，
                        // 与 reattach 快照喂完这个场景精确对应。
                        let _ = redraw_tx.try_send(());
                    }
                    Err(_) => break,
                }
            }
            // 读线程退出（EOF/守护离线）：标记连接已断，并主动关掉发送端，让 UI 侧
            // 的 recv 任务收到 Err 而退出——UI 侧据此触发自动重连（见 terminal_view.rs
            // 的 schedule_auto_reconnect）。adopt_terminal 换上新连接后 dead 归零，
            // 旧 channel 的 Err 不会再误触发重连。
            dead_reader.store(true, Ordering::Relaxed);
            drop(redraw_tx);
        });

        let (search_result_tx, search_result_rx) = smol::channel::unbounded();

        Ok(Self {
            term,
            writer,
            size,
            metrics,
            daemon_geometry,
            daemon_geometry_generation: 0,
            remote_geometry_locked: false,
            notify,
            bell_notify,
            title,
            last_damage_cursor: Mutex::new(None),
            search_query: Mutex::new(String::new()),
            search_matches: Mutex::new(Vec::new()),
            search_index: Mutex::new(0),
            search_result_tx,
            search_result_rx,
            search_generation: Mutex::new(0),
            search_matches_top: Mutex::new(0),
            last_search_rescan: Mutex::new(Instant::now()),
            pending_step: Mutex::new(None),
            dead,
            redraw_rx,
        })
    }

    /// 重绘唤醒的接收端（clone 一份给 UI 侧的 `cx.spawn` 任务 await）。见 `redraw_rx` 字段。
    pub fn redraw_channel(&self) -> smol::channel::Receiver<()> {
        self.redraw_rx.clone()
    }

    /// 连接是否已断（读线程退出后为 true）。UI 侧拿它判断要不要自动重连；
    /// `adopt_terminal` 换上重连好的新 Terminal 后自然归 false。
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// 一次性握手：在已连上的 stream 上声明会话 + 读首行尺寸 + 重放字节数，不重试
    /// （连接与重试策略都在 `spawn` 里）。replay_len 是 reattach 时守护即将吐给我们
    /// 的历史字节数（新建会话是 0），供 spawn() 的读线程划一条"重放 / 实时"边界，
    /// 见那边的用法。拆成独立函数是为了测试能注入假守护。
    fn handshake_on(
        mut writer: UnixStream,
        rows: usize,
        cols: usize,
        cwd: Option<&str>,
        id: &str,
        launch: Option<&str>,
        create_if_missing: bool,
    ) -> anyhow::Result<HandshakeResult> {
        // 只在等回执这一段设读超时；同文件 probe/remote/subscribe 都设了，唯独
        // 这条最要命的主线程路径曾经漏掉。
        writer.set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))?;
        writeln!(
            writer,
            "{}",
            open_request(rows, cols, cwd, id, launch, create_if_missing)
        )?;
        let mut buffered = BufReader::new(writer);
        let mut line = String::new();
        buffered.read_line(&mut line)?;
        let v: serde_json::Value = serde_json::from_str(line.trim())?;
        if v["ok"].as_bool() == Some(false) {
            anyhow::bail!("{}", v["err"].as_str().unwrap_or("终端重新附着失败"));
        }
        let size = TermSize {
            rows: v["rows"].as_u64().unwrap_or(rows as u64) as usize,
            cols: v["cols"].as_u64().unwrap_or(cols as u64) as usize,
        };
        let replay_len = v["replay_len"].as_u64().unwrap_or(0) as usize;
        let geometry_token = v["geometry_token"].as_str().map(str::to_owned);
        let daemon_handles_color_requests = v["daemon_handles_color_requests"]
            .as_bool()
            .unwrap_or(false);
        // 握手完必须清掉超时：这条 stream 接下来交给读线程长期读 PTY 输出（见
        // spawn 里 `let mut reader = buffered`），空闲终端半天没输出是常态，读循环
        // 对任何 Err 一律 break 当 EOF——超时留着就等于给每个安静的终端定时断线。
        buffered.get_ref().set_read_timeout(None)?;
        Ok((
            buffered,
            size,
            replay_len,
            geometry_token,
            daemon_handles_color_requests,
        ))
    }

    /// 取走最新普通通知消息（读并清）：OSC 9/99/777 上报的文本。
    pub fn take_notification(&self) -> Option<String> {
        self.notify.lock().ok().and_then(|mut g| g.take())
    }

    /// 取走一次普通终端 BEL 提醒；它不携带 Agent 状态语义。
    pub fn take_bell_notification(&self) -> Option<String> {
        self.bell_notify.lock().ok()?.take()
    }

    /// 当前终端标题（agent 报告的任务名或装饰）；未设置返回 None。
    pub fn current_title(&self) -> Option<String> {
        self.title.lock().ok().and_then(|g| g.clone())
    }

    /// 自上次调用以来，终端网格内容是否真的变化了（读并清 alacritty 自带的
    /// damage tracking）。涵盖：PTY 写入的字符/颜色、光标移动、翻滚历史、
    /// 进出备用屏幕（vim/less 等全屏 TUI）、resize 等——这些都由 alacritty 自动
    /// 判定。**不**涵盖：用户拖选（Term.selection 的变化 alacritty 不计入 damage，
    /// 它认为选区高亮是渲染层的事）、Cmd 悬停链接高亮——这两个在 TerminalView
    /// 各自的鼠标事件处理里已经各自调用 cx.notify()，不依赖这里。
    ///
    /// 由 TerminalView 在 PTY 唤醒事件中调用（每个 Terminal 独占一个 Term，不会有
    /// 多个消费者互相"偷"对方读到的脏区）。
    pub fn take_damage(&self) -> bool {
        let Ok(mut term) = self.term.lock() else {
            // 拿不到锁（锁中毒）：保守起见当作有变化，避免画面从此卡死不再刷新。
            return true;
        };
        // alacritty 的 damage_cursor() 每次都会无条件把**当前**光标格标脏（为闪烁动画
        // 设计）。smelt 没有闪烁，静止时必须滤掉「仅当前光标那一格」——否则空闲也 33fps。
        //
        // 但光标真的移动时，脏区可能**只有新位置那一格**（旧格不在 partial 里），若仍按
        // 「等于当前光标就忽略」会吞掉移动 → 光标不重画。所以再记一帧光标位置：动了就
        // 算有变化。
        let cursor = term.grid().cursor.point;
        let cur = (cursor.line.0, cursor.column.0);
        let cursor_moved = match self.last_damage_cursor.lock() {
            Ok(mut g) => {
                let moved = g.map(|prev| prev != cur).unwrap_or(true);
                *g = Some(cur);
                moved
            }
            Err(_) => true,
        };
        let cursor_line = cur.0 as usize;
        let cursor_col = cur.1;
        let damaged = match term.damage() {
            TermDamage::Full => true,
            TermDamage::Partial(it) => {
                let mut any = false;
                let mut only_idle_cursor = true;
                for l in it {
                    any = true;
                    if l.line != cursor_line || l.left != cursor_col || l.right != cursor_col {
                        only_idle_cursor = false;
                        break;
                    }
                }
                if !any {
                    false
                } else if only_idle_cursor {
                    // 脏区恰好是当前光标格：只有光标真的动了才算变化
                    cursor_moved
                } else {
                    true
                }
            }
        };
        term.reset_damage();
        damaged
    }

    /// Apply the daemon's canonical grid without writing a resize frame back.
    /// A remote renderer holds the geometry lease while
    /// `remote_geometry_locked` is true; local viewport changes must wait.
    pub fn sync_daemon_geometry(&mut self) -> bool {
        let update = {
            let Ok(signal) = self.daemon_geometry.lock() else {
                return false;
            };
            if signal.generation == self.daemon_geometry_generation {
                return false;
            }
            signal
                .geometry
                .map(|geometry| (signal.generation, geometry))
        };
        let Some((generation, geometry)) = update else {
            return false;
        };
        self.daemon_geometry_generation = generation;
        self.remote_geometry_locked = geometry.remote_controlled;

        let rows = usize::from(geometry.rows);
        let cols = usize::from(geometry.cols);
        let grid_changed = self.size.rows != rows || self.size.cols != cols;
        self.size = TermSize { rows, cols };
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.rows = geometry.rows;
            metrics.cols = geometry.cols;
            if geometry.cell_width > 0 {
                metrics.cell_w = geometry.cell_width;
            }
            if geometry.cell_height > 0 {
                metrics.cell_h = geometry.cell_height;
            }
        }
        if grid_changed && let Ok(mut term) = self.term.lock() {
            term.resize(self.size);
        }
        true
    }

    pub fn remote_geometry_locked(&self) -> bool {
        self.remote_geometry_locked
    }

    /// 按新行列 + 单元格像素 resize：同步 alacritty 网格，并发帧让守护 ioctl
    /// TIOCSWINSZ（含 ws_xpixel/ws_ypixel）。`cell_w_px` / `cell_h_px` 为 0 时只更新
    /// 行列（兼容老路径）。无变化则跳过。
    ///
    /// reattach 后请再调一次 [`Self::force_resize`]：本函数 same_size 早退会挡掉同尺寸帧；
    /// 守护 jolt 用 cell=0，需要 GUI 首帧量到真实 cell 像素后再强制同步一次。
    pub fn resize(&mut self, rows: usize, cols: usize, cell_w_px: u16, cell_h_px: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let same_grid = rows == self.size.rows && cols == self.size.cols;
        let same_cell = self
            .metrics
            .lock()
            .ok()
            .is_some_and(|m| m.cell_w == cell_w_px && m.cell_h == cell_h_px);
        if same_grid && same_cell {
            return;
        }
        self.apply_resize(rows, cols, cell_w_px, cell_h_px, same_grid);
    }

    /// 无条件向守护发 type-1 resize 帧（行列/cell 相同也发）。
    /// 首帧布局、reattach 后补真实 cell 像素时用——避免 `resize` 早退导致 PTY
    /// 一直停在守护默认/零像素尺寸，Claude/Grok TUI 排版错位。
    pub fn force_resize(&mut self, rows: usize, cols: usize, cell_w_px: u16, cell_h_px: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let same_grid = rows == self.size.rows && cols == self.size.cols;
        self.apply_resize(rows, cols, cell_w_px, cell_h_px, same_grid);
    }

    fn apply_resize(
        &mut self,
        rows: usize,
        cols: usize,
        cell_w_px: u16,
        cell_h_px: u16,
        same_grid: bool,
    ) {
        self.size = TermSize { rows, cols };
        if let Ok(mut m) = self.metrics.lock() {
            m.rows = rows as u16;
            m.cols = cols as u16;
            if cell_w_px > 0 {
                m.cell_w = cell_w_px;
            }
            if cell_h_px > 0 {
                m.cell_h = cell_h_px;
            }
        }
        if !same_grid && let Ok(mut term) = self.term.lock() {
            term.resize(self.size);
        }
        // type 1 帧：cols + rows + cell_w + cell_h（各 u32 BE）。老 smeltd 只认 8 字节，
        // 新守护认 16 字节并把 cell 像素乘到 ws_xpixel/ws_ypixel。
        let (cw, ch) = self
            .metrics
            .lock()
            .ok()
            .map(|m| (m.cell_w, m.cell_h))
            .unwrap_or((cell_w_px, cell_h_px));
        let mut payload = [0u8; 16];
        payload[0..4].copy_from_slice(&(cols as u32).to_be_bytes());
        payload[4..8].copy_from_slice(&(rows as u32).to_be_bytes());
        payload[8..12].copy_from_slice(&(cw as u32).to_be_bytes());
        payload[12..16].copy_from_slice(&(ch as u32).to_be_bytes());
        let _ = self.writer.send_resize(&payload);
    }

    /// 向 shell 写入字节（键盘输入用）：帧转发给守护。
    ///
    /// `false` 表示这次输入没有完整入队；调用方可以据此向用户反馈，而不是把
    /// 队列背压/连接断开静默吞掉。
    pub fn send_input(&mut self, bytes: &[u8]) -> bool {
        self.writer.send_input(bytes)
    }

    /// 粘贴文本到 PTY。对端开了 bracketed paste（`CSI ?2004h`）时包
    /// `\x1b[200~…\x1b[201~`，并剥掉内容里的 ESC（防注入序列）；否则只把 `\r\n`/`\n`
    /// 规范成 `\r`——shell 行编辑器认的是 CR，原样喂 LF 会在 zsh/bash 里被当成提交
    /// 多次。跟 Zed `Terminal::paste` / iTerm 行为一致。
    pub fn paste(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        let bracketed = match self.term.lock() {
            Ok(term) => term.mode().contains(TermMode::BRACKETED_PASTE),
            Err(_) => false,
        };
        self.send_input(&encode_paste(text, bracketed))
    }

    /// 是否处于「应用光标键」模式（DECCKM）。像 Claude Code 里那种上下选列表的全屏
    /// TUI，进入时会开这个模式，把方向键约定成 SS3（`ESC O A/B/C/D`）而非默认的
    /// CSI（`ESC [ A/B/C/D`）——发错一种应用收不到方向键，见 keystroke_to_bytes。
    pub fn app_cursor_mode(&self) -> bool {
        match self.term.lock() {
            Ok(term) => term.mode().contains(TermMode::APP_CURSOR),
            Err(_) => false,
        }
    }

    /// 对端有没有开 kitty keyboard protocol 的「消歧」层（进入时发 `CSI > 1 u`）。
    /// 传统终端编码里 Shift+Enter 跟裸 Enter 撞车（都是 `\r`），修饰键信息丢了；开了这个
    /// 模式后带修饰键的按键改用 CSI u 编码上报，两者才能分开。Claude Code 从 v2.1 起
    /// 启动时会主动开——不开的程序（bash/zsh）就得继续收遗留编码，见 keystroke_to_bytes。
    pub fn kitty_keyboard_mode(&self) -> bool {
        match self.term.lock() {
            Ok(term) => term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES),
            Err(_) => false,
        }
    }

    /// 焦点变化上报（DEC 1004，`CSI ?1004h` 打开）：应用开了这个模式时，终端在获得 / 失去
    /// 焦点时要发 `ESC[I` / `ESC[O`。vim、部分 TUI 靠它决定要不要暂停动画、要不要重绘成
    /// 「未聚焦」的样子。没开这个模式的应用绝不能收到这两个序列——否则会被当成普通输入。
    pub fn report_focus(&mut self, focused: bool) {
        let enabled = match self.term.lock() {
            Ok(term) => term.mode().contains(TermMode::FOCUS_IN_OUT),
            Err(_) => false,
        };
        if enabled {
            self.send_input(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// 快照当前可视网格 + 光标。用 renderable_content：尊重滚动偏移、带光标，
    /// 并处理反色（INVERSE）/粗体/下划线属性。
    pub fn snapshot(&self) -> Frame {
        let term = match self.term.lock() {
            Ok(t) => t,
            Err(_) => {
                return Frame {
                    rows: Vec::new(),
                    cursor: None,
                    cursor_pos: None,
                    wrapped: Vec::new(),
                };
            }
        };
        let content = term.renderable_content();
        let cursor_pt = content.cursor.point;
        let display_offset = content.display_offset;
        // 选区范围由 alacritty 维护（滚动跟随、新输出漂移、宽字符边界都是它处理），
        // 这里只做逐 cell 的 contains 判定——indexed.point 与 SelectionRange 坐标同源，直接比。
        let sel_range = content.selection;

        let cols = self.size.cols;
        let mut rows: Vec<Vec<Cell>> = Vec::with_capacity(self.size.rows);
        let mut wrapped: Vec<bool> = Vec::with_capacity(self.size.rows);
        let mut row: Vec<Cell> = Vec::with_capacity(cols);
        let mut count = 0usize;
        // 当前行最后一格是不是 WRAPLINE——每格都刷新，行满时刚好是最后一格的值。
        let mut row_wraps;
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let selected = sel_range
                .as_ref()
                .is_some_and(|r| r.contains(indexed.point));
            let flags = cell.flags;
            row_wraps = flags.contains(Flags::WRAPLINE);
            let inverse = flags.contains(Flags::INVERSE);
            let mut fg = resolve(cell.fg, true);
            let mut bg = resolve(cell.bg, false);
            // 反色（SGR 7）后真正当底色用的是**前景那个颜色**，默认底色的判定也得跟着换。
            let bg_color = if inverse { cell.fg } else { cell.bg };
            let bg_default = matches!(bg_color, Color::Named(NamedColor::Background));
            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            // 宽字符占两格，第二格是 WIDE_CHAR_SPACER 占位：一律记成 '\0'。
            // 渲染侧（render_row）据此跳过该格但让列号照常前进，于是宽字符后面的内容
            // 列号不再连续 → 自动断成新的一批、按 grid 列重新定位。字形本身宽窄不影响
            // 后续字符的位置，所以这里不必再区分「字形正好两格的 CJK」和「宽度不足的
            // emoji」——那个区分只在「靠字形宽度自然占位」的旧渲染下才有意义。
            let ch = if flags.contains(Flags::WIDE_CHAR_SPACER) {
                '\0'
            } else {
                cell.c
            };
            row.push(Cell {
                ch,
                fg,
                bg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                dim: flags.contains(Flags::DIM),
                // 下划线有 5 种（普通/双线/波浪/点/虚线），`ALL_UNDERLINES` 是它们的聚合位；
                // 只认 UNDERLINE 那一位的话，编译器诊断的波浪线之类会整个不显示。
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                undercurl: flags.contains(Flags::UNDERCURL),
                strikeout: flags.contains(Flags::STRIKEOUT),
                zw: cell
                    .zerowidth()
                    .filter(|z| !z.is_empty())
                    .map(|z| z.to_vec().into_boxed_slice()),
                link: cell.hyperlink().map(|h| Arc::from(h.uri())),
                bg_default,
                selected,
            });
            count += 1;
            if count.is_multiple_of(cols) {
                rows.push(std::mem::take(&mut row));
                wrapped.push(row_wraps);
            }
        }
        if !row.is_empty() {
            rows.push(row);
            wrapped.push(false);
        }

        // 光标位置：alacritty 的 cursor.point 是**活动区**坐标（不含滚动偏移），加上
        // display_offset 才是屏幕上的行——上滚 N 行看历史时，内容整体下移 N 行，光标也
        // 跟着往下走（iTerm2 行为）。只有滚到光标离开可视区才没有位置。
        // 之前是「一上滚就直接 None」：滚一行光标就消失，IME 候选窗也跟着跳回左上角。
        let cursor_pos = {
            let r = cursor_pt.line.0 + display_offset as i32;
            if r >= 0 && (r as usize) < rows.len() {
                Some((r as usize, cursor_pt.column.0))
            } else {
                None
            }
        };
        // 可见光标：应用没用 CSI ?25l 隐藏时才交给渲染层画（见 Frame 字段注释）。
        // 形状随 DECSCUSR 走（zsh vi-mode 会在插入/普通态之间切竖线和方块）。
        let cursor = match content.cursor.shape {
            CursorShape::Hidden => None,
            shape => cursor_pos.map(|(r, c)| {
                let kind = match shape {
                    CursorShape::Underline => CursorKind::Underline,
                    CursorShape::Beam => CursorKind::Bar,
                    CursorShape::HollowBlock => CursorKind::Hollow,
                    _ => CursorKind::Block,
                };
                (r, c, kind)
            }),
        };

        Frame {
            rows,
            cursor,
            cursor_pos,
            wrapped,
        }
    }

    /// 上下滚动历史缓冲：正数向上翻看历史，负数向下。（Shift+PageUp 用，强制本地历史。）
    pub fn scroll(&mut self, lines: i32) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// 滚回底部：真实终端的通行做法——手滑滚了一下历史后忘了滚回去，键盘一敲就该
    /// 跟手回到最新输出，不然新内容（比如 Claude Code 退出时打的那行提示）默默追加
    /// 到当前视野之外，用户会误以为「没打印」。
    pub fn scroll_to_bottom(&mut self) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Bottom);
        }
    }

    /// 滚轮：按终端当前模式分流，`lines` 正数向上、负数向下，`(row,col)` 为 0 基单元格。
    ///
    /// - **备用屏**（Claude/Grok）→ 始终 SGR 滚轮给进程（本地 mouse 位 reattach 会丢）。
    /// - 主屏 + 应用开了鼠标 → SGR/X10。
    /// - **主屏无鼠标** → 本地 history。注意默认 `ALTERNATE_SCROLL` 绝不能在主屏发方向键。
    pub fn scroll_wheel(&mut self, lines: i32, row: usize, col: usize) {
        let mode = match self.term.lock() {
            Ok(term) => *term.mode(),
            Err(_) => return,
        };
        match scroll_wheel_plan(mode, lines, row, col) {
            ScrollWheelPlan::Send(bytes) => {
                let _ = self.send_input(&bytes);
            }
            ScrollWheelPlan::LocalHistory(delta) => {
                let Ok(mut term) = self.term.lock() else {
                    return;
                };
                term.scroll_display(Scroll::Delta(delta));
            }
        }
    }

    /// 应用是否开了任意鼠标上报（click / drag / motion 之一）。UI 用来在
    /// 「本地框选」和「转发给 TUI」之间分流；按住 Shift 时调用方应强制走本地选区
    /// （xterm 约定：Shift 旁路应用鼠标）。
    pub fn mouse_mode(&self) -> bool {
        match self.term.lock() {
            Ok(term) => term.mode().intersects(TermMode::MOUSE_MODE),
            Err(_) => false,
        }
    }

    /// 鼠标按下/松开上报。`button`：0=左、1=中、2=右（xterm 约定）。
    /// 应用开了 `MOUSE_MODE` 时才编码转发，否则返回 false。
    pub fn mouse_button(&mut self, button: u8, pressed: bool, row: usize, col: usize) -> bool {
        let mode = match self.term.lock() {
            Ok(term) => *term.mode(),
            Err(_) => return false,
        };
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        // SGR：button + pressed 决定 M/m；X10：松开固定 button 3。
        let code = if !pressed && !mode.contains(TermMode::SGR_MOUSE) {
            3
        } else {
            button.min(2)
        };
        self.send_input(&encode_mouse(mode, code, pressed, row, col));
        true
    }

    /// 按住某键拖动上报（button = 32+btn）。仅在 `MOUSE_DRAG` 或 `MOUSE_MOTION` 时转发。
    pub fn mouse_drag(&mut self, button: u8, row: usize, col: usize) -> bool {
        let mode = match self.term.lock() {
            Ok(term) => *term.mode(),
            Err(_) => return false,
        };
        if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
            return false;
        }
        // 32 = motion 标志；+0/1/2 = 左/中/右。
        self.send_input(&encode_mouse(mode, 32 + button.min(2), true, row, col));
        true
    }

    /// 无按键悬停 motion（button = 35）。仅 `MOUSE_MOTION` 全开时 TUI 才关心。
    pub fn mouse_motion(&mut self, row: usize, col: usize) -> bool {
        let mode = match self.term.lock() {
            Ok(term) => *term.mode(),
            Err(_) => return false,
        };
        if !mode.contains(TermMode::MOUSE_MOTION) {
            return false;
        }
        self.send_input(&encode_mouse(mode, 35, true, row, col));
        true
    }

    /// 重建搜索命中列表（查询变了或内容大变时）。不滚动、不改当前序号（夹到合法范围）。
    ///
    /// 全量扫描在后台线程执行（`collect_search_matches` 遍历整个 scrollback，大缓冲下
    /// 同步跑会卡 UI）；结果经 channel 回传，UI 侧 `poll_search_results` 消费后更新。
    /// 返回的状态基于当前缓存——新结果落地前可能短暂显示旧值。
    pub fn set_search_query(&mut self, query: &str) -> SearchStatus {
        let q = query.trim().to_string();
        if q.is_empty() {
            self.clear_search();
            return SearchStatus::default();
        }
        // 查询没变且刚重扫过（内容变化触发的重扫）：节流跳过，避免搜索线程风暴。
        let query_changed = self.search_query.lock().ok().is_none_or(|g| *g != q);
        if !query_changed {
            let now = Instant::now();
            let last = self.last_search_rescan.lock().map(|g| *g).unwrap_or(now);
            if now.duration_since(last) < SEARCH_RESCAN_THROTTLE {
                return self.search_status();
            }
        }
        if let Ok(mut g) = self.search_query.lock() {
            *g = q.clone();
        }
        let generation = {
            let mut g = self
                .search_generation
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *g += 1;
            *g
        };
        if let Ok(mut g) = self.last_search_rescan.lock() {
            *g = Instant::now();
        }
        // 后台扫：锁 term 只发生在任务线程内，UI 线程不被阻塞。
        let term = Arc::clone(&self.term);
        let tx = self.search_result_tx.clone();
        thread::spawn(move || {
            let (top, matches) = match term.lock() {
                Ok(t) => (t.topmost_line().0, collect_search_matches(&t, &q)),
                Err(_) => (0, Vec::new()),
            };
            let _ = tx.try_send((generation, q, top, matches));
        });
        self.search_status()
    }

    /// 消费后台搜索任务的结果；返回是否有新结果落地（UI 据此决定是否重绘）。
    ///
    /// 过期结果（代数或查询串与当前不符）直接丢弃——用户快速改查询时，旧任务
    /// 晚到不能覆盖新查询的结果。查询变了时 `find_next` 记下的待步进方向也在这里执行。
    pub fn poll_search_results(&mut self) -> bool {
        let mut updated = false;
        while let Ok((generation, q, top, matches)) = self.search_result_rx.try_recv() {
            let current_gen = self.search_generation.lock().map(|g| *g).unwrap_or(0);
            let current_q = self
                .search_query
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            if generation != current_gen || q != current_q {
                continue;
            }
            let total = matches.len();
            if let Ok(mut g) = self.search_matches.lock() {
                *g = matches;
            }
            if let Ok(mut g) = self.search_matches_top.lock() {
                *g = top;
            }
            if let Ok(mut g) = self.search_index.lock() {
                if total == 0 {
                    *g = 0;
                } else {
                    *g = (*g).min(total - 1);
                }
            }
            updated = true;
        }
        // 查询变了时 find_next 记下的待步进方向：新结果落地后执行。
        if updated {
            let backward = self
                .pending_step
                .lock()
                .map(|mut g| g.take())
                .unwrap_or(None);
            if let Some(backward) = backward {
                let total = self.search_matches.lock().map(|m| m.len()).unwrap_or(0);
                if total > 0 {
                    if let Ok(mut g) = self.search_index.lock() {
                        *g = if backward { total - 1 } else { 0 };
                    }
                    self.scroll_to_active_match();
                }
            }
        }
        updated
    }

    /// 当前搜索查询串（搜索条打开且非空时返回）。
    pub fn current_search_query(&self) -> Option<String> {
        self.search_query
            .lock()
            .ok()
            .map(|g| g.clone())
            .filter(|q| !q.is_empty())
    }

    /// 跳到下一处 / 上一处命中并滚动到可视区。查询串变了会先异步重建列表，
    /// 步进在结果落地后执行（见 `poll_search_results` 的 pending_step）。
    pub fn find_next(&mut self, query: &str, backward: bool) -> SearchStatus {
        let q = query.trim().to_string();
        if q.is_empty() {
            self.clear_search();
            return SearchStatus::default();
        }
        let query_changed = self.search_query.lock().ok().is_none_or(|g| *g != q);
        if query_changed {
            // 新查询：异步重建，记下步进方向，结果回来后从首/末条起跳。
            let _ = self.set_search_query(&q);
            if let Ok(mut g) = self.pending_step.lock() {
                *g = Some(backward);
            }
            return self.search_status();
        }
        // 查询没变：基于当前缓存步进（内容变化的重扫由 refresh_search_highlights 负责）。
        let total = self.search_matches.lock().map(|m| m.len()).unwrap_or(0);
        if total == 0 {
            return SearchStatus::default();
        }
        if let Ok(mut g) = self.search_index.lock() {
            *g = if backward {
                if *g == 0 { total - 1 } else { *g - 1 }
            } else {
                (*g + 1) % total
            };
        }
        self.scroll_to_active_match();
        self.search_status()
    }

    /// 当前搜索序号汇总。
    pub fn search_status(&self) -> SearchStatus {
        let total = self.search_matches.lock().map(|m| m.len()).unwrap_or(0);
        if total == 0 {
            return SearchStatus::default();
        }
        let current = self
            .search_index
            .lock()
            .map(|g| (*g + 1).min(total))
            .unwrap_or(1);
        SearchStatus { current, total }
    }

    /// 当前可视区内所有命中（含 active 标记），供 paint 高亮。
    ///
    /// 只做「缓存命中 → 可视区」的映射，不再全量重扫：新输出推入时 grid 坐标系整体
    /// 平移，用当前 topmost_line 与收集时的差值平移缓存坐标（见 `search_matches_top`）；
    /// 滚动（display_offset 变化）时 scrollback 内容没变，映射随 offset 正确平移。
    /// 内容变化后的重扫由 `refresh_search_highlights` 异步触发（见 `set_search_query`），
    /// 重扫完成前高亮可能短暂滞后一帧。本方法只在搜索条打开时被调用，命中数有
    /// SEARCH_MATCH_CAP 兜底。
    pub fn viewport_search_hits(&self) -> Vec<SearchHit> {
        let Ok(term) = self.term.lock() else {
            return Vec::new();
        };
        let offset = term.grid().display_offset();
        let top = term.topmost_line().0;
        let matches = match self.search_matches.lock() {
            Ok(m) => m.clone(),
            Err(_) => return Vec::new(),
        };
        let cached_top = self.search_matches_top.lock().map(|g| *g).unwrap_or(top);
        let delta = top - cached_top;
        let active_idx = self.search_index.lock().map(|g| *g).unwrap_or(0);
        let mut out = Vec::new();
        for (i, (start, end)) in matches.iter().enumerate() {
            let start = Point::new(start.line + delta, start.column);
            let end = Point::new(end.line + delta, end.column);
            if let Some(hit) =
                match_to_viewport_hit(start, end, offset, self.size.cols, i == active_idx)
            {
                out.push(hit);
            }
        }
        out
    }

    fn scroll_to_active_match(&mut self) {
        let Ok(mut term) = self.term.lock() else {
            return;
        };
        let Ok(matches) = self.search_matches.lock() else {
            return;
        };
        let idx = self.search_index.lock().map(|g| *g).unwrap_or(0);
        if let Some((start, _)) = matches.get(idx) {
            // 缓存坐标可能因新输出推入而过期：按 topmost_line 差值平移后再滚动。
            let top = term.topmost_line().0;
            let cached_top = self.search_matches_top.lock().map(|g| *g).unwrap_or(top);
            let delta = top - cached_top;
            term.scroll_to_point(Point::new(start.line + delta, start.column));
        }
    }

    /// 清空搜索状态（关搜索条时）。
    pub fn clear_search(&mut self) {
        if let Ok(mut g) = self.search_query.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.search_matches.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.search_matches_top.lock() {
            *g = 0;
        }
        if let Ok(mut g) = self.search_index.lock() {
            *g = 0;
        }
        if let Ok(mut g) = self.pending_step.lock() {
            *g = None;
        }
        // 代数 +1：进行中的后台任务结果到达时会被判过期丢弃。
        if let Ok(mut g) = self.search_generation.lock() {
            *g += 1;
        }
    }

    /// 滚动条用的 offset / 上限 / 可视行数。
    pub fn scroll_info(&self) -> ScrollInfo {
        let Ok(term) = self.term.lock() else {
            return ScrollInfo {
                offset: 0,
                max_offset: 0,
                viewport_rows: self.size.rows,
            };
        };
        ScrollInfo {
            offset: term.grid().display_offset(),
            max_offset: term.history_size(),
            viewport_rows: self.size.rows,
        }
    }

    /// 把 display_offset 设到目标值（夹到 `[0, history_size]`）。
    pub fn set_scroll_offset(&mut self, offset: usize) {
        let Ok(mut term) = self.term.lock() else {
            return;
        };
        let max = term.history_size();
        let target = offset.min(max);
        let cur = term.grid().display_offset();
        let delta = target as i32 - cur as i32;
        if delta != 0 {
            // 正数 = 向上看历史（增大 offset）
            term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// 可视区 (行, 列) → 缓冲区绝对坐标：行列先夹进可视范围，再按**当前**
    /// display_offset 换算。选区跟随滚动的关键就是每次都用当前偏移重算。
    fn grid_point(&self, term: &Term<EventProxy>, row: usize, col: usize) -> Point {
        let row = row.min(self.size.rows.saturating_sub(1));
        let col = col.min(self.size.cols.saturating_sub(1));
        viewport_to_point(term.grid().display_offset(), Point::new(row, Column(col)))
    }

    /// 开始一段选区。`left_side`：起点落在单元格左半还是右半（alacritty 用它决定
    /// 该格是否纳入选区——同格同侧的空 Simple 选区不产出内容，单击/微抖不会误选）。
    pub fn selection_start(
        &mut self,
        row: usize,
        col: usize,
        left_side: bool,
        kind: SelectionKind,
    ) {
        let Ok(mut term) = self.term.lock() else {
            return;
        };
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let point = self.grid_point(&term, row, col);
        let side = if left_side { Side::Left } else { Side::Right };
        term.selection = Some(Selection::new(ty, point, side));
    }

    /// 拖动更新选区活动端。坐标按当前 display_offset 重算，所以滚动后再拖、
    /// 或拖着不动光滚动（拖边缘自动滚动）都落在正确的缓冲区行上。
    pub fn selection_update(&mut self, row: usize, col: usize, left_side: bool) {
        let Ok(mut term) = self.term.lock() else {
            return;
        };
        let point = self.grid_point(&term, row, col);
        let side = if left_side { Side::Left } else { Side::Right };
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    /// 清除选区。
    pub fn selection_clear(&mut self) {
        if let Ok(mut term) = self.term.lock() {
            term.selection = None;
        }
    }

    /// 当前选区文本：委托 alacritty 按缓冲区绝对行遍历（含已滚出可视区的
    /// scrollback），宽字符占位/软换行由它处理。空选区（单击未拖动）返回 None。
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock().ok()?;
        term.selection_to_string().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod damage_gate_tests {
    use super::*;

    /// 一次性测试会话的自动清理：Drop 无论函数正常返回还是中途 panic（assert
    /// 失败）都会执行，不像原来手写在函数末尾的 `kill_remote(&id)`——那种写法
    /// 一旦前面某个 assert 炸了就会被 unwind 跳过，永远执行不到。
    ///
    /// 真实教训：这几个测试连的是真实 smeltd 守护进程（不是 mock），而它们本身
    /// 是有记录的 flaky（全量并行跑时偶发超时失败，见 flaky-damage-gate-tests 记
    /// 忆）——每 flaky 失败一次就跳过一次手写清理，泄漏一个游离会话会话；开发机上
    /// 长期攒了一堆 `smelt-kitty-test-*`/`smelt-damage-test-*`，跟真实项目会话
    /// 混在守护进程的会话计数里，「侧栏会话数」和「守护进程会话数」对不上正是
    /// 这个原因。
    struct TestSessionGuard(String);
    impl Drop for TestSessionGuard {
        fn drop(&mut self) {
            kill_remote(&self.0);
        }
    }

    /// 自动重连的触发前提：连接断开后 `is_dead()` 必须置位。模拟守护侧杀掉会话
    /// （= 连接被断，与 exec 交接断开连接同一条路径），读线程应读到 EOF 并标记
    /// dead——UI 侧（terminal_view 的 schedule_auto_reconnect）据此启动自动重连。
    /// 若这个不置位，exec 断线后终端就永远静默冻结，正是本次「终端全卡、只能
    /// 重启 GUI」bug 的根因之一。
    #[test]
    fn killed_session_marks_terminal_dead() {
        let dir = std::env::temp_dir().join(format!("smelt-dead-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let id = format!("dead-test-{}", uuid_like());
        // 不持 guard：会话由下面的 kill_remote 主动清理。
        let term = Terminal::spawn(24, 80, dir.to_str(), &id, None).expect("spawn 失败");
        assert!(!term.is_dead(), "刚连上不该是断的");

        // 等 shell 起来（读线程确实在跑），再杀掉会话模拟连接断开。
        thread::sleep(Duration::from_millis(500));
        kill_remote(&id);

        // 守护 remove 会话并 shutdown client → 读线程 EOF → dead 置位。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if term.is_dead() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "kill 会话后读线程应在超时窗口内标记 dead"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// 验证上面这个修复模式本身是对的：手写清理语句会被 panic 跳过，Drop 不会
    /// ——这正是要堵的洞，不是走个形式验证 Rust 语言特性。
    #[test]
    fn guard_cleans_up_even_when_body_panics() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let cleaned = Arc::new(AtomicBool::new(false));
        let cleaned2 = cleaned.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = Guard(cleaned2);
            panic!("模拟测试中途 assert 失败");
        }));
        assert!(result.is_err(), "闭包应该 panic 了");
        assert!(
            cleaned.load(Ordering::SeqCst),
            "即使 panic，Drop 也该执行清理，不能被跳过"
        );
    }

    /// P0 性能修复的验证：真空闲时 take_damage() 应稳定为 false（跳过重画），
    /// 写入字节后应变 true（真实变化不会被吞掉）。用全新一次性 session id +
    /// 空临时目录，不碰任何真实/持久化会话。
    ///
    /// 交互 shell 会间歇吐 PROMPT / OSC 标题，不能当「真空闲」基线。先跑 `cat`
    /// 堵住前台（不再画 prompt），再测门控；有输入时 cat 回显触发真实 damage。
    #[test]
    fn idle_then_input_toggles_damage() {
        let dir = std::env::temp_dir().join(format!("smelt-damage-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let id = format!("damage-test-{}", uuid_like());
        let _guard = TestSessionGuard(id.clone());

        let mut term = Terminal::spawn(24, 80, dir.to_str(), &id, None).expect("spawn 失败");

        // 等 shell 起来后进入 cat：前台进程阻塞读，不再周期性画 prompt。
        thread::sleep(Duration::from_millis(500));
        term.send_input(b"cat\n");
        thread::sleep(Duration::from_millis(300));

        // 排掉 cat 启动 + 命令回显带来的 damage，直到连续安静。
        let mut quiet_streak = 0usize;
        for _ in 0..80 {
            thread::sleep(Duration::from_millis(50));
            if term.take_damage() {
                quiet_streak = 0;
            } else {
                quiet_streak += 1;
                if quiet_streak >= 10 {
                    break;
                }
            }
        }
        assert!(
            quiet_streak >= 10,
            "cat 阻塞后未能进入真空闲（quiet_streak={quiet_streak}），无法测 damage 门控"
        );

        // 真空闲：take_damage() 应稳定为 false。shell 输出由 pump 线程异步解析，
        // 高负载下偶有迟到一帧——按「连续 10 次全 false」断言，容忍孤立迟到帧，
        // 同时要求真实连续安静（门控坏了就凑不齐连续安静）。
        let mut quiet_streak = 0usize;
        for _ in 0..60 {
            thread::sleep(Duration::from_millis(100));
            if term.take_damage() {
                quiet_streak = 0;
            } else {
                quiet_streak += 1;
                if quiet_streak >= 10 {
                    break;
                }
            }
        }
        assert!(
            quiet_streak >= 10,
            "真空闲时 take_damage() 不该返回 true（连续安静 {quiet_streak}/10）"
        );

        // 写入真实字节：cat 回显，应被判定为变化。轮询等待，避免固定 sleep 在高
        // 负载下不够 cat 回传。
        term.send_input(b"hi\n");
        let mut saw_damage = false;
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(50));
            if term.take_damage() {
                saw_damage = true;
                break;
            }
        }
        assert!(saw_damage, "写入字节后 take_damage() 应返回 true");
        // 清理交给 _guard 的 Drop（含 panic 路径），不再手写。
    }

    /// 走完整生产路径（Terminal::spawn 的真 PTY + 真 shell + alacritty 解析）验证
    /// kitty keyboard protocol 能被识别——上面 event_proxy 那个测试是手搭 Config 的，
    /// 万一 spawn 里忘了开 kitty_keyboard 它照样绿，这里才防得住。
    #[test]
    fn spawned_terminal_honors_kitty_keyboard_protocol() {
        let dir = std::env::temp_dir().join(format!("smelt-kitty-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let id = format!("kitty-test-{}", uuid_like());
        let _guard = TestSessionGuard(id.clone());

        let term = Terminal::spawn(24, 80, dir.to_str(), &id, None).expect("spawn 失败");
        thread::sleep(Duration::from_millis(800)); // 等 shell 起来

        assert!(!term.kitty_keyboard_mode(), "shell 刚起来时不该开着");

        // 让 shell 真的把 `CSI > 1 u` 吐到 PTY 上（Claude Code v2.1+ 启动时干的事）。
        let mut term = term;
        term.send_input(b"printf '\\033[>1u'\n");
        // 解析由 pump 线程异步进行，高负载下 shell 执行 + 回传可能超过固定 sleep；
        // 轮询等待置位（上限 3s），避免时序 flaky。
        let mut kitty_on = false;
        for _ in 0..30 {
            thread::sleep(Duration::from_millis(100));
            if term.kitty_keyboard_mode() {
                kitty_on = true;
                break;
            }
        }
        assert!(
            kitty_on,
            "真实 PTY 上收到 CSI > 1 u 后应置位——没置位说明 spawn 的 Config 没开 kitty_keyboard"
        );
        // 清理交给 _guard 的 Drop（含 panic 路径），不再手写。
    }

    /// 全屏 TUI（Cursor CLI / Claude Code）用 `CSI ?25l` 藏真实光标、在输入框自画
    /// 假光标，真实光标常停在屏幕角落——snapshot 照画就会多出一个孤立反色块。
    /// 隐藏时 cursor（渲染用）必须为 None，cursor_pos（IME 定位用）必须保留。
    /// 直接往 Term 注入序列（不经 shell），避免时序 flaky；注入前等启动输出沉淀。
    #[test]
    fn hidden_cursor_not_rendered_but_position_kept() {
        let dir = std::env::temp_dir().join(format!("smelt-cursor-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let id = format!("cursor-test-{}", uuid_like());
        let _guard = TestSessionGuard(id.clone());

        let term = Terminal::spawn(24, 80, dir.to_str(), &id, None).expect("spawn 失败");
        thread::sleep(Duration::from_millis(800)); // 等 shell 启动输出沉淀，避免并发 advance 干扰

        let frame = term.snapshot();
        let (row, col, kind) = frame.cursor.expect("shell 正常状态光标应可见");
        assert_eq!(
            Some((row, col)),
            frame.cursor_pos,
            "光标可见时位置应与 cursor_pos 一致"
        );
        assert_eq!(kind, CursorKind::Block, "没发过 DECSCUSR 时是默认的实心块");

        let inject = |bytes: &[u8]| {
            let mut parser: Processor = Processor::new();
            let mut t = term.term.lock().unwrap();
            parser.advance(&mut *t, bytes);
        };

        inject(b"\x1b[?25l");
        let frame = term.snapshot();
        assert!(
            frame.cursor.is_none(),
            "CSI ?25l 隐藏后不该再交给渲染层画反色块"
        );
        assert!(
            frame.cursor_pos.is_some(),
            "隐藏光标的位置（IME 定位用）不该丢"
        );

        inject(b"\x1b[?25h");
        assert!(
            term.snapshot().cursor.is_some(),
            "CSI ?25h 后光标应恢复可见"
        );

        // DECSCUSR：zsh vi-mode 靠它在插入态切竖线、普通态切回方块。形状必须带到渲染层，
        // 不能一律画成块。
        inject(b"\x1b[5 q"); // 5/6 = 竖线（闪烁/稳定）
        let (.., kind) = term.snapshot().cursor.expect("竖线光标仍是可见光标");
        assert_eq!(kind, CursorKind::Bar, "CSI 5 SP q 应切成竖线");

        inject(b"\x1b[3 q"); // 3/4 = 下划线
        let (.., kind) = term.snapshot().cursor.expect("下划线光标仍是可见光标");
        assert_eq!(kind, CursorKind::Underline, "CSI 3 SP q 应切成下划线");
        // 清理交给 _guard 的 Drop（含 panic 路径），不再手写。
    }

    /// 用户报告：框选后滚动，选区高亮消失。选区跟随滚动是本次重构的核心目标——
    /// 选区存缓冲区绝对坐标，滚动只改 display_offset，snapshot 的逐 cell contains
    /// 判定应该继续命中。直接注入内容+滚动（不经 shell 时序），确定性复现。
    #[test]
    fn selection_survives_scrolling() {
        let dir = std::env::temp_dir().join(format!("smelt-selscroll-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let id = format!("selscroll-test-{}", uuid_like());
        let _guard = TestSessionGuard(id.clone());

        let mut term = Terminal::spawn(24, 80, dir.to_str(), &id, None).expect("spawn 失败");
        thread::sleep(Duration::from_millis(800)); // 等 shell 启动输出沉淀

        // 注入 40 行，把内容顶进 scrollback（24 行屏高）。
        {
            let mut parser: Processor = Processor::new();
            let mut t = term.term.lock().unwrap();
            for i in 0..40 {
                parser.advance(&mut *t, format!("content-{i}\r\n").as_bytes());
            }
        }

        // 在可视区第 5 行选中前 9 列（"content-N" 长度 9）。
        term.selection_start(5, 0, true, SelectionKind::Simple);
        term.selection_update(5, 8, false);
        let text_before = term.selection_text().expect("建完选区应有文本");
        assert!(
            text_before.starts_with("content-"),
            "选到的应是注入的内容行，实际: {text_before:?}"
        );
        let frame = term.snapshot();
        let sel_row_before = frame.rows.iter().position(|r| r.iter().any(|c| c.selected));
        assert_eq!(sel_row_before, Some(5), "选区高亮应画在第 5 行");

        // 向上滚 3 行：高亮应跟着内容下移到第 8 行，文本不变。
        term.scroll(3);
        let frame = term.snapshot();
        let sel_row_after = frame.rows.iter().position(|r| r.iter().any(|c| c.selected));
        assert_eq!(
            sel_row_after,
            Some(8),
            "滚动 3 行后选区高亮应跟随内容移到第 8 行"
        );
        assert_eq!(
            term.selection_text().as_deref(),
            Some(text_before.as_str()),
            "滚动不该改变选区文本"
        );
        // 清理交给 _guard 的 Drop（含 panic 路径），不再手写。
    }

    /// 不依赖 uuid crate（terminal.rs 本身不需要它），用 pid+时间戳拼一个够唯一的 id。
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{}-{nanos}", std::process::id())
    }
}

/// 验证 EventProxy 会把 PtyWrite / ColorRequest 这类「终端该怎么回应」的事件真的
/// 写回去，不用起真实 shell/PTY——直接喂原始转义序列给 alacritty 的 Processor，
/// 用 UnixStream::pair() 在另一头当"假守护"读回应帧即可，快且不 flaky。
#[cfg(test)]
mod open_request_tests {
    use super::open_request;

    #[test]
    fn open_request_serializes_initial_launch() {
        let value = open_request(24, 80, Some("/tmp/project"), "sid-1", Some("claude"), true);
        assert_eq!(value["initial_launch"], "claude");
        assert_eq!(value["create_if_missing"], true);
        assert!(value.get("launch").is_none());
        assert!(value.get("missing").is_none());
    }

    #[test]
    fn reattach_request_does_not_allow_session_creation() {
        let value = open_request(24, 80, None, "sid-1", None, false);
        assert_eq!(value["create_if_missing"], false);
    }
}

#[cfg(test)]
mod successor_daemon_tests {
    use super::*;

    fn info(pid: Option<u32>, started_at: Option<u64>) -> DaemonInfo {
        DaemonInfo {
            pid,
            started_at,
            ..DaemonInfo::default()
        }
    }

    #[test]
    fn successor_is_a_new_pid_or_started_at() {
        let old = info(Some(10), Some(100));
        assert!(is_successor_daemon(
            Some(&old),
            Some(&info(Some(11), Some(100)))
        ));
        assert!(is_successor_daemon(
            Some(&old),
            Some(&info(Some(10), Some(200)))
        ));
        assert!(!is_successor_daemon(Some(&old), Some(&old)));
        assert!(!is_successor_daemon(Some(&old), None));
        assert!(is_successor_daemon(None, Some(&old)));
    }
}

#[cfg(test)]
mod install_commit_tests {
    use super::*;

    /// 核心回归：守护已在 managed 路径时安装只写 smeltd.next，不 handoff——
    /// 此前无条件 handoff，ACP 忙就 exit 75 半安装（映射写了、App 没换）。
    #[test]
    fn managed_daemon_stages_disk_only() {
        let managed = managed_daemon_path();
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&managed)),
            InstallCommitAction::StageDiskOnly
        );
        // next 暂存态仍在 managed 目录内：同样不得在安装时 handoff。
        let staged = managed_daemon_dir().join("smeltd.next");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&staged)),
            InstallCommitAction::StageDiskOnly
        );
    }

    /// 守护仍住在 .app 里：必须 handoff 迁出，否则换 App 会被 SIGKILL。
    #[test]
    fn app_resident_daemon_must_relocate() {
        let in_app = std::path::PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smeltd");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&in_app)),
            InstallCommitAction::RelocateHandoff
        );
        // 非 managed 的其它路径同样迁出。
        let elsewhere = std::path::PathBuf::from("/tmp/smeltd");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&elsewhere)),
            InstallCommitAction::RelocateHandoff
        );
    }

    /// 老守护无 exe 字段：无法证明已迁出，保守迁出（一次性，此后永久跳过）。
    #[test]
    fn unknown_exe_relocates_conservatively() {
        assert_eq!(
            install_commit_action_for_running_daemon(None),
            InstallCommitAction::RelocateHandoff
        );
    }

    #[test]
    fn staging_an_update_does_not_replace_the_live_managed_binary() {
        let dir = managed_daemon_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let live = managed_daemon_path();
        let staged = staged_daemon_path();
        std::fs::write(&live, b"running-image").unwrap();
        let src = std::env::temp_dir().join(format!(
            "smelt-stage-src-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&src, b"pending-image").unwrap();

        let dest = stage_managed_daemon_update(&src).unwrap();
        assert_eq!(dest, staged);
        assert_eq!(std::fs::read(&live).unwrap(), b"running-image");
        assert_eq!(std::fs::read(&staged).unwrap(), b"pending-image");
        assert_eq!(
            pending_upgrade_exe().as_deref(),
            Some(staged.as_path()),
            "空闲升级必须 exec smeltd.next，不能再 exec 正在跑的旧文件"
        );

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&live);
    }
}

#[cfg(test)]
mod outdated_fingerprint_tests {
    use super::*;

    fn fixture(content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "smelt-outdated-fp-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// 核心回归：StageDiskOnly 后磁盘是新的、进程指纹还是老的，必须判旧。
    /// 之前 `same_daemon_binary(running_exe_path, bundled)` 比的是两个磁盘新文件，
    /// 误判“不旧”，升级永不触发。
    #[test]
    fn staged_disk_with_old_process_is_outdated() {
        let disk_new = fixture(b"new-binary-bytes");
        let disk_old = fixture(b"old-binary-bytes");
        let pinned_old = smelt_plugin_host::executable_fingerprint(&disk_old).unwrap();
        assert!(
            outdated_by_fingerprint(&pinned_old, Some(&disk_new)),
            "磁盘新了、进程还是老的，必须判旧"
        );
        std::fs::remove_file(disk_new).ok();
        std::fs::remove_file(disk_old).ok();
    }

    /// 内容一致（仅 cp 造成 mtime 变化）不判旧：handoff 会闪断终端，不能误触。
    #[test]
    fn same_content_is_not_outdated() {
        let disk = fixture(b"same-binary-bytes");
        let pinned = smelt_plugin_host::executable_fingerprint(&disk).unwrap();
        assert!(!outdated_by_fingerprint(&pinned, Some(&disk)));
        std::fs::remove_file(disk).ok();
    }

    /// 期望文件缺失/不可读→保守不旧，避免误报打扰用户。
    #[test]
    fn missing_expected_file_is_not_outdated() {
        assert!(!outdated_by_fingerprint("abc123", None));
        assert!(!outdated_by_fingerprint(
            "abc123",
            Some(std::path::Path::new("/nonexistent/smeltd-xyz"))
        ));
    }
}

#[cfg(test)]
mod handshake_timeout_tests {
    use super::*;

    /// 复现启动卡死/崩溃链的第一环：守护接受了连接但一字不回（僵死、半退出、
    /// upgrade 交接卡住都会这样）。握手在 GUI 主线程同步跑，read_line 没有超时
    /// 就是永久 beachball。修复后应在 HANDSHAKE_READ_TIMEOUT 内返回 Err。
    /// 用 UnixStream::pair 当假守护：对端只收不回，且保持存活（drop 会变成 EOF，
    /// 测的就不是"不回话"了）。
    #[test]
    fn handshake_times_out_against_mute_daemon() {
        let (ours, theirs) = UnixStream::pair().expect("pair 失败");
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let r = Terminal::handshake_on(ours, 24, 80, None, "mute-daemon-test", None, true);
            let _ = tx.send(r.is_err());
        });
        match rx.recv_timeout(HANDSHAKE_READ_TIMEOUT + Duration::from_secs(2)) {
            Ok(is_err) => assert!(is_err, "不回话的守护应让握手失败，而不是握手成功"),
            Err(_) => panic!("握手对不回话的守护没有在超时窗口内返回——主线程会被永久卡死"),
        }
        drop(theirs); // 撑到断言之后才放，确保对端全程存活
    }

    #[test]
    fn handshake_rejects_attach_only_missing_session_response() {
        let (ours, mut theirs) = UnixStream::pair().expect("pair 失败");
        thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(theirs.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&request).unwrap()["create_if_missing"],
                false
            );
            writeln!(
                theirs,
                "{}",
                serde_json::json!({
                    "ok": false,
                    "err": "终端会话不存在",
                    "rows": 24,
                    "cols": 80,
                    "replay_len": 0,
                })
            )
            .unwrap();
        });

        let error = match Terminal::handshake_on(ours, 24, 80, None, "missing", None, false) {
            Ok(_) => panic!("attach-only 缺失会话必须失败"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("不存在"));
    }

    #[test]
    fn handshake_reads_daemon_color_request_capability() {
        let (ours, mut theirs) = UnixStream::pair().expect("pair 失败");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(theirs.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.contains("\"op\":\"open\""));
            writeln!(
                theirs,
                "{}",
                serde_json::json!({
                    "rows": 24,
                    "cols": 80,
                    "replay_len": 0,
                    "daemon_handles_color_requests": true,
                })
            )
            .unwrap();
            // `handshake_on` 成功前会清掉读超时；macOS 上若这里先关 socket，那个
            // setsockopt 可能报 EINVAL，测到的是夹具断链而不是能力位解析。
            let _ = release_rx.recv_timeout(Duration::from_secs(1));
        });

        let result = Terminal::handshake_on(ours, 24, 80, None, "color-capability", None, true);
        let _ = release_tx.send(());
        let (_buffered, _size, _replay, _geometry_token, daemon_handles_color_requests) =
            result.expect("能力位握手应成功");
        assert!(daemon_handles_color_requests);
    }

    /// 复现 2026-08-04 hang 报告（Smelt 0.6.9）的第二环：守护不再消费终端写
    /// socket 时，UI 主线程不能直接进入 `write_all`。假守护在握手后保持连接但
    /// 不再读，验证一次满额输入只入队并立即返回；实际 socket 阻塞仅允许发生在
    /// 专属 writer 线程。
    #[test]
    fn writer_queue_returns_without_waiting_for_mute_daemon() {
        let (mut daemon_side, client_side) = UnixStream::pair().expect("pair 失败");

        // 假守护线程不 join：断言窗口（2s）远小于其存活时长，进程退出时兜底清理。
        thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(daemon_side.try_clone().expect("clone 失败"));
            reader.read_line(&mut line).expect("应收到 open 请求行");
            let _ = daemon_side.write_all(b"{\"rows\":24,\"cols\":80,\"replay_len\":0}\n");
            // 之后保持连接打开但不再读：客户端写满发送缓冲后应阻塞（修复前）
            // / 超时返回（修复后）。
            thread::sleep(Duration::from_secs(60));
        });

        let (buffered, _size, _replay, _geometry_token, _daemon_handles_color_requests) =
            Terminal::handshake_on(client_side, 24, 80, None, "write-timeout-test", None, true)
                .expect("假守护正常回执，握手应成功");

        // 与 Terminal::spawn 同一条路径：后续写入进入后台单消费者队列。UI 线程只把
        // 4MB 输入请求投进去便返回；假守护不消费时，阻塞最多发生在 writer 线程。
        let writer = TerminalWriter::start(buffered.get_ref().try_clone().expect("clone 失败"))
            .expect("writer 启动失败");
        let payload = vec![0u8; TERMINAL_WRITE_QUEUE_MAX_BYTES];
        let started = std::time::Instant::now();
        assert!(
            writer.send_input(&payload),
            "健康队列应接受上限内的一次输入"
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "UI 投递输入不应等待 daemon 消费 socket"
        );
        writer.close();
    }
}

#[cfg(test)]
mod managed_daemon_ensure_tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;

    fn managed_test_file(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "smelt-managed-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn explicit_ensure_then_connect_fallback_only_ensures_once() {
        let ensured = AtomicBool::new(false);
        let calls = Cell::new(0);
        let managed = managed_test_file("cached");
        std::fs::write(&managed, b"daemon").unwrap();

        let first = remember_managed_daemon_ensure(
            {
                calls.set(calls.get() + 1);
                Ok(managed.clone())
            },
            &ensured,
        );
        let fallback = ensure_managed_daemon_for_connect(&ensured, managed.clone(), || {
            calls.set(calls.get() + 1);
            Ok(managed.clone())
        });

        assert_eq!(first.unwrap(), managed);
        assert_eq!(fallback.unwrap(), managed);
        assert_eq!(calls.get(), 1);
        assert!(ensured.load(Ordering::Relaxed));
        let _ = std::fs::remove_file(managed);
    }

    #[test]
    fn cached_ensure_runs_again_when_managed_binary_disappears() {
        let ensured = AtomicBool::new(true);
        let calls = Cell::new(0);
        let managed = managed_test_file("missing");

        let result = ensure_managed_daemon_for_connect(&ensured, managed.clone(), || {
            calls.set(calls.get() + 1);
            std::fs::write(&managed, b"restored")?;
            Ok(managed.clone())
        });

        assert_eq!(result.unwrap(), managed);
        assert_eq!(calls.get(), 1, "文件消失后不能继续盲信 ensure 缓存");
        let _ = std::fs::remove_file(managed);
    }

    #[test]
    fn daemon_promoted_staging_file_is_a_completed_install() {
        let root = managed_test_file("daemon-promoted-staging");
        let staged = root.join("smeltd.next");
        let managed = root.join("smeltd");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&managed, b"new daemon image").unwrap();

        finish_staged_managed_install(&staged, &managed)
            .expect("daemon 已自行提升到正式路径时 GUI 不应再报 rename 失败");
        assert_eq!(std::fs::read(&managed).unwrap(), b"new daemon image");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_staging_and_managed_files_is_not_a_completed_install() {
        let root = managed_test_file("missing-staging-and-managed");
        let staged = root.join("smeltd.next");
        let managed = root.join("smeltd");

        assert!(
            finish_staged_managed_install(&staged, &managed).is_err(),
            "两个路径都不存在时不能把真实安装丢失误判成 daemon 已自行提升"
        );
    }

    #[test]
    fn file_lock_serializes_managed_daemon_installers() {
        const INSTALLERS: usize = 8;
        let lock_path = managed_test_file("install-lock");
        let barrier = Arc::new(std::sync::Barrier::new(INSTALLERS));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut installers = Vec::new();

        for _ in 0..INSTALLERS {
            let lock_path = lock_path.clone();
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            installers.push(thread::spawn(move || {
                barrier.wait();
                let _lock = acquire_file_lock(&lock_path).expect("安装锁获取失败");
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for installer in installers {
            installer.join().unwrap();
        }
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "跨执行上下文的 managed 安装必须完全串行"
        );
        let _ = std::fs::remove_file(lock_path);
    }

    #[test]
    fn app_bundle_plugins_are_not_in_codesign_nested_bundle_dirs() {
        let root = app_bundle_plugin_root(std::path::Path::new(
            "/Applications/Smelt.app/Contents/MacOS/smelt",
        ))
        .expect("GUI 在 .app/Contents/MacOS 下必须能定位 bundled 插件");
        assert_eq!(
            root,
            std::path::PathBuf::from("/Applications/Smelt.app/Contents/Resources/plugin-packages")
        );
        assert!(
            !root.components().any(|c| c.as_os_str() == "PlugIns"),
            "Contents/PlugIns 会被 codesign 当成嵌套 bundle：{root:?}"
        );
    }

    #[test]
    fn candidate_release_selects_its_plugin_set_before_handoff() {
        let root = managed_test_file("candidate-release");
        let candidate = root.join("Smelt.app");
        let daemon = candidate.join("Contents/MacOS/smeltd");
        let package = candidate
            .join("Contents")
            .join(APP_BUNDLE_PLUGIN_PACKAGES)
            .join("com.example");
        let entrypoint = package.join("bin/main.ts");
        let smelt_root = root.join("state");
        std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        std::fs::create_dir_all(daemon.parent().unwrap()).unwrap();
        std::fs::write(&daemon, b"candidate-daemon").unwrap();
        std::fs::write(&entrypoint, b"export default {};\n").unwrap();
        std::fs::write(
            package.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/main.ts",
                "capabilities": []
            })
            .to_string(),
        )
        .unwrap();

        let prepared_daemon = prepare_bundled_release(&candidate, &smelt_root).unwrap();

        assert_eq!(prepared_daemon, daemon);
        let selected = smelt_plugin_host::active_plugin_set_root(&smelt_root, &prepared_daemon)
            .unwrap()
            .expect("handoff 前必须已经选中插件集合");
        assert!(selected.join("com.example/plugin.json").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_app_cli_is_opt_in() {
        assert!(maybe_run_install_app(["smelt"]).is_none());
        assert!(maybe_run_install_app(["smelt", "--help"]).is_none());
        assert_eq!(crate::cli::maybe_run(["smelt", "--help"]), Some(0));
        assert_eq!(maybe_run_install_app(["smelt", "--install-app"]), Some(2));
    }

    #[test]
    fn cargo_layout_stages_every_bundled_plugin_it_finds() {
        use std::os::unix::fs::PermissionsExt;

        // 复刻 cargo 的布局：<root>/plugins 是插件源，<root>/target/debug 是产物。
        let root = managed_test_file("dev-plugins");
        let bin_dir = root.join("target/debug");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let gui = bin_dir.join("smelt");
        std::fs::write(&gui, b"gui").unwrap();

        let write_manifest = |name: &str, id: &str, entrypoint: &str, bundled: bool| {
            let dir = root.join("plugins").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("plugin.json"),
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "version": "1.0.0",
                    "api_version": 1,
                    "entrypoint": format!("bin/{entrypoint}"),
                    "capabilities": [],
                    "bundled": bundled,
                })
                .to_string(),
            )
            .unwrap();
            dir
        };
        let write_entrypoint = |dir: &std::path::Path, name: &str| {
            let path = dir.join("bin").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"export default {};\n").unwrap();
        };

        // 有入口、带 web 的插件：整包（含网页）都要被装进去。
        let alpha = write_manifest("alpha", "com.example.alpha", "alpha-bin", true);
        std::fs::create_dir_all(alpha.join("web")).unwrap();
        std::fs::write(alpha.join("web/index.html"), b"<!doctype html>").unwrap();
        std::fs::write(
            alpha.join(smelt_plugin_api::PLUGIN_UI_MANIFEST_FILE),
            serde_json::json!({ "contributions": [] }).to_string(),
        )
        .unwrap();
        std::fs::write(
            alpha.join(smelt_plugin_api::PLUGIN_INPUT_MANIFEST_FILE),
            serde_json::json!({ "contributions": [] }).to_string(),
        )
        .unwrap();
        std::fs::write(
            alpha.join(smelt_plugin_api::PLUGIN_AGENT_MANIFEST_FILE),
            serde_json::json!({ "contributions": [] }).to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(alpha.join("assets")).unwrap();
        std::fs::write(alpha.join("assets/icon.svg"), b"<svg/>").unwrap();
        write_entrypoint(&alpha, "alpha-bin");
        // 测试用插件：不进产物。
        let beta = write_manifest("beta", "com.example.beta", "beta-bin", false);
        write_entrypoint(&beta, "beta-bin");
        // 缺入口的插件：跳过而不是让整个启动失败。
        write_manifest("gamma", "com.example.gamma", "gamma-bin", true);
        // Shared Bun 的入口是包内数据，不能要求 target/debug 里存在同名二进制。
        let bun = root.join("plugins/bun");
        std::fs::create_dir_all(bun.join("bin")).unwrap();
        std::fs::write(
            bun.join("plugin.json"),
            serde_json::json!({
                "id": "com.example.bun",
                "name": "Bun",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/main.ts",
                "capabilities": [],
                "contributions": [],
                "bundled": true,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            bun.join("bin/main.ts"),
            "export { default } from './api.ts';",
        )
        .unwrap();
        std::fs::write(
            bun.join("bin/api.ts"),
            "export default { invoke() { return null; } };",
        )
        .unwrap();
        std::fs::write(bun.join("bin/api.test.ts"), "throw new Error('test');").unwrap();

        let staged = bundled_plugin_root_for(&gui).expect("开发态必须能组装插件包");
        assert!(staged.join("com.example.alpha/plugin.json").is_file());
        assert!(staged.join("com.example.alpha/bin/alpha-bin").is_file());
        assert!(
            staged
                .join("com.example.alpha")
                .join(smelt_plugin_api::PLUGIN_UI_MANIFEST_FILE)
                .is_file(),
            "UI sidecar 必须跟包一起 stage"
        );
        assert!(
            staged
                .join("com.example.alpha")
                .join(smelt_plugin_api::PLUGIN_INPUT_MANIFEST_FILE)
                .is_file(),
            "输入路由 sidecar 必须跟包一起 stage"
        );
        assert!(
            staged
                .join("com.example.alpha")
                .join(smelt_plugin_api::PLUGIN_AGENT_MANIFEST_FILE)
                .is_file(),
            "智能体 sidecar 必须跟包一起 stage"
        );
        assert!(
            staged.join("com.example.alpha/assets/icon.svg").is_file(),
            "智能体等 contribution 的包内资源必须跟包一起 stage"
        );
        assert!(
            staged.join("com.example.alpha/web/index.html").is_file(),
            "面板资源必须跟包一起 stage，否则装了也打不开"
        );
        assert!(
            !staged.join("com.example.beta").exists(),
            "bundled=false 的插件不该进产物"
        );
        assert!(
            !staged.join("com.example.gamma").exists(),
            "缺少 package entrypoint 的插件应当跳过"
        );
        assert!(
            staged.join("com.example.bun/bin/main.ts").is_file(),
            "Shared Bun 入口必须从 package 源目录 stage"
        );
        assert!(
            staged.join("com.example.bun/bin/api.ts").is_file(),
            "Bun 入口旁边的模块必须一起 stage，否则运行时 import 会失败"
        );
        assert!(
            !staged.join("com.example.bun/bin/api.test.ts").exists(),
            "测试文件不能进运行时插件包"
        );
        assert_eq!(
            std::fs::metadata(staged.join("com.example.bun/bin/main.ts"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0,
            "Shared Bun module must stay non-executable package data"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_spawn_strips_host_color_suppression() {
        let mut cmd = std::process::Command::new("/bin/true");
        cmd.env("NO_COLOR", "1");
        cmd.env("FORCE_COLOR", "0");
        smelt_paths::export_to(&mut cmd);
        smelt_core::tty_color::clear_command(&mut cmd);
        let present: Vec<String> = cmd
            .get_envs()
            .filter_map(|(key, value)| {
                value?;
                key.to_str().map(str::to_string)
            })
            .collect();
        for key in smelt_core::tty_color::SUPPRESSION_VARS {
            assert!(
                !present.iter().any(|k| k == key),
                "拉起 smeltd 不得带上 {key}，实际: {present:?}"
            );
        }
    }
}

#[cfg(test)]
mod event_proxy_answers_tests {
    use super::*;

    fn make_proxy() -> (EventProxy, UnixStream) {
        let (probe, sock) = UnixStream::pair().expect("pair 失败");
        probe
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let proxy = EventProxy {
            bell_notify: Arc::new(Mutex::new(None)),
            title: Arc::new(Mutex::new(None)),
            writer: TerminalWriter::start(sock).expect("writer 启动失败"),
            metrics: Arc::new(Mutex::new(TermMetrics {
                rows: 24,
                cols: 80,
                cell_w: 8,
                cell_h: 16,
            })),
            daemon_handles_color_requests: false,
        };
        (proxy, probe)
    }

    /// 读一帧 [type:u8][len:u32 BE][payload] 并返回 (type, payload 字符串)。
    fn read_frame(probe: &mut UnixStream) -> (u8, String) {
        let mut header = [0u8; 5];
        probe
            .read_exact(&mut header)
            .expect("应该收到回应帧，说明 PtyWrite 被丢了");
        let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        probe
            .read_exact(&mut payload)
            .expect("帧头声明的长度和实际 payload 对不上");
        (
            header[0],
            String::from_utf8(payload).expect("回应应该是纯文本转义序列"),
        )
    }

    /// `ESC[6n`（Cursor Position Report 查询）：alacritty 解析后应该通过
    /// Event::PtyWrite 吐出 `ESC[row;colR`，之前这个事件被 `_ => {}` 吞掉，
    /// Claude Code 输入框那类依赖精确光标定位的渲染（ghost-text 补全）就拿不到
    /// 位置信息。
    #[test]
    fn cursor_position_query_gets_answered() {
        let (proxy, mut probe) = make_proxy();
        let size = TermSize { rows: 24, cols: 80 };
        let mut term = Term::new(Config::default(), &size, proxy);
        let mut parser: Processor = Processor::new();

        parser.advance(&mut term, b"\x1b[6n");

        let (ty, resp) = read_frame(&mut probe);
        assert_eq!(ty, 0, "回应要走 type=0（PTY 输入）帧，跟键盘输入同一条路");
        assert!(
            resp.starts_with("\x1b[") && resp.ends_with('R'),
            "应为 ESC[row;colR 格式的光标位置回应，实际收到: {resp:?}"
        );
    }

    /// 兼容旧守护：`OSC 11 ?`（查询当前背景色）仍要由客户端兜底，回应里应带上当前主题的
    /// 默认背景色（深色下 `bg_stage` = `0x070707`，跟随卡片配色）而不是空/无回应。
    #[test]
    fn background_color_query_gets_answered() {
        let _guard = lock_theme_globals();
        set_dark_mode(true); // 全局态，跟其它测试共进程跑，显式定住深色断言的前提
        set_bg_override(None);
        let (proxy, mut probe) = make_proxy();
        let size = TermSize { rows: 24, cols: 80 };
        let mut term = Term::new(Config::default(), &size, proxy);
        let mut parser: Processor = Processor::new();

        parser.advance(&mut term, b"\x1b]11;?\x07");

        let (ty, resp) = read_frame(&mut probe);
        assert_eq!(ty, 0);
        assert!(
            resp.contains("rgb:0707/0707/0707"),
            "应含默认背景色（bg_stage 深色 0x070707）的 rgb 十六进制，实际: {resp:?}"
        );
    }

    #[test]
    fn daemon_owned_color_query_is_not_answered_twice_by_client() {
        let (mut proxy, mut probe) = make_proxy();
        proxy.daemon_handles_color_requests = true;
        probe.set_nonblocking(true).unwrap();
        let size = TermSize { rows: 24, cols: 80 };
        let mut term = Term::new(Config::default(), &size, proxy);
        let mut parser: Processor = Processor::new();

        parser.advance(&mut term, b"\x1b]11;?\x07");

        let mut byte = [0u8; 1];
        let error = probe
            .read(&mut byte)
            .expect_err("新版守护已应答时客户端不该再写一份");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }

    /// 用户在设置里自选了终端底色时，OSC 11 必须回**用户那个色**：TUI 就是靠这个
    /// 回应挑灰度的，回主题色而渲染成用户色，等于让 TUI 按错的底色配色。
    #[test]
    fn background_color_query_reports_user_override() {
        let _guard = lock_theme_globals();
        set_dark_mode(true);
        set_bg_override(Some(0x00ab_cdef));
        assert_eq!(default_bg(), 0x00ab_cdef);

        let (proxy, mut probe) = make_proxy();
        let size = TermSize { rows: 24, cols: 80 };
        let mut term = Term::new(Config::default(), &size, proxy);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b]11;?\x07");
        let (_, resp) = read_frame(&mut probe);
        assert!(
            resp.contains("rgb:abab/cdcd/efef"),
            "应回用户自选底色 0xabcdef，实际: {resp:?}"
        );

        set_bg_override(None);
        assert_eq!(
            default_bg(),
            crate::ui_theme::bg_stage(),
            "清掉自选色后应回到跟随主题"
        );
    }

    /// TextAreaSizeRequest：应用查文本区尺寸时必须按当前 metrics 回应，不能吞掉。
    #[test]
    fn text_area_size_request_gets_answered() {
        let (proxy, mut probe) = make_proxy();
        // 直接发事件（不经 parser）：验证 EventProxy 分支真的写帧。
        use alacritty_terminal::event::WindowSize;
        proxy.send_event(Event::TextAreaSizeRequest(std::sync::Arc::new(
            |ws: WindowSize| {
                format!(
                    "{}x{}@{}x{}",
                    ws.num_cols, ws.num_lines, ws.cell_width, ws.cell_height
                )
            },
        )));
        let (ty, resp) = read_frame(&mut probe);
        assert_eq!(ty, 0);
        assert_eq!(resp, "80x24@8x16", "应回 metrics 里的 80×24 格、8×16 像素");
    }

    /// Shift+Enter 能不能换行，全押在这个位上：TUI 进入时发 `CSI > 1 u` 开 kitty keyboard
    /// protocol，alacritty 要把它解析成 TermMode::DISAMBIGUATE_ESC_CODES，keystroke_to_bytes
    /// 才会改发 CSI u 编码。退出时发 `CSI < u` 弹栈还原——还原不掉的话，TUI 退出后普通
    /// shell 里按 Shift+Enter 就会被吐出 `[13;2u` 乱码。
    #[test]
    fn kitty_keyboard_protocol_toggles_disambiguate_mode() {
        let (proxy, _probe) = make_proxy();
        let size = TermSize { rows: 24, cols: 80 };
        // 跟 Terminal::spawn 用同一份 config：kitty_keyboard 关着的话 alacritty 会把
        // CSI u 全静默丢掉，这个测试就成了摆设。
        let mut term = Term::new(term_config(), &size, proxy);
        let mut parser: Processor = Processor::new();

        assert!(
            !term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES),
            "默认不该开着——普通 shell 不认 CSI u"
        );

        parser.advance(&mut term, b"\x1b[>1u"); // Claude Code v2.1+ 启动时发这个
        assert!(
            term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES),
            "收到 CSI > 1 u 后应置位，否则 Shift+Enter 永远退化成裸 Enter"
        );

        parser.advance(&mut term, b"\x1b[<u"); // 退出时弹栈
        assert!(
            !term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES),
            "TUI 退出后应还原，否则 shell 里 Shift+Enter 会吐出 `[13;2u` 乱码"
        );
    }
}

#[cfg(test)]
mod bell_notification_tests {
    use super::*;

    #[test]
    fn bell_uses_its_own_notification_slot() {
        let (sock, _peer) = UnixStream::pair().expect("创建 socket pair");
        let bell_notify = Arc::new(Mutex::new(None));
        let proxy = EventProxy {
            bell_notify: Arc::clone(&bell_notify),
            title: Arc::new(Mutex::new(None)),
            writer: TerminalWriter::start(sock).expect("writer 启动失败"),
            metrics: Arc::new(Mutex::new(TermMetrics {
                rows: 24,
                cols: 80,
                cell_w: 8,
                cell_h: 16,
            })),
            daemon_handles_color_requests: false,
        };

        proxy.send_event(Event::Bell);

        assert_eq!(
            bell_notify.lock().unwrap().take().as_deref(),
            Some("🔔 响铃")
        );
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;
    use std::io::{ErrorKind, Read};

    fn make_terminal_with_peer(rows: usize, cols: usize) -> (Terminal, UnixStream) {
        let (peer, sock) = UnixStream::pair().expect("pair 失败");
        peer.set_read_timeout(Some(Duration::from_millis(200)))
            .expect("设置读超时失败");
        let writer = TerminalWriter::start(sock).expect("writer 启动失败");
        let metrics = Arc::new(Mutex::new(TermMetrics {
            rows: rows as u16,
            cols: cols as u16,
            cell_w: 8,
            cell_h: 16,
        }));
        let proxy = EventProxy {
            bell_notify: Arc::new(Mutex::new(None)),
            title: Arc::new(Mutex::new(None)),
            writer: writer.clone(),
            metrics: metrics.clone(),
            daemon_handles_color_requests: false,
        };
        let size = TermSize { rows, cols };
        let term = Term::new(term_config(), &size, proxy);
        let (_, redraw_rx) = smol::channel::bounded::<()>(1);
        let (search_result_tx, search_result_rx) = smol::channel::unbounded();
        (
            Terminal {
                term: Arc::new(Mutex::new(term)),
                writer,
                size,
                metrics,
                daemon_geometry: Arc::new(Mutex::new(DaemonGeometrySignal::default())),
                daemon_geometry_generation: 0,
                remote_geometry_locked: false,
                notify: Arc::new(Mutex::new(None)),
                bell_notify: Arc::new(Mutex::new(None)),
                title: Arc::new(Mutex::new(None)),
                last_damage_cursor: Mutex::new(None),
                search_query: Mutex::new(String::new()),
                search_matches: Mutex::new(Vec::new()),
                search_index: Mutex::new(0),
                search_result_tx,
                search_result_rx,
                search_generation: Mutex::new(0),
                search_matches_top: Mutex::new(0),
                last_search_rescan: Mutex::new(Instant::now()),
                pending_step: Mutex::new(None),
                dead: Arc::new(AtomicBool::new(false)),
                redraw_rx,
            },
            peer,
        )
    }

    fn read_resize(peer: &mut UnixStream) -> [u32; 4] {
        let mut header = [0u8; 5];
        peer.read_exact(&mut header).expect("应收到 resize 帧");
        assert_eq!(header[0], 1, "应为 type=1 resize 帧");
        let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        assert_eq!(len, 16, "resize 帧应包含行列和 cell 像素");
        let mut payload = [0u8; 16];
        peer.read_exact(&mut payload)
            .expect("resize payload 不完整");
        std::array::from_fn(|index| {
            u32::from_be_bytes(payload[index * 4..index * 4 + 4].try_into().unwrap())
        })
    }

    fn assert_no_frame(peer: &mut UnixStream) {
        let mut byte = [0u8; 1];
        match peer.read(&mut byte) {
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
            Ok(0) => panic!("对端提前关闭，无法判断是否多发 resize"),
            Ok(_) => panic!("同尺寸 resize 不应再次发送帧"),
            Err(error) => panic!("读取 resize 帧失败: {error}"),
        }
    }

    #[test]
    fn resize_only_sends_when_grid_or_cell_changes() {
        let (mut terminal, mut peer) = make_terminal_with_peer(24, 80);

        terminal.resize(24, 80, 8, 16);
        assert_no_frame(&mut peer);

        terminal.resize(25, 80, 8, 16);
        assert_eq!(read_resize(&mut peer), [80, 25, 8, 16]);

        terminal.resize(25, 80, 8, 16);
        assert_no_frame(&mut peer);

        terminal.resize(25, 80, 9, 16);
        assert_eq!(read_resize(&mut peer), [80, 25, 9, 16]);
    }
}

#[cfg(test)]
mod paste_encode_tests {
    use super::encode_paste;

    #[test]
    fn plain_paste_normalizes_newlines_to_cr() {
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_wraps_and_strips_esc() {
        let out = encode_paste("hi\x1b[31m", true);
        assert_eq!(out, b"\x1b[200~hi[31m\x1b[201~");
    }
}

#[cfg(test)]
mod attachment_cleanup_tests {
    use super::TerminalWriter;
    use super::{TERMINAL_FRAME_MAX_BYTES, TERMINAL_WRITE_QUEUE_MAX_BYTES};
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn closing_writer_closes_peer_stream() {
        let (writer, mut peer) = UnixStream::pair().expect("pair 失败");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("设置读超时失败");

        let writer = TerminalWriter::start(writer).expect("writer 启动失败");
        writer.close();

        let mut byte = [0; 1];
        assert_eq!(peer.read(&mut byte).expect("peer 读取失败"), 0);
    }

    #[test]
    fn large_input_is_chunked_across_queue_budget() {
        let (mut peer, writer_side) = UnixStream::pair().expect("pair 失败");
        let writer = TerminalWriter::start(writer_side).expect("writer 启动失败");
        let expected = TERMINAL_WRITE_QUEUE_MAX_BYTES + TERMINAL_FRAME_MAX_BYTES / 2 + 17;
        let payload = vec![0x5a; expected];
        let (done_tx, done_rx) = mpsc::channel();

        std::thread::spawn(move || {
            let mut received = 0usize;
            loop {
                let mut header = [0u8; 5];
                if peer.read_exact(&mut header).is_err() {
                    return;
                }
                let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
                assert!(len <= TERMINAL_FRAME_MAX_BYTES, "writer 不得发送超限帧");
                let mut frame = vec![0u8; len];
                if peer.read_exact(&mut frame).is_err() {
                    return;
                }
                received += len;
                if received >= expected {
                    let _ = done_tx.send(received);
                    return;
                }
            }
        });

        assert!(
            writer.send_input(&payload),
            "大粘贴不应因总长度超过队列预算失败"
        );
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("应收到完整的大粘贴"),
            expected
        );
        writer.close();
    }
}

#[cfg(test)]
mod search_resync_tests {
    use super::*;

    /// 搭一个不连守护的 Terminal：假 UnixStream 写端 + 直接构造的 Term。
    /// 专测搜索坐标——不 spawn shell，无时序依赖，不 flaky。
    fn make_terminal(rows: usize, cols: usize) -> Terminal {
        let (_probe, sock) = UnixStream::pair().expect("pair 失败");
        let writer =
            TerminalWriter::start(sock.try_clone().expect("clone 失败")).expect("writer 启动失败");
        let (search_result_tx, search_result_rx) = smol::channel::unbounded();
        let metrics = Arc::new(Mutex::new(TermMetrics {
            rows: rows as u16,
            cols: cols as u16,
            cell_w: 8,
            cell_h: 16,
        }));
        let proxy = EventProxy {
            bell_notify: Arc::new(Mutex::new(None)),
            title: Arc::new(Mutex::new(None)),
            writer: writer.clone(),
            metrics: metrics.clone(),
            daemon_handles_color_requests: false,
        };
        let term = Term::new(term_config(), &TermSize { rows, cols }, proxy);
        Terminal {
            term: Arc::new(Mutex::new(term)),
            writer,
            size: TermSize { rows, cols },
            metrics,
            daemon_geometry: Arc::new(Mutex::new(DaemonGeometrySignal::default())),
            daemon_geometry_generation: 0,
            remote_geometry_locked: false,
            notify: Arc::new(Mutex::new(None)),
            bell_notify: Arc::new(Mutex::new(None)),
            title: Arc::new(Mutex::new(None)),
            last_damage_cursor: Mutex::new(None),
            search_query: Mutex::new(String::new()),
            search_matches: Mutex::new(Vec::new()),
            search_index: Mutex::new(0),
            search_result_tx,
            search_result_rx,
            search_generation: Mutex::new(0),
            search_matches_top: Mutex::new(0),
            last_search_rescan: Mutex::new(Instant::now()),
            pending_step: Mutex::new(None),
            dead: Arc::new(AtomicBool::new(false)),
            // 测试不驱动 UI 重绘，给一个即时关闭的通道占位即可。
            redraw_rx: {
                let (_, rx) = smol::channel::bounded::<()>(1);
                rx
            },
        }
    }

    /// 测试辅助：等后台搜索结果落地（模拟 UI 的 poll 循环）。
    fn wait_search(t: &mut Terminal) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !t.poll_search_results() {
            assert!(Instant::now() < deadline, "search result timeout");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn daemon_geometry_resizes_local_grid_and_tracks_remote_lease() {
        let mut terminal = make_terminal(59, 181);
        {
            let mut signal = terminal.daemon_geometry.lock().unwrap();
            signal.generation = 1;
            signal.geometry = Some(smelt_core::osc::TerminalGeometryOsc {
                cols: 49,
                rows: 47,
                cell_width: 8,
                cell_height: 15,
                remote_controlled: true,
            });
        }

        assert!(terminal.sync_daemon_geometry());
        assert_eq!((terminal.size.cols, terminal.size.rows), (49, 47));
        assert!(terminal.remote_geometry_locked());
        let metrics = terminal.metrics.lock().unwrap();
        assert_eq!((metrics.cols, metrics.rows), (49, 47));
        assert_eq!((metrics.cell_w, metrics.cell_h), (8, 15));
    }

    /// 直接往 Term 喂字节（等价于读线程收到 PTY 输出）。
    fn feed(t: &Terminal, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        if let Ok(mut term) = t.term.lock() {
            parser.advance(&mut *term, bytes);
        }
    }

    /// 取一条命中在当前视口网格上盖住的实际文本——高亮画在哪，就该是什么字。
    fn hit_text(t: &Terminal, hit: &SearchHit) -> String {
        let term = t.term.lock().unwrap();
        let offset = term.grid().display_offset();
        (hit.col_start..=hit.col_end)
            .map(|c| term.grid()[viewport_to_point(offset, Point::new(hit.row, Column(c)))].c)
            .collect()
    }

    /// 截图 bug 复现：搜索后日志继续滚，缓存的命中坐标整体过期（每滚一行
    /// 全部 Line -1），高亮落在无关文本上（搜 schwab 却高亮 HTTP/1）。
    /// 命中行滚出可视区后就不该再画任何高亮。
    #[test]
    fn stale_hits_vanish_after_scroll() {
        let mut t = make_terminal(4, 20);
        feed(&t, b"one needle here\r\nfill-a\r\nfill-b\r\nfill-c");
        t.set_search_query("needle");
        wait_search(&mut t);
        let st = t.search_status();
        assert_eq!((st.current, st.total), (1, 1));
        let hits = t.viewport_search_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hit_text(&t, &hits[0]),
            "needle",
            "滚动前高亮就该在命中文本上"
        );

        // 新输出滚 2 行：needle 行进 scrollback，贴底视口里已没有命中
        feed(&t, b"\r\nnew-1\r\nnew-2");
        let hits = t.viewport_search_hits();
        assert!(
            hits.is_empty(),
            "命中已滚出可视区，不该再画高亮（旧坐标落在 {:?}）",
            hits.first().map(|h| hit_text(&t, h))
        );
    }

    /// 回看历史时高亮必须跟着内容走：滚动后命中的视口位置变了，重算要对准。
    #[test]
    fn hits_track_content_into_scrollback() {
        let mut t = make_terminal(4, 20);
        feed(&t, b"one needle here\r\nfill-a\r\nfill-b\r\nfill-c");
        t.set_search_query("needle");
        wait_search(&mut t);
        feed(&t, b"\r\nnew-1\r\nnew-2");

        t.set_scroll_offset(2); // 回看到最初 4 行，needle 应在视口第 0 行
        let hits = t.viewport_search_hits();
        assert_eq!(hits.len(), 1, "回看后 needle 回到可视区");
        assert_eq!(hit_text(&t, &hits[0]), "needle", "高亮必须落在命中文本上");
        assert_eq!(hits[0].row, 0);
    }

    /// 同一查询按「下一个」：搜索之后新输出里的命中也要被看见。
    #[test]
    fn find_next_picks_up_new_matches() {
        let mut t = make_terminal(4, 20);
        feed(&t, b"one needle here\r\nfill-a");
        t.find_next("needle", false);
        wait_search(&mut t); // 查询变了：异步重建 + 待步进，结果落地后从首条起跳
        let st = t.search_status();
        assert_eq!((st.current, st.total), (1, 1));

        feed(&t, b"\r\nsecond needle x");
        // 模拟 refresh_search_highlights：内容变化触发重扫（等节流窗口过去）。
        std::thread::sleep(SEARCH_RESCAN_THROTTLE);
        t.set_search_query("needle");
        wait_search(&mut t);
        let st = t.find_next("needle", false);
        assert_eq!(st.total, 2, "新输出里的命中必须被看见");
        assert_eq!(st.current, 2);
    }
}

#[cfg(test)]
mod mouse_encode_tests {
    use super::encode_mouse;
    use alacritty_terminal::term::TermMode;

    #[test]
    fn sgr_press_release_and_drag() {
        let sgr = TermMode::SGR_MOUSE | TermMode::MOUSE_MODE;
        assert_eq!(encode_mouse(sgr, 0, true, 2, 4), b"\x1b[<0;5;3M");
        assert_eq!(encode_mouse(sgr, 0, false, 2, 4), b"\x1b[<0;5;3m");
        assert_eq!(encode_mouse(sgr, 32, true, 2, 4), b"\x1b[<32;5;3M");
    }
}

#[cfg(test)]
mod scroll_wheel_plan_tests {
    use super::{ScrollWheelPlan, scroll_wheel_plan};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn primary_screen_default_modes_scroll_local_history() {
        // 回归：alacritty 默认含 ALTERNATE_SCROLL；主屏滚轮绝不能发 ^[[A 进 shell。
        let mode = TermMode::default();
        assert!(
            mode.contains(TermMode::ALTERNATE_SCROLL),
            "前提：默认应含 ALTERNATE_SCROLL，否则测不到回归点"
        );
        assert!(!mode.contains(TermMode::ALT_SCREEN));
        assert_eq!(
            scroll_wheel_plan(mode, 3, 1, 2),
            ScrollWheelPlan::LocalHistory(3)
        );
        assert_eq!(
            scroll_wheel_plan(mode, -2, 1, 2),
            ScrollWheelPlan::LocalHistory(-2)
        );
    }

    #[test]
    fn mouse_mode_sgr_forwards_wheel_to_app() {
        let mode = TermMode::SGR_MOUSE | TermMode::MOUSE_MOTION | TermMode::ALT_SCREEN;
        match scroll_wheel_plan(mode, 1, 4, 8) {
            ScrollWheelPlan::Send(b) => {
                assert_eq!(b, b"\x1b[<64;9;5M");
            }
            other => panic!("期望 Send SGR 滚轮，got {other:?}"),
        }
        match scroll_wheel_plan(mode, -1, 4, 8) {
            ScrollWheelPlan::Send(b) => assert_eq!(b, b"\x1b[<65;9;5M"),
            other => panic!("期望 Send 下滚，got {other:?}"),
        }
    }

    #[test]
    fn alt_screen_without_local_mouse_still_sends_sgr() {
        // reattach 本地丢 mouse 位：仍发 SGR（进程侧跟踪通常还在）；不滚本地 history。
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        match scroll_wheel_plan(mode, 1, 0, 0) {
            ScrollWheelPlan::Send(b) => assert_eq!(b, b"\x1b[<64;1;1M"),
            other => panic!("备用屏应发 SGR 滚轮，got {other:?}"),
        }
        match scroll_wheel_plan(mode, -1, 3, 5) {
            ScrollWheelPlan::Send(b) => assert_eq!(b, b"\x1b[<65;6;4M"),
            other => panic!("下滚 SGR，got {other:?}"),
        }
    }
}

// OSC 通知解析单测见 crate::osc（workspace 与 smeltd 共用）。

#[cfg(test)]
mod search_literal_tests {
    use super::{SearchHit, escape_regex_literal, match_to_viewport_hit};
    use alacritty_terminal::index::{Column, Line, Point};

    #[test]
    fn escapes_regex_metachars_for_literal_search() {
        assert_eq!(escape_regex_literal("a.b*c"), r"a\.b\*c");
        assert_eq!(escape_regex_literal("plain"), "plain");
    }

    #[test]
    fn match_maps_into_viewport_with_offset() {
        // 缓冲 line=-5，offset=5 → 可视第 0 行
        let start = Point::new(Line(-5), Column(3));
        let end = Point::new(Line(-5), Column(7));
        let hit = match_to_viewport_hit(start, end, 5, 80, true).unwrap();
        assert_eq!(
            hit,
            SearchHit {
                row: 0,
                col_start: 3,
                col_end: 7,
                active: true,
            }
        );
        // offset 不够 → 仍在历史上方，不可见
        assert!(match_to_viewport_hit(start, end, 2, 80, false).is_none());
    }
}
