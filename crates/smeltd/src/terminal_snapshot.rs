//! 网格 → ANSI 快照（主屏：history + 可视区 + 模式；备用屏：可视区 + 模式）。
//!
//! attach / watch / handoff 共用同一套模式感知 keyframe：主屏按 history + viewport
//! 序列化，备用屏只按 viewport 序列化；两者都用按行 CUP + 绝对 SGR，空 Term 解析后
//! 即当前画面。

use std::io::Write;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

/// attach/handoff 快照最多带上的历史行数（含可视区）；避免超大会话一次吐爆客户端。
pub(crate) const SNAPSHOT_MAX_LINES: usize = 10_000;

/// GUI 客户端 reattach 用：主屏带 scrollback history，备用屏只画 viewport。
///
/// `launch` 只用于会话元数据，不能作为终端模式的替代品：Codex 等程序可能在
/// 主屏上运行，而真正的 TUI 是否使用备用屏必须由 `TermMode::ALT_SCREEN` 决定。
pub(crate) fn snapshot_ansi<T: EventListener>(term: &Term<T>, _launch: Option<&str>) -> Vec<u8> {
    snapshot_for_terminal_mode(term, SNAPSHOT_MAX_LINES)
}

/// Read-only renderers and handoff use the same mode-aware keyframe as desktop attach.
///
/// `max_scrollback_lines` 是**消费端声明的预算**，不是服务端的猜测：手机走的是
/// 隧道，主屏 TUI（不切备用屏、完全靠终端滚动的那类）的 history 动辄几十万字节，
/// 一次全推过去就是「进页面先干等一会儿」。让客户端说明自己这次要多少行，
/// 需要更多历史时再提高预算重来，省下的是链路上的字节，不是可见内容。
///
/// 注意这里**不看是哪个 agent**：备用屏 TUI 的快照本来就只画可视区，预算对它无害；
/// 按程序名特判既会漏（同一个程序可能中途切屏），也违背快照分流只认
/// `TermMode::ALT_SCREEN` 这条既有约定。
pub(crate) fn snapshot_ansi_for_watch<T: EventListener>(
    term: &Term<T>,
    _launch: Option<&str>,
    max_scrollback_lines: usize,
) -> Vec<u8> {
    snapshot_for_terminal_mode(term, max_scrollback_lines)
}

/// 这个会话当前能提供的最大历史行数（含可视区）。客户端据此判断「还有没有更老的
/// 内容可以加载」——备用屏没有回滚，可用行数就是可视区。
pub(crate) fn available_snapshot_lines<T: EventListener>(term: &Term<T>) -> usize {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return term.screen_lines().max(1);
    }
    history_span(term)
}

fn history_span<T: EventListener>(term: &Term<T>) -> usize {
    let top = term.topmost_line();
    let bottom = term.bottommost_line();
    (bottom.0 - top.0 + 1).max(0) as usize
}

/// 写入 handoff.json 的 grid：主屏包含 history + viewport，备用屏只包含 viewport。
pub(crate) fn snapshot_ansi_for_handoff<T: EventListener>(
    term: &Term<T>,
    _launch: Option<&str>,
) -> Vec<u8> {
    snapshot_for_terminal_mode(term, SNAPSHOT_MAX_LINES)
}

fn snapshot_for_terminal_mode<T: EventListener>(term: &Term<T>, max_lines: usize) -> Vec<u8> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        snapshot_viewport(term)
    } else {
        snapshot_with_history(term, max_lines)
    }
}

fn snapshot_viewport<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let mut out = snapshot_mode_prefix(term, /*clear_scrollback=*/ false);
    paint_viewport_keyframe(&mut out, term);
    snapshot_cursor_suffix(term, &mut out);
    out
}

fn snapshot_with_history<T: EventListener>(term: &Term<T>, max_lines: usize) -> Vec<u8> {
    let mut out = snapshot_mode_prefix(term, /*clear_scrollback=*/ true);
    // Disable autowrap while serializing full-width rows. Explicit CRLFs then
    // build real scrollback instead of CUP row numbers clamping to the screen.
    out.extend_from_slice(b"\x1b[?7l");
    paint_history_keyframe(&mut out, term, max_lines);
    if term.mode().contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7h");
    }
    snapshot_cursor_suffix(term, &mut out);
    out
}

fn snapshot_mode_prefix<T: EventListener>(term: &Term<T>, clear_scrollback: bool) -> Vec<u8> {
    let mode = *term.mode();
    let cols = term.columns().max(1);
    let screen_lines = term.screen_lines().max(1);
    let mut out = Vec::with_capacity(cols.saturating_mul(screen_lines).saturating_mul(8));
    out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
    if mode.contains(TermMode::ALT_SCREEN) {
        out.extend_from_slice(b"\x1b[?1049h");
    } else {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    if mode.contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7h");
    } else {
        out.extend_from_slice(b"\x1b[?7l");
    }
    append_mode_restores(&mut out, mode);
    out.extend_from_slice(b"\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    if clear_scrollback {
        out.extend_from_slice(b"\x1b[3J");
    }
    out
}

fn snapshot_cursor_suffix<T: EventListener>(term: &Term<T>, out: &mut Vec<u8>) {
    let cols = term.columns().max(1);
    let screen_lines = term.screen_lines().max(1);
    let content = term.renderable_content();
    let cursor = content.cursor.point;
    let display_offset = term.grid().display_offset();
    let cursor_row = cursor.line.0 + display_offset as i32;
    if cursor_row >= 0 && (cursor_row as usize) < screen_lines {
        let col = cursor.column.0.min(cols.saturating_sub(1));
        let _ = write!(out, "\x1b[{};{}H", cursor_row as usize + 1, col + 1);
        match content.cursor.shape {
            CursorShape::Hidden => out.extend_from_slice(b"\x1b[?25l"),
            CursorShape::Underline => out.extend_from_slice(b"\x1b[4 q\x1b[?25h"),
            CursorShape::Beam => out.extend_from_slice(b"\x1b[6 q\x1b[?25h"),
            CursorShape::HollowBlock => out.extend_from_slice(b"\x1b[0 q\x1b[?25h"),
            CursorShape::Block => out.extend_from_slice(b"\x1b[2 q\x1b[?25h"),
        }
    }

    // The next PTY bytes are a diff against the terminal's current rendition.
    // Restore it after painting the keyframe so live output starts from the same state.
    let style = CellStyle::from_cell(&term.grid().cursor.template);
    if style.link.is_some() {
        emit_link_osc(out, style.link.as_deref());
    }
    emit_absolute_sgr(out, &style);
}

/// TUI 可视区 keyframe：按行 CUP + 绝对 SGR（Codux `terminal_snapshot_data` 同构）。
fn paint_viewport_keyframe<T: EventListener>(out: &mut Vec<u8>, term: &Term<T>) {
    let cols = term.columns().max(1);
    let rows = term.screen_lines().max(1);
    let display_offset = term.grid().display_offset();

    // row → (col → cell 引用通过复制字符+样式)
    let mut grid: Vec<Vec<Option<KeyframeCell>>> = vec![vec![None; cols]; rows];
    for indexed in term.renderable_content().display_iter {
        let row = indexed.point.line.0 + display_offset as i32;
        if row < 0 || row as usize >= rows {
            continue;
        }
        let col = indexed.point.column.0;
        if col >= cols {
            continue;
        }
        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let mut text = String::new();
        if cell.c != '\0' && !cell.c.is_control() {
            text.push(cell.c);
        }
        if let Some(zw) = cell.zerowidth() {
            for &ch in zw {
                if !ch.is_control() {
                    text.push(ch);
                }
            }
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        // 空白且默认样式：跳过，让主题底透出（Codux 同策略）
        if text.trim().is_empty()
            && is_default_fg(cell.fg)
            && is_default_bg(cell.bg)
            && !cell_has_visuals(cell)
        {
            continue;
        }
        grid[row as usize][col] = Some(KeyframeCell {
            text,
            width,
            style: CellStyle::from_cell(cell),
        });
    }

    emit_keyframe_rows(out, &grid);
}

/// Shell：history + 可视区，按缓冲行顺序硬换行推进（绝对 SGR）。
fn paint_history_keyframe<T: EventListener>(out: &mut Vec<u8>, term: &Term<T>, max_lines: usize) {
    let cols = term.columns().max(1);
    let top = term.topmost_line();
    let bottom = term.bottommost_line();
    // 预算至少要够铺满可视区，否则光标定位会落在快照之外。
    let max_lines = max_lines.clamp(term.screen_lines().max(1), SNAPSHOT_MAX_LINES);
    let span = (bottom.0 - top.0 + 1).max(0) as usize;
    let start = if span > max_lines {
        Line(bottom.0 - max_lines as i32 + 1)
    } else {
        top
    };

    let mut rows: Vec<Vec<Option<KeyframeCell>>> = Vec::new();
    let mut line = start;
    while line <= bottom {
        let row = &term.grid()[line];
        let mut cells = vec![None; cols];
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut text = String::new();
            if cell.c != '\0' && !cell.c.is_control() {
                text.push(cell.c);
            }
            if let Some(zw) = cell.zerowidth() {
                for &ch in zw {
                    if !ch.is_control() {
                        text.push(ch);
                    }
                }
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            if text.trim().is_empty()
                && is_default_fg(cell.fg)
                && is_default_bg(cell.bg)
                && !cell_has_visuals(cell)
            {
                continue;
            }
            cells[col] = Some(KeyframeCell {
                text,
                width,
                style: CellStyle::from_cell(cell),
            });
        }
        rows.push(cells);
        line += 1;
    }
    emit_history_rows(out, &rows);
}

fn cell_has_visuals(cell: &Cell) -> bool {
    let f = cell.flags;
    f.intersects(
        Flags::BOLD
            | Flags::DIM
            | Flags::ITALIC
            | Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE
            | Flags::INVERSE
            | Flags::HIDDEN
            | Flags::STRIKEOUT
            | Flags::BOLD_ITALIC
            | Flags::DIM_BOLD,
    ) || cell.hyperlink().is_some()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CellStyle {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: u8,
    inverse: bool,
    hidden: bool,
    strike: bool,
    link: Option<String>,
}

impl CellStyle {
    fn default_style() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            bold: false,
            dim: false,
            italic: false,
            underline: 0,
            inverse: false,
            hidden: false,
            strike: false,
            link: None,
        }
    }

    pub(crate) fn from_cell(cell: &Cell) -> Self {
        let f = cell.flags;
        Self {
            fg: cell.fg,
            bg: cell.bg,
            bold: f.contains(Flags::BOLD) || f.contains(Flags::BOLD_ITALIC),
            dim: f.contains(Flags::DIM) || f.contains(Flags::DIM_BOLD),
            italic: f.contains(Flags::ITALIC) || f.contains(Flags::BOLD_ITALIC),
            underline: underline_kind(f),
            inverse: f.contains(Flags::INVERSE),
            hidden: f.contains(Flags::HIDDEN),
            strike: f.contains(Flags::STRIKEOUT),
            link: cell.hyperlink().map(|h| h.uri().to_string()),
        }
    }
}

#[derive(Clone)]
struct KeyframeCell {
    text: String,
    width: usize,
    style: CellStyle,
}

/// 按行 `\x1b[row;1H` + 绝对 SGR 吐出（Codux `terminal_snapshot_data`）。
/// 每行画完后 `\x1b[K`（EL）清掉行尾残留，避免长输出软换行后 prompt 盖不干净。
fn emit_keyframe_rows(out: &mut Vec<u8>, rows: &[Vec<Option<KeyframeCell>>]) {
    let mut current = CellStyle::default_style();
    for (row_index, row_cells) in rows.iter().enumerate() {
        let Some(last_col) = row_cells.iter().rposition(|c| {
            c.as_ref().is_some_and(|cell| {
                !cell.text.trim().is_empty() || cell.style != CellStyle::default_style()
            })
        }) else {
            // 空行也 CUP + EL，清掉可能残留的旧内容
            let _ = write!(out, "\x1b[{};1H\x1b[K", row_index + 1);
            continue;
        };
        let _ = write!(out, "\x1b[{};1H", row_index + 1);
        let mut col = 0;
        while col <= last_col {
            match &row_cells[col] {
                Some(cell) => {
                    if cell.style != current {
                        if cell.style.link != current.link {
                            emit_link_osc(out, cell.style.link.as_deref());
                        }
                        emit_absolute_sgr(out, &cell.style);
                        current = cell.style.clone();
                    }
                    if cell.text.is_empty() {
                        for _ in 0..cell.width.max(1) {
                            out.push(b' ');
                        }
                    } else {
                        for ch in cell.text.chars() {
                            push_char(out, ch);
                        }
                    }
                    col += cell.width.max(1);
                }
                None => {
                    if current != CellStyle::default_style() {
                        if current.link.is_some() {
                            emit_link_osc(out, None);
                        }
                        out.extend_from_slice(b"\x1b[0m");
                        current = CellStyle::default_style();
                    }
                    out.push(b' ');
                    col += 1;
                }
            }
        }
        // 行尾 EL：抹掉该行 last_col 之后的旧字符（长 cargo 行糊进 prompt 的主因）
        if current != CellStyle::default_style() {
            if current.link.is_some() {
                emit_link_osc(out, None);
            }
            out.extend_from_slice(b"\x1b[0m");
            current = CellStyle::default_style();
        }
        out.extend_from_slice(b"\x1b[K");
    }
    if current != CellStyle::default_style() {
        if current.link.is_some() {
            emit_link_osc(out, None);
        }
        out.extend_from_slice(b"\x1b[0m");
    }
}

/// Emit buffered lines sequentially so lines above the viewport become real
/// terminal history. CUP cannot address rows outside the visible screen.
fn emit_history_rows(out: &mut Vec<u8>, rows: &[Vec<Option<KeyframeCell>>]) {
    let mut current = CellStyle::default_style();
    for (row_index, row_cells) in rows.iter().enumerate() {
        out.push(b'\r');
        let last_col = row_cells.iter().rposition(|c| {
            c.as_ref().is_some_and(|cell| {
                !cell.text.trim().is_empty() || cell.style != CellStyle::default_style()
            })
        });
        if let Some(last_col) = last_col {
            let mut col = 0;
            while col <= last_col {
                match &row_cells[col] {
                    Some(cell) => {
                        if cell.style != current {
                            if cell.style.link != current.link {
                                emit_link_osc(out, cell.style.link.as_deref());
                            }
                            emit_absolute_sgr(out, &cell.style);
                            current = cell.style.clone();
                        }
                        if cell.text.is_empty() {
                            for _ in 0..cell.width.max(1) {
                                out.push(b' ');
                            }
                        } else {
                            for ch in cell.text.chars() {
                                push_char(out, ch);
                            }
                        }
                        col += cell.width.max(1);
                    }
                    None => {
                        if current != CellStyle::default_style() {
                            if current.link.is_some() {
                                emit_link_osc(out, None);
                            }
                            out.extend_from_slice(b"\x1b[0m");
                            current = CellStyle::default_style();
                        }
                        out.push(b' ');
                        col += 1;
                    }
                }
            }
        }
        if current != CellStyle::default_style() {
            if current.link.is_some() {
                emit_link_osc(out, None);
            }
            out.extend_from_slice(b"\x1b[0m");
            current = CellStyle::default_style();
        }
        out.extend_from_slice(b"\x1b[K");
        if row_index + 1 < rows.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
}

fn emit_link_osc(out: &mut Vec<u8>, uri: Option<&str>) {
    out.extend_from_slice(b"\x1b]8;;");
    if let Some(u) = uri {
        out.extend_from_slice(u.as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

/// 绝对 SGR：始终以 `0` 开头（Codux `snapshot_style_sgr`），杜绝差分状态机半截泄漏。
fn emit_absolute_sgr(out: &mut Vec<u8>, style: &CellStyle) {
    let mut params = Vec::with_capacity(32);
    params.push(b'0');
    let push = |params: &mut Vec<u8>, code: u8| {
        params.push(b';');
        push_u8(params, code);
    };
    if style.bold {
        push(&mut params, 1);
    }
    if style.dim {
        push(&mut params, 2);
    }
    if style.italic {
        push(&mut params, 3);
    }
    if style.underline != 0 {
        params.push(b';');
        match style.underline {
            1 => params.extend_from_slice(b"4"),
            2 => params.extend_from_slice(b"4:2"),
            3 => params.extend_from_slice(b"4:3"),
            4 => params.extend_from_slice(b"4:4"),
            5 => params.extend_from_slice(b"4:5"),
            _ => params.extend_from_slice(b"4"),
        }
    }
    if style.inverse {
        push(&mut params, 7);
    }
    if style.hidden {
        push(&mut params, 8);
    }
    if style.strike {
        push(&mut params, 9);
    }
    // 颜色：绝对模式下总是写上（含默认 39/49），与 Codux 一致
    append_color_params_abs(&mut params, true, style.fg);
    append_color_params_abs(&mut params, false, style.bg);

    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(&params);
    out.push(b'm');
}

fn append_color_params_abs(params: &mut Vec<u8>, is_fg: bool, color: Color) {
    params.push(b';');
    match color {
        Color::Named(n) => {
            push_u8(params, named_sgr_code(n, is_fg));
        }
        Color::Indexed(i) => {
            push_u8(params, if is_fg { 38 } else { 48 });
            params.extend_from_slice(b";5;");
            push_u8(params, i);
        }
        Color::Spec(rgb) => {
            push_u8(params, if is_fg { 38 } else { 48 });
            params.extend_from_slice(b";2;");
            push_u8(params, rgb.r);
            params.push(b';');
            push_u8(params, rgb.g);
            params.push(b';');
            push_u8(params, rgb.b);
        }
    }
}

fn push_char(out: &mut Vec<u8>, ch: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
}

fn append_mode_restores(out: &mut Vec<u8>, mode: TermMode) {
    if mode.contains(TermMode::APP_CURSOR) {
        out.extend_from_slice(b"\x1b[?1h");
    }
    if mode.contains(TermMode::BRACKETED_PASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    // 鼠标：按实际打开的子模式恢复（SGR 优先）
    if mode.intersects(TermMode::MOUSE_MODE) {
        if mode.contains(TermMode::SGR_MOUSE) {
            out.extend_from_slice(b"\x1b[?1006h");
        }
        if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            out.extend_from_slice(b"\x1b[?1000h");
        }
        if mode.contains(TermMode::MOUSE_DRAG) {
            out.extend_from_slice(b"\x1b[?1002h");
        }
        if mode.contains(TermMode::MOUSE_MOTION) {
            out.extend_from_slice(b"\x1b[?1003h");
        }
    }
    if mode.contains(TermMode::FOCUS_IN_OUT) {
        out.extend_from_slice(b"\x1b[?1004h");
    }
}

fn underline_kind(flags: Flags) -> u8 {
    if flags.contains(Flags::UNDERCURL) {
        3
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        2
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        4
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        5
    } else if flags.contains(Flags::UNDERLINE) {
        1
    } else {
        0
    }
}

fn push_u8(params: &mut Vec<u8>, n: u8) {
    if n >= 100 {
        params.push(b'0' + n / 100);
        params.push(b'0' + (n / 10) % 10);
        params.push(b'0' + n % 10);
    } else if n >= 10 {
        params.push(b'0' + n / 10);
        params.push(b'0' + n % 10);
    } else {
        params.push(b'0' + n);
    }
}

fn is_default_fg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Foreground))
}
fn is_default_bg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Background))
}

fn named_sgr_code(n: NamedColor, is_fg: bool) -> u8 {
    use NamedColor::*;
    match (n, is_fg) {
        (Black, true) => 30,
        (Red, true) => 31,
        (Green, true) => 32,
        (Yellow, true) => 33,
        (Blue, true) => 34,
        (Magenta, true) => 35,
        (Cyan, true) => 36,
        (White, true) => 37,
        (Foreground, true) => 39,
        (BrightBlack, true) => 90,
        (BrightRed, true) => 91,
        (BrightGreen, true) => 92,
        (BrightYellow, true) => 93,
        (BrightBlue, true) => 94,
        (BrightMagenta, true) => 95,
        (BrightCyan, true) => 96,
        (BrightWhite, true) => 97,
        (Black, false) => 40,
        (Red, false) => 41,
        (Green, false) => 42,
        (Yellow, false) => 43,
        (Blue, false) => 44,
        (Magenta, false) => 45,
        (Cyan, false) => 46,
        (White, false) => 47,
        (Background, false) => 49,
        (BrightBlack, false) => 100,
        (BrightRed, false) => 101,
        (BrightGreen, false) => 102,
        (BrightYellow, false) => 103,
        (BrightBlue, false) => 104,
        (BrightMagenta, false) => 105,
        (BrightCyan, false) => 106,
        (BrightWhite, false) => 107,
        (_, true) => 39,
        (_, false) => 49,
    }
}
