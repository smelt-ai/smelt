//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! 本轮：内嵌真终端（portable-pty + alacritty_terminal），起你的 shell、
//! 实时渲染输出、并支持键盘输入。颜色 / 多标签 / resize 留到后续增量。
//!
//! 运行： cargo run --bin workspace

mod terminal;

use std::ops::Range;
use std::time::Duration;

use gpui::*;
use gpui_component::*;
use smol::Timer;
use terminal::Terminal;

/// 终端网格刷新间隔（后台线程在更新，UI 定时快照重绘）。
const REFRESH: Duration = Duration::from_millis(30);

/// 终端字体与网格度量（渲染与行列计算共用，保持一致）。
const FONT_PX: f32 = 13.0;
const LINE_PX: f32 = 18.0;
/// 等宽字宽 ≈ 字号 × 该比例（用于从窗口宽度估算列数）。
const CELL_W_RATIO: f32 = 0.6;
/// 估算的边距 / 标题栏高度，用于从窗口尺寸推算可用网格区域。
const PAD_X: f32 = 16.0;
const PAD_Y: f32 = 16.0;
const TITLEBAR_H: f32 = 40.0;

/// 工作台根视图：一个内嵌终端。
struct Workspace {
    terminal: Terminal,
    focus_handle: FocusHandle,
    did_focus: bool,
    /// 输入法合成中的预编辑文本（未提交），仅用于满足 IME 协议，不发给 PTY。
    marked_text: Option<String>,
}

impl Workspace {
    fn new(cx: &mut Context<Self>) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(String::from));
        let terminal = Terminal::spawn(24, 80, cwd.as_deref()).expect("启动内嵌终端失败");

        // 定时重绘：后台读线程更新 Term 网格，这里每 30ms 通知 UI 刷新。
        cx.spawn(async move |this, cx| loop {
            Timer::after(REFRESH).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break; // 视图已销毁
            }
        })
        .detach();

        Self {
            terminal,
            focus_handle: cx.focus_handle(),
            did_focus: false,
            marked_text: None,
        }
    }
}

/// 输入法（IME）支持：中文等需要合成的输入走这里，最终提交的文字通过
/// replace_text_in_range 回调进来，写入 PTY。英文/可打印字符同样经此路径。
impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        self.marked_text.clone()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        None
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
        // 提交：清掉预编辑，把文字写进 PTY。
        self.marked_text = None;
        if !text.is_empty() {
            self.terminal.send_input(text.as_bytes());
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
        // 合成中：只记录预编辑文本，不发给 PTY。
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
        // 候选窗定位：先放到元素左上角附近（后续可精确到光标像素位置）。
        Some(Bounds {
            origin: element_bounds.origin,
            size: size(px(2.0), px(LINE_PX)),
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

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 首帧把焦点抢到终端上，之后不再干预（用户可自由切换焦点）。
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }

        // 依据窗口尺寸重算终端行列，并 resize 网格 + PTY（无变化则内部跳过）。
        // 等宽字符宽度按字号的 ~0.6 估算（够用；精确测量作为后续优化）。
        {
            let vp = window.viewport_size();
            let cell_w = FONT_PX * CELL_W_RATIO;
            let vw = f32::from(vp.width);
            let vh = f32::from(vp.height);
            let cols = (((vw - PAD_X) / cell_w).floor() as usize).max(20);
            let grid_rows = (((vh - TITLEBAR_H - PAD_Y) / LINE_PX).floor() as usize).max(5);
            self.terminal.resize(grid_rows, cols);
        }

        let frame = self.terminal.snapshot();
        let cursor = frame.cursor;
        // canvas 在 paint 阶段注册输入处理器（IME）所需的句柄。
        let fh = self.focus_handle.clone();
        let entity = cx.entity();

        div()
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                if let Some(bytes) = keystroke_to_bytes(&ev.keystroke) {
                    this.terminal.send_input(&bytes);
                    cx.notify();
                }
            }))
            // 鼠标滚轮翻看历史缓冲
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let lines = match ev.delta {
                    ScrollDelta::Lines(p) => p.y as i32,
                    ScrollDelta::Pixels(p) => (f32::from(p.y) / LINE_PX) as i32,
                };
                if lines != 0 {
                    this.terminal.scroll(lines);
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1a1b26))
            .text_color(rgb(0xc0caf5))
            .font_family("monospace")
            .text_sm()
            // 顶部标题栏：mac 三色圆点 + 标题
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .bg(rgb(0x16161e))
                    .child(dot(0xff5f56))
                    .child(dot(0xffbd2e))
                    .child(dot(0x27c93f))
                    .child(
                        div()
                            .ml_2()
                            .text_color(rgb(0x7aa2f7))
                            .child("smelt workspace · terminal"),
                    ),
            )
            // 终端主体：逐行渲染 alacritty 网格快照（带颜色）
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .p_2()
                    .text_size(px(FONT_PX))
                    .line_height(px(LINE_PX))
                    .children(frame.rows.into_iter().enumerate().map(move |(r, row)| {
                        let cc = match cursor {
                            Some((cr, cc)) if cr == r => Some(cc),
                            _ => None,
                        };
                        render_row(row, cc)
                    })),
            )
            // 透明覆盖层：在 paint 阶段注册 IME 输入处理器（不影响布局/绘制）。
            .child(
                canvas(
                    move |_bounds, _window, _cx| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &fh,
                            ElementInputHandler::new(bounds, entity),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

/// 渲染一行：把同属性的连续单元合并成一个 span；光标单元反色单独渲染。
fn render_row(row: Vec<terminal::Cell>, cursor_col: Option<usize>) -> Div {
    let mut spans: Vec<Div> = Vec::new();
    let mut i = 0;
    while i < row.len() {
        // 光标所在单元：前景/背景互换，形成反色块
        if Some(i) == cursor_col {
            let c = &row[i];
            spans.push(cell_span(&c.ch.to_string(), c.bg, c.fg, c.bold, c.underline));
            i += 1;
            continue;
        }
        let c = &row[i];
        let (fg, bg, bold, underline) = (c.fg, c.bg, c.bold, c.underline);
        let mut text = String::new();
        while i < row.len()
            && Some(i) != cursor_col
            && row[i].fg == fg
            && row[i].bg == bg
            && row[i].bold == bold
            && row[i].underline == underline
        {
            text.push(row[i].ch);
            i += 1;
        }
        spans.push(cell_span(&text, fg, bg, bold, underline));
    }
    // 固定行高，与行列计算的 LINE_PX 一致，保证行数与网格对齐
    div().flex().h(px(LINE_PX)).children(spans)
}

/// 一个文本 span：前景/背景色 + 可选粗体/下划线。
fn cell_span(text: &str, fg: u32, bg: u32, bold: bool, underline: bool) -> Div {
    let mut d = div().child(text.to_string()).text_color(rgb(fg)).bg(rgb(bg));
    if bold {
        d = d.font_weight(FontWeight::BOLD);
    }
    if underline {
        d = d.underline();
    }
    d
}

/// 把一次「非文本按键」转成写给 PTY 的字节：特殊键（回车/退格/方向键等）和
/// Ctrl 组合。可打印字符与空格走 IME 的 replace_text_in_range，不在这里处理。
/// 返回 None 表示不由这里发送。
fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;

    // Cmd（platform）组合留给应用级快捷键，不进终端。
    if m.platform {
        return None;
    }

    // 特殊键 → 终端控制序列
    let named: Option<&[u8]> = match ks.key.as_str() {
        "enter" => Some(b"\r"),
        "backspace" => Some(b"\x7f"),
        "tab" => Some(b"\t"),
        "escape" => Some(b"\x1b"),
        "left" => Some(b"\x1b[D"),
        "right" => Some(b"\x1b[C"),
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "delete" => Some(b"\x1b[3~"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(bytes.to_vec());
    }

    // Ctrl + 字母 → 控制字节（ctrl-c=0x03, ctrl-d=0x04 ...）
    if m.control && ks.key.len() == 1 {
        let c = ks.key.as_bytes()[0];
        if c.is_ascii_alphabetic() {
            return Some(vec![c.to_ascii_lowercase() - b'a' + 1]);
        }
    }

    None
}

/// 标题栏上的一个 mac 风格圆点。
fn dot(color: u32) -> impl IntoElement {
    div().w(px(12.)).h(px(12.)).rounded_full().bg(rgb(color))
}

fn main() {
    gpui_platform::application().run(move |cx| {
        // 用任何 gpui-component 功能前必须先初始化。
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| Workspace::new(cx));
                // 顶层视图必须包一层 Root（组件库的主题/遮罩系统要求）。
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("打开窗口失败");
        })
        .detach();
    });
}
