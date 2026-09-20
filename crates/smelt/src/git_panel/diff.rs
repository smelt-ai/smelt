//! Git diff 的行模型渲染、聚合视图和评论输入区。
//!
//! Worktree/branch 操作与 `Workspace` 状态管理留在父模块；这里集中处理 diff 展示，
//! 让大段虚拟列表和 sticky header 逻辑不再挤在同一个文件。

use super::*;

/// diff 行类型 → (前景, 整行背景, 左色条, 行内变化片段深底)。
/// 色值走 ui_theme 的 diff_* 语义位，深浅两套主题各一份（浅色下「深底」其实是浅底，
/// 名字保留 hl 语义：比整行底更浓，用来点出行内真正改动的片段）。
pub(crate) fn diff_colors(kind: DiffKind) -> (Rgba, Option<Rgba>, Option<Rgba>, Rgba) {
    use crate::ui_theme as t;
    match kind {
        DiffKind::Add => (
            rgb(t::diff_add_fg()),
            Some(rgb(t::diff_add_bg())),
            Some(rgb(t::diff_add_bar())),
            rgb(t::diff_add_hl()),
        ),
        DiffKind::Del => (
            rgb(t::diff_del_fg()),
            Some(rgb(t::diff_del_bg())),
            Some(rgb(t::diff_del_bar())),
            rgb(t::diff_del_hl()),
        ),
        DiffKind::Context => (rgb(t::diff_ctx_fg()), None, None, rgb(0)),
        DiffKind::Meta => (rgb(t::diff_meta_fg()), None, None, rgb(0)),
    }
}

/// 审查评论可以覆盖增删行和周边上下文代码；hunk/文件元信息没有稳定代码语义，
/// 不纳入拖选范围。
pub(crate) fn is_commentable_diff_line(line: &DiffLine) -> bool {
    line.kind != DiffKind::Meta
}

/// 文本区（flex_1）：有 segments 就拆成多段（变化段上深底），否则整行一段。
pub(crate) fn diff_text_area(l: &DiffLine, fg: Rgba, hl: Rgba) -> Div {
    match &l.segments {
        Some(segs) => div()
            .flex_1()
            .px_2()
            .text_color(fg)
            .flex()
            .children(segs.iter().map(|(s, changed)| {
                let span = div().child(s.clone());
                if *changed {
                    span.bg(hl).rounded_sm()
                } else {
                    span
                }
            })),
        None => div()
            .flex_1()
            .px_2()
            .text_color(fg)
            .child(if l.text.is_empty() {
                "\u{00a0}".to_string()
            } else {
                l.text.clone()
            }),
    }
}

/// diff 行号栏的共用基础样式：固定宽度、不伸缩、右对齐、次级文字色。
pub(crate) fn diff_gutter_base(gw: f32) -> Div {
    div()
        .w(px(gw))
        .flex_none()
        .px_1()
        .flex()
        .justify_end()
        .text_color(rgb(crate::ui_theme::text_faint()))
}

/// 统一 diff 不换行，内容区必须比最长可见行宽，外层横向滚动条才有可滚动的画布。
/// 非 ASCII 按两个等宽字符估算，足以覆盖中英文代码和注释；避免为偶发超长单行
/// 分配无上限的布局宽度。
pub(crate) fn diff_content_width(lines: &[DiffLine], gutter_w: f32) -> f32 {
    const MONO_CHAR_WIDTH: f32 = 8.4;
    const MAX_CANVAS_WIDTH: f32 = 12_000.0;
    let widest = lines
        .iter()
        .map(|line| {
            line.text
                .chars()
                .map(|ch| if ch.is_ascii() { 1usize } else { 2usize })
                .sum::<usize>()
        })
        .max()
        .unwrap_or_default() as f32;
    // 左色条、行号栏（按并排双轨留够余量，统一视图会使画布稍宽）、
    // 文本内边距，以及 hunk 操作区预留。
    (2.0 + gutter_w * 2.0 + 92.0 + widest * MONO_CHAR_WIDTH).clamp(640.0, MAX_CANVAS_WIDTH)
}

/// 按块操作按钮渲染要的上下文：仓库根 + 该 diff 能不能拼出合法 patch。
/// 打包传递，省得每个渲染函数都多挂两个参数。
#[derive(Clone)]
pub(crate) struct HunkCtx {
    root: String,
    /// 当前视图下该给哪些按钮。
    ops: HunkOps,
    /// 行下标 → 该行是第几个 hunk 的头。只有 hunk 头那行才渲染按钮。
    starts: Rc<std::collections::HashMap<usize, usize>>,
    /// F7 当前停在第几块，给它的头行描边，不然跳完不知道落在哪。
    active: Option<usize>,
}

impl HunkCtx {
    /// 这行是不是某个 hunk 的头；是就返回块序号。不可 patch 的 diff 不给按钮，
    /// 但仍要返回序号——F7 导航和高亮对子模块/未跟踪文件一样有用。
    fn idx_at(&self, line: usize) -> Option<usize> {
        self.starts.get(&line).copied()
    }
}

/// hunk 头那行右侧的按钮组：暂存本块 / 丢弃本块。
///
/// 用 div 而不是 Button 组件：这些行住在 uniform_list 里，只有可见区间会被构造，
/// 行内嵌带状态的组件容易和虚拟滚动的复用打架，"查看完整文件 ↗" 也是同样的写法。
pub(crate) fn hunk_buttons(idx: usize, ctx: &HunkCtx, ws: &Entity<Workspace>) -> Div {
    let btn = |label: &'static str, id: &'static str, color: u32, hover: u32| {
        div()
            .id((id, idx))
            .px_2()
            .text_xs()
            .cursor_pointer()
            .text_color(rgb(color))
            .hover(|s| s.text_color(rgb(hover)))
            .child(label)
    };
    // 平时透明、鼠标移到这一行才显形（group 名见 render_diff_line）。IDEA 的按块
    // 操作也是藏在 gutter 里、hover 才明显——常驻的文字按钮太吵，每个 hunk 头顶
    // 着两颗按钮，一屏下来全是它们。
    let bar = div()
        .flex()
        .items_center()
        .gap_1()
        .opacity(0.0)
        .group_hover(HUNK_ROW_GROUP, |s| s.opacity(1.0));
    match ctx.ops {
        HunkOps::None => bar,
        HunkOps::StageDiscard => {
            let (ws_stage, root_stage) = (ws.clone(), ctx.root.clone());
            let (ws_discard, root_discard) = (ws.clone(), ctx.root.clone());
            bar.child(btn("暂存块", "hunk-stage", 0x7dcfff, 0xa9dcff).on_click(
                move |_ev, _w, cx| {
                    let root = root_stage.clone();
                    ws_stage.update(cx, |this, cx| this.stage_hunk(root, idx, cx));
                },
            ))
            .child(btn("丢弃块", "hunk-discard", 0x8b6b7a, 0xff7a93).on_click(
                move |_ev, _w, cx| {
                    let root = root_discard.clone();
                    ws_discard.update(cx, |this, cx| this.start_discard_hunk(root, idx, cx));
                },
            ))
        }
        HunkOps::Unstage => {
            let (ws_un, root_un) = (ws.clone(), ctx.root.clone());
            bar.child(
                btn("取消暂存块", "hunk-unstage", 0x7dcfff, 0xa9dcff).on_click(
                    move |_ev, _w, cx| {
                        let root = root_un.clone();
                        ws_un.update(cx, |this, cx| this.unstage_hunk(root, idx, cx));
                    },
                ),
            )
        }
    }
}

/// 渲染一行 diff：左侧色条 + 单一审查行号栏 + 文本；整行按类型上淡背景。
/// 若有 segments（行内 diff 结果），变化片段再叠一层更深的底色。
/// 只有行号栏支持鼠标拖选连续范围，避免浏览或复制代码时误触发评论。
struct DiffLineParams<'a> {
    selected: bool,
    show_comment_anchor: bool,
    workspace: &'a Entity<Workspace>,
    hunks: &'a HunkCtx,
    gutter_width: f32,
    code_scroll: &'a ScrollHandle,
    code_width: f32,
}

fn render_diff_line(i: usize, l: &DiffLine, params: DiffLineParams<'_>) -> Stateful<Div> {
    let DiffLineParams {
        selected,
        show_comment_anchor,
        workspace: ws,
        hunks,
        gutter_width,
        code_scroll,
        code_width,
    } = params;
    let (fg, bg, bar, hl) = diff_colors(l.kind);
    // 行号栏是整份 diff 共用的固定轨道。不给 flex shrink 的机会，否则
    // 窄面板时每行会按自身文本宽度压缩，看上去行号像是错位的。
    let gutter = || diff_gutter_base(gutter_width);

    let extend_ws = ws.clone();
    let finish_ws = ws.clone();
    let mut row = div()
        .id(("diff-line", i))
        .flex()
        .items_center()
        .h(px(FILE_LINE_H))
        .whitespace_nowrap()
        // 只能在行号栏开始；开始后整行负责续选和收尾，这样指针移到正文或
        // 在该行任意位置松开都不会丢失事件。
        .on_mouse_move(move |_, _window, cx| {
            extend_ws.update(cx, |this, cx| this.extend_diff_selection(i, cx));
        })
        .on_mouse_up(MouseButton::Left, move |_, window, cx| {
            finish_ws.update(cx, |this, cx| this.finish_diff_selection(window, cx));
        });
    if let Some(b) = bg {
        row = row.bg(b);
    }
    // 交互只在行号栏触发，但范围本身要铺满整行，才能像代码审查那样一眼识别
    // “这段代码正在被评论”。选区色覆盖 diff 的红绿底，语义仍由左侧色条保留。
    if selected {
        row = row.bg(rgb(crate::ui_theme::bg_hover()));
    }
    let review_gutter: AnyElement = if is_commentable_diff_line(l) {
        let begin_ws = ws.clone();
        gutter()
            .id(("diff-review-gutter", i))
            .cursor_pointer()
            // 选区视觉只落在行号栏：连续范围是一条清楚的审查轨道，不污染代码底色。
            .when(selected, |g| {
                g.bg(rgb(crate::ui_theme::bg_hover()))
                    .border_l_2()
                    .border_color(rgb(crate::ui_theme::blue()))
            })
            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                begin_ws.update(cx, |this, cx| this.begin_diff_selection(i, cx));
            })
            .child(if show_comment_anchor {
                // 和 Codex 一样，`+` 是选区锚点的可见反馈，不是第二套点击行为。
                // 点击/拖拽仍由整个行号栏接收，因此鼠标可以从这里继续扩选。
                let open_ws = ws.clone();
                div()
                    .id(("diff-comment-anchor", i))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(rgb(crate::ui_theme::bg_card()))
                    .border_1()
                    .border_color(rgb(crate::ui_theme::border()))
                    .hover(|d| {
                        d.bg(rgb(crate::ui_theme::bg_hover()))
                            .border_color(rgb(crate::ui_theme::border_loud()))
                    })
                    .text_lg()
                    .text_color(rgb(crate::ui_theme::text()))
                    .child("+")
                    // `+` 只打开评论器；阻止事件冒泡，避免再次触发行号栏的
                    // mousedown/mouseup 而重置或提前完成拖选。
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_event, window, cx| {
                        open_ws.update(cx, |this, cx| this.open_diff_comment(window, cx));
                    })
                    .into_any_element()
            } else {
                div()
                    .child(
                        l.new_ln
                            .or(l.old_ln)
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    )
                    .into_any_element()
            })
            .into_any_element()
    } else {
        gutter()
            .child(
                l.new_ln
                    .or(l.old_ln)
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            )
            .into_any_element()
    };
    let hunk_idx = hunks.idx_at(i);
    // F7 停在这块就描一道边，跳完才看得出落点。
    if hunk_idx.is_some() && hunk_idx == hunks.active {
        row = row
            .border_1()
            .border_color(rgb(crate::ui_theme::diff_hunk_fg()));
    }
    // hunk 头行才需要 hover 分组（按钮藏在里面）。
    if hunk_idx.is_some() {
        row = row.group(HUNK_ROW_GROUP);
    }
    let mut code_canvas = div()
        .min_w(px(code_width))
        .h_full()
        .flex()
        .child(diff_text_area(l, fg, hl));
    if let Some(idx) = hunk_idx.filter(|_| hunks.ops != HunkOps::None) {
        code_canvas = code_canvas.child(hunk_buttons(idx, hunks, ws));
    }
    let mut code_viewport = div()
        .id(("diff-code-scroll", i))
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_x_scroll()
        .track_scroll(code_scroll)
        .child(code_canvas);
    // GPUI 默认会把纯纵向滚轮映射到仅横滚的子容器，导致代码横滚和
    // 外层虚拟列表同时竞争手势。限制到 X 轴后，纵向仍交给列表，只有
    // 触控板横向手势或底部滚动条才会移动代码画布。
    code_viewport.style().restrict_scroll_to_axis = Some(true);

    row
        // 左侧色条：增/删才有，其它用等宽透明占位保持对齐。
        .child(match bar {
            Some(c) => div().w(px(2.)).flex_none().h_full().bg(c),
            None => div().w(px(2.)).flex_none().h_full(),
        })
        .child(review_gutter)
        // 横向滚动只包住代码画布。色条、行号与评论器属于外层审查轨道，不能
        // 随长行横移；所有代码行共用一个句柄以保持上下行对齐。
        .child(code_viewport)
}

/// 只读的 diff 行渲染，给「日志」页看某次提交的改动用。
///
/// 与 [`render_diff_line`] 的区别是不带选行/评论那套交互——历史提交是既成事实，
/// 没有「选中几行发给 agent 去改」的语义。共用同一套配色和行内高亮，两处观感一致。
pub(crate) fn render_readonly_diff_line(l: &DiffLine, gw: f32) -> Div {
    let (fg, bg, bar, hl) = diff_colors(l.kind);
    let gutter =
        |n: Option<u32>| diff_gutter_base(gw).child(n.map(|v| v.to_string()).unwrap_or_default());
    let mut row = div()
        .flex()
        .items_center()
        .h(px(FILE_LINE_H))
        .whitespace_nowrap();
    if let Some(b) = bg {
        row = row.bg(b);
    }
    row.child(match bar {
        Some(c) => div().w(px(2.)).flex_none().h_full().bg(c),
        None => div().w(px(2.)).flex_none().h_full(),
    })
    .child(gutter(l.old_ln))
    .child(gutter(l.new_ln))
    .child(diff_text_area(l, fg, hl))
}

/// 把线性的 diff 行重排成并排的行对：上下文左右对齐；一组删/增按顺序配对，
/// 数量不等时多出的一侧留空；纯新增（无对应删行）左侧空。
pub(crate) fn build_split_rows(lines: &[DiffLine]) -> Vec<SplitRow> {
    let n = lines.len();
    let mut rows = Vec::new();
    let mut i = 0;
    while i < n {
        match lines[i].kind {
            DiffKind::Meta => {
                rows.push(SplitRow::Full(i));
                i += 1;
            }
            DiffKind::Context => {
                rows.push(SplitRow::Both(Some(i), Some(i)));
                i += 1;
            }
            DiffKind::Del => {
                let ds = i;
                while i < n && lines[i].kind == DiffKind::Del {
                    i += 1;
                }
                let de = i;
                let as_ = i;
                while i < n && lines[i].kind == DiffKind::Add {
                    i += 1;
                }
                let ae = i;
                let (dn, an) = (de - ds, ae - as_);
                for k in 0..dn.max(an) {
                    let l = (k < dn).then_some(ds + k);
                    let r = (k < an).then_some(as_ + k);
                    rows.push(SplitRow::Both(l, r));
                }
            }
            DiffKind::Add => {
                // 纯新增块（前面没有删行）：左侧空、右侧逐行。
                while i < n && lines[i].kind == DiffKind::Add {
                    rows.push(SplitRow::Both(None, Some(i)));
                    i += 1;
                }
            }
        }
    }
    rows
}

/// 渲染并排的半行（左或右，flex_1）。idx 为 None 时是空侧占位（暗底）。
/// left=true 用旧行号，否则用新行号。ri 是并排行在 rows 里的下标，只用来拼 id
/// （idx 本身在 Both(None, Some(i)) 这类情况下左右可能撞号，ri+left 才唯一）。
/// 可评论代码行同 render_diff_line 一样仅在行号栏支持拖选连续范围。
pub(crate) fn render_half(
    ri: usize,
    idx: Option<usize>,
    left: bool,
    lines: &[DiffLine],
    selected: &HashSet<usize>,
    ws: &Entity<Workspace>,
    gw: f32,
) -> Stateful<Div> {
    // overflow_hidden：长行必须裁剪在本半区内，否则会溢出盖住另一半，并排就糊了。
    let base = div()
        .id(("diff-half", ri * 2 + left as usize))
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .h_full();
    let Some(i) = idx else {
        // 空侧：略暗的底表示「此侧无对应行」。
        return base.bg(rgb(crate::ui_theme::diff_empty_bg()));
    };
    let l = &lines[i];
    let (fg, bg, bar, hl) = diff_colors(l.kind);
    let ln = if left { l.old_ln } else { l.new_ln };
    let extend_ws = ws.clone();
    let finish_ws = ws.clone();
    let mut row = base
        .on_mouse_move(move |_, _window, cx| {
            extend_ws.update(cx, |this, cx| this.extend_diff_selection(i, cx));
        })
        .on_mouse_up(MouseButton::Left, move |_, window, cx| {
            finish_ws.update(cx, |this, cx| this.finish_diff_selection(window, cx));
        });
    if let Some(b) = bg {
        row = row.bg(b);
    }
    if selected.contains(&i) {
        row = row.bg(rgb(crate::ui_theme::bg_hover()));
    }
    let review_gutter: AnyElement = if is_commentable_diff_line(l) {
        let begin_ws = ws.clone();
        diff_gutter_base(gw)
            .id(("diff-review-gutter-split", ri * 2 + left as usize))
            .cursor_pointer()
            .when(selected.contains(&i), |g| {
                g.bg(rgb(crate::ui_theme::bg_hover()))
                    .border_l_2()
                    .border_color(rgb(crate::ui_theme::blue()))
            })
            .child(ln.map(|v| v.to_string()).unwrap_or_default())
            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                begin_ws.update(cx, |this, cx| this.begin_diff_selection(i, cx));
            })
            .into_any_element()
    } else {
        diff_gutter_base(gw)
            .child(ln.map(|v| v.to_string()).unwrap_or_default())
            .into_any_element()
    };
    row.child(match bar {
        Some(c) => div().w(px(2.)).flex_none().h_full().bg(c),
        None => div().w(px(2.)).flex_none().h_full(),
    })
    .child(review_gutter)
    .child(diff_text_area(l, fg, hl))
}

/// 渲染并排视图的一行。ri 是该行在 rows 里的下标，透传给 render_half 拼 id。
pub(crate) fn render_split_row(
    ri: usize,
    row: &SplitRow,
    lines: &[DiffLine],
    selected: &HashSet<usize>,
    ws: &Entity<Workspace>,
    hunks: &HunkCtx,
    gw: f32,
) -> Div {
    match row {
        SplitRow::Full(i) => {
            let l = &lines[*i];
            let (fg, bg, _, _) = diff_colors(l.kind);
            let mut d = div()
                .flex()
                .items_center()
                .h(px(FILE_LINE_H))
                .w_full()
                .overflow_hidden()
                .whitespace_nowrap();
            if let Some(b) = bg {
                d = d.bg(b);
            }
            // hunk 头在并排视图里也是整行，同样挂按钮；文本占满剩余宽度把按钮推到右边。
            let hunk_idx = hunks.idx_at(*i);
            if hunk_idx.is_some() && hunk_idx == hunks.active {
                d = d
                    .border_1()
                    .border_color(rgb(crate::ui_theme::diff_hunk_fg()));
            }
            if hunk_idx.is_some() {
                d = d.group(HUNK_ROW_GROUP);
            }
            let mut d = d.child(div().flex_1().px_2().text_color(fg).child(l.text.clone()));
            if let Some(idx) = hunk_idx
                && hunks.ops != HunkOps::None
            {
                d = d.child(hunk_buttons(idx, hunks, ws));
            }
            d
        }
        SplitRow::Both(l, r) => div()
            .flex()
            .items_center()
            .h(px(FILE_LINE_H))
            // w_full 关键：容器占满整宽，两个 flex_1 半区才会真正各占一半；
            // 否则容器 hug content，grow 失效，空侧塌成 0 宽、内容顶到最左。
            .w_full()
            .whitespace_nowrap()
            .child(render_half(ri, *l, true, lines, selected, ws, gw))
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .bg(rgb(crate::ui_theme::diff_gutter())),
            ) // 中缝分隔
            .child(render_half(ri, *r, false, lines, selected, ws, gw)),
    }
}

/// Git diff 查看面板：uniform_list 虚拟滚动。split 为 true 时并排（左旧右新），
/// 否则统一视图。顶部文件名右侧有「统一/并排」切换按钮。改动行（+/-）可点选，
/// 选中后配合底部评论框「发送到终端」，把反馈批量写进当前激活终端的 PTY。
pub(crate) fn aggregate_diff_rows(
    lines: &[DiffLine],
    collapsed: &HashSet<String>,
) -> Vec<AggregateDiffRow> {
    let mut rows = Vec::new();
    let mut hidden = false;
    for (ix, line) in lines.iter().enumerate() {
        if line.file_header {
            let mut adds = 0;
            let mut dels = 0;
            for next in &lines[ix + 1..] {
                if next.file_header {
                    break;
                }
                adds += usize::from(next.kind == DiffKind::Add);
                dels += usize::from(next.kind == DiffKind::Del);
            }
            hidden = collapsed.contains(&line.text);
            rows.push(AggregateDiffRow::Header {
                path: line.text.clone(),
                adds,
                dels,
            });
        } else if !hidden {
            rows.push(AggregateDiffRow::Line(ix));
        }
    }
    rows
}

pub(crate) fn aggregate_file_headers(lines: &[DiffLine]) -> Vec<AggregateFileHeader> {
    let mut headers = Vec::new();
    let mut adds = 0;
    let mut dels = 0;
    for line in lines.iter().rev() {
        if line.file_header {
            headers.push(AggregateFileHeader { adds, dels });
            adds = 0;
            dels = 0;
        } else {
            adds += usize::from(line.kind == DiffKind::Add);
            dels += usize::from(line.kind == DiffKind::Del);
        }
    }
    headers.reverse();
    headers
}

/// 计算每个文件在统一视图展开时实际占用的正文高度。
///
/// `rows` 会根据折叠状态隐藏正文行，不能拿它来推断展开高度；点击已折叠的
/// sticky 标题时，仍需要知道完整高度，才能把同一段代码留在视口原来的位置。
pub(crate) fn aggregate_file_body_heights(lines: &[DiffLine]) -> Vec<f32> {
    let mut heights = Vec::new();
    let mut body_lines = 0usize;
    for line in lines.iter().rev() {
        if line.file_header {
            heights.push(body_lines as f32 * FILE_LINE_H);
            body_lines = 0;
        } else {
            body_lines += 1;
        }
    }
    heights.reverse();
    heights
}

pub(crate) fn aggregate_sticky_headers(
    rows: &[DiffReviewRow],
    split_rows: &[SplitRow],
    lines: &[DiffLine],
    split: bool,
) -> Vec<AggregateStickyHeader> {
    let mut top = 0.0;
    let mut headers = Vec::new();
    let mut use_visible_body_fallback = false;
    if split {
        let file_headers = aggregate_file_headers(lines);
        let mut header_index = 0;
        for (row_index, row) in split_rows.iter().enumerate() {
            if let SplitRow::Full(line_index) = row
                && let Some(line) = lines.get(*line_index)
                && line.file_header
            {
                let (adds, dels) = file_headers
                    .get(header_index)
                    .map(|header| (header.adds, header.dels))
                    .unwrap_or_default();
                header_index += 1;
                headers.push(AggregateStickyHeader {
                    path: line.text.clone(),
                    adds,
                    dels,
                    top,
                    height: FILE_LINE_H,
                    body_height: 0.0,
                    offset_y: 0.0,
                    row_index,
                });
            }
            top += FILE_LINE_H;
        }
    } else {
        let body_heights = aggregate_file_body_heights(lines);
        use_visible_body_fallback = body_heights.is_empty();
        let mut header_index = 0;
        for (row_index, row) in rows.iter().enumerate() {
            if let DiffReviewRow::Header { path, adds, dels } = row {
                headers.push(AggregateStickyHeader {
                    path: path.clone(),
                    adds: *adds,
                    dels: *dels,
                    top,
                    height: FILE_LINE_H + 8.0,
                    body_height: body_heights.get(header_index).copied().unwrap_or(0.0),
                    offset_y: 0.0,
                    row_index,
                });
                header_index += 1;
            }
            top += match row {
                DiffReviewRow::Header { .. } => FILE_LINE_H + 8.0,
                DiffReviewRow::Line(_) => FILE_LINE_H,
                DiffReviewRow::CommentComposer => COMMENT_COMPOSER_H,
            };
        }
    }
    let content_height = top;
    for index in 0..headers.len() {
        let next_top = headers
            .get(index + 1)
            .map(|header| header.top)
            .unwrap_or(content_height);
        if use_visible_body_fallback && headers[index].body_height <= 0.0 {
            headers[index].body_height =
                (next_top - headers[index].top - headers[index].height).max(0.0);
        }
    }
    headers
}

pub(crate) fn aggregate_file_row_index(
    lines: &[DiffLine],
    path: &str,
    collapsed: &HashSet<String>,
) -> Option<usize> {
    aggregate_diff_rows(lines, collapsed).iter().position(
        |row| matches!(row, AggregateDiffRow::Header { path: header, .. } if header == path),
    )
}

pub(crate) fn diff_file_row_index(
    lines: &[DiffLine],
    path: &str,
    collapsed: &HashSet<String>,
    split: bool,
) -> Option<usize> {
    if split {
        let header = lines
            .iter()
            .position(|line| line.file_header && line.text == path)?;
        build_split_rows(lines)
            .iter()
            .position(|row| matches!(row, SplitRow::Full(index) if *index == header))
    } else {
        aggregate_file_row_index(lines, path, collapsed)
    }
}

/// 当前聚合 diff 是否已经具备文件树导航所需的数据。
///
/// 单独保留成纯函数，避免异步加载状态与“目标文件是否真的在快照里”混为一谈。
pub(crate) fn aggregate_diff_ready_for_file(
    diff: &GitDiff,
    root: &str,
    scope: DiffScope,
    path: &str,
) -> bool {
    diff.root == root
        && diff.aggregate
        && diff.scope == scope
        && diff
            .lines
            .iter()
            .any(|line| line.file_header && line.text == path)
}

/// 聚合 diff 中文件标题相对滚动内容顶部的位置。
pub(crate) fn aggregate_file_scroll_top(
    lines: &[DiffLine],
    path: &str,
    collapsed: &HashSet<String>,
    split: bool,
    comment_after_line: Option<usize>,
) -> Option<f32> {
    if split {
        return diff_file_row_index(lines, path, collapsed, true)
            .map(|row| row as f32 * FILE_LINE_H);
    }

    let rows = aggregate_diff_rows(lines, collapsed);
    let target = rows.iter().position(
        |row| matches!(row, AggregateDiffRow::Header { path: header, .. } if header == path),
    )?;
    let mut top = rows[..target]
        .iter()
        .map(|row| match row {
            AggregateDiffRow::Header { .. } => FILE_LINE_H + 8.0,
            AggregateDiffRow::Line(_) => FILE_LINE_H,
        })
        .sum::<f32>();
    let comment_insert_at = comment_after_line.and_then(|line_index| {
        rows.iter()
            .rposition(|row| matches!(row, AggregateDiffRow::Line(index) if *index == line_index))
    });
    if comment_insert_at.is_some_and(|insert_at| target > insert_at) {
        top += COMMENT_COMPOSER_H;
    }
    Some(top)
}

/// 聚合 diff 里当前滚动位置对应的文件标题。
///
/// 标题位置在派生缓存里一次性算好；每帧只按滚动偏移二分，避免大 diff 下重复
/// 扫描全部文件和代码行。
pub(crate) fn aggregate_sticky_header(
    headers: &[AggregateStickyHeader],
    scroll_offset_y: f32,
    comment_insert_at: Option<usize>,
) -> Option<AggregateStickyHeader> {
    let scroll_top = (-scroll_offset_y).max(0.0);
    let adjusted_top = |header: &AggregateStickyHeader| {
        header.top
            + if comment_insert_at.is_some_and(|at| header.row_index > at) {
                COMMENT_COMPOSER_H
            } else {
                0.0
            }
    };
    let index = headers.partition_point(|header| adjusted_top(header) <= scroll_top);
    let current_index = index.checked_sub(1)?;
    let mut header = headers.get(current_index)?.clone();
    // 标题还在自己的自然位置时，让列表里的真实行负责绘制它。只有离开视口后
    // 才创建覆盖层，避免同一标题在顶部被绘制两次、点击区域也发生重叠。
    if scroll_top <= adjusted_top(&header) {
        return None;
    }
    // 评论器属于选中行所在文件的正文流。它不在缓存里的基础行数组中，因而要
    // 临时计入当前文件的可折叠高度；否则点击 sticky 标题时视口会少保留 188px。
    let composer_in_body = comment_insert_at.is_some_and(|at| {
        at > header.row_index
            && headers
                .get(current_index + 1)
                .is_none_or(|next| at < next.row_index)
    });
    if composer_in_body {
        header.body_height += COMMENT_COMPOSER_H;
    }
    if let Some(next) = headers.get(current_index + 1) {
        // CSS sticky 会让当前标题被下一个标题逐步顶出，而不是在交界点瞬间换字。
        // 当前标题切换到下一个标题前，最多只移动自身高度，避免露出滚动区外。
        header.offset_y =
            (adjusted_top(next) - scroll_top - header.height).clamp(-header.height, 0.0);
    }
    Some(header)
}

/// 折叠/展开文件后调整虚拟列表滚动锚点，保持视口里的下一段代码位置不变。
/// `was_collapsed` 表示点击前的状态：展开会增加正文高度，折叠会移除正文高度。
pub(crate) fn sticky_toggle_scroll_top(
    scroll_top: f32,
    header_top: f32,
    header_height: f32,
    body_height: f32,
    was_collapsed: bool,
) -> f32 {
    let body_start = header_top + header_height;
    if was_collapsed {
        // 展开时只有正文已经在视口上方，才需要把滚动位置向下补回；标题附近
        // 的视口保持不动，让新正文自然从标题下方出现。
        if scroll_top >= body_start {
            scroll_top + body_height
        } else {
            scroll_top
        }
    } else {
        // 折叠时正文还没进入视口就不动；否则扣除被移除的高度，并且不越过
        // 当前文件标题，避免标题本身被滚出视口。
        if scroll_top >= body_start {
            (scroll_top - body_height).max(header_top)
        } else {
            scroll_top
        }
    }
}

pub(crate) fn aggregate_file_header_paths(lines: &[DiffLine]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.file_header)
        .map(|line| line.text.clone())
        .collect()
}

pub(crate) fn aggregate_all_files_collapsed(paths: &[String], collapsed: &HashSet<String>) -> bool {
    !paths.is_empty() && paths.iter().all(|path| collapsed.contains(path))
}

/// 全部收起 ↔ 全部展开。已经全部收起时展开；否则收起当前这份 diff 里的所有文件。
pub(crate) fn toggle_aggregate_collapse_all(paths: &[String], collapsed: &mut HashSet<String>) {
    if aggregate_all_files_collapsed(paths, collapsed) {
        for path in paths {
            collapsed.remove(path);
        }
    } else {
        for path in paths {
            collapsed.insert(path.clone());
        }
    }
}

/// 聚合 diff 的文件标题行。列表里的普通标题和顶部 sticky 标题共用这套结构，
/// 确保折叠箭头、路径以及增删计数不会在两个位置逐渐产生视觉差异。
pub(crate) fn render_aggregate_diff_header(
    id: impl Into<ElementId>,
    path: String,
    adds: usize,
    dels: usize,
    collapsed: Option<bool>,
    toggle: Option<Entity<Workspace>>,
    preserve_scroll: Option<(VirtualListScrollHandle, f32, f32)>,
) -> Stateful<Div> {
    let toggle_path = path.clone();
    let was_collapsed = collapsed.unwrap_or(false);
    let row_height = if toggle.is_some() {
        FILE_LINE_H + 8.
    } else {
        FILE_LINE_H
    };
    let mut header = div()
        .id(id)
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .h(px(row_height))
        .px_3()
        .bg(rgb(crate::ui_theme::bg_hover()))
        .child(
            div()
                .w(px(10.))
                .flex_none()
                .child(collapsed.map_or("", |collapsed| if collapsed { "▸" } else { "▾" })),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_semibold()
                .child(path),
        )
        .child(
            div()
                .text_color(rgb(crate::ui_theme::green()))
                .child(format!("+{adds}")),
        )
        .child(
            div()
                .text_color(rgb(crate::ui_theme::red()))
                .child(format!("-{dels}")),
        );
    if let Some(toggle) = toggle {
        header = header
            .cursor_pointer()
            .hover(|d| d.bg(rgb(crate::ui_theme::bg_selected())))
            .active(|d| d.opacity(0.9))
            .on_click(move |_event, _window, cx| {
                toggle.update(cx, |workspace, cx| {
                    if !workspace.git_collapsed_diff_files.remove(&toggle_path) {
                        workspace
                            .git_collapsed_diff_files
                            .insert(toggle_path.clone());
                    }
                    workspace.git_collapsed_diff_files_gen =
                        workspace.git_collapsed_diff_files_gen.wrapping_add(1);
                    cx.notify();
                });
                // 文件正文高度变化后，将滚动位置按相同增量移动，保留视口里的
                // 下一段代码；这比直接跳到文件标题顶部更接近 CSS sticky 的体验。
                if let Some((scroll, target_top, body_height)) = preserve_scroll.as_ref() {
                    let current_offset = scroll.offset();
                    let current_top = (-current_offset.y.as_f32()).max(0.0);
                    let anchored_top = sticky_toggle_scroll_top(
                        current_top,
                        *target_top,
                        row_height,
                        *body_height,
                        was_collapsed,
                    );
                    scroll.set_offset(point(current_offset.x, px(-anchored_top)));
                }
            });
    }
    header
}

/// 评论器只在点击锚点 `+` 后插入（拖选/滚动等任何其他状态变化不得改变
/// 虚拟列表高度），插入到被选中最后一行之后。选中行变化频繁，所以这段
/// 派生逻辑不缓存，渲染时对基础行动态执行。
pub(crate) fn insert_comment_composer(
    rows: &mut Vec<DiffReviewRow>,
    selected: &HashSet<usize>,
    comment_open: bool,
) {
    let Some(last_selected) = comment_open
        .then(|| selected.iter().max().copied())
        .flatten()
    else {
        return;
    };
    if let Some(insert_at) = rows
        .iter()
        .rposition(|row| matches!(row, DiffReviewRow::Line(index) if *index == last_selected))
    {
        rows.insert(insert_at + 1, DiffReviewRow::CommentComposer);
    }
}

pub(crate) struct GitDiffPaneParams<'a> {
    pub root: &'a str,
    pub git_diff: &'a Option<GitDiff>,
    /// diff 为空时没有派生数据可算，所以是 Option——布局仍然保留这一栏，
    /// 只是画成空态。
    pub derived: Option<&'a DiffDerivedCache>,
    /// 空态提示语。由调用方决定：工作区干净是"没有文件更改"，
    /// 有改动只是还没选文件则是"选择文件查看更改"。
    pub empty_hint: &'a str,
    pub collapsed_diff_files: &'a HashSet<String>,
    pub diff_selected: &'a HashSet<usize>,
    pub diff_selection_cursor: Option<usize>,
    pub diff_selection_dragging: bool,
    pub diff_comment_open: bool,
    pub diff_comment_input: Option<&'a Entity<gpui_component::input::TextareaState>>,
    pub diff_scroll: &'a VirtualListScrollHandle,
    pub diff_code_scroll: &'a ScrollHandle,
    pub active_hunk: Option<usize>,
}

pub(crate) fn git_diff_pane(params: GitDiffPaneParams<'_>, cx: &mut Context<Workspace>) -> Div {
    let GitDiffPaneParams {
        root,
        git_diff,
        derived,
        empty_hint,
        collapsed_diff_files,
        diff_selected,
        diff_selection_cursor,
        diff_selection_dragging,
        diff_comment_open,
        diff_comment_input,
        diff_scroll,
        diff_code_scroll,
        active_hunk,
    } = params;
    let (muted, fg, border, accent) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.border, t.accent)
    };
    match (git_diff, derived) {
        (Some(_), None) | (None, _) => placeholder_view(empty_hint, muted),
        (Some(d), Some(derived)) => {
            let name = d
                .path
                .rsplit('/')
                .next()
                .unwrap_or(d.path.as_str())
                .to_string();
            let lines = d.lines.clone();
            let ws = cx.entity();
            // 行号列宽等度量来自派生缓存（diff 内容不变时复用，避免每帧 O(n) 重算）。
            let (gutter_w, content_w) = (derived.gutter_w, derived.content_w);
            let code_w = (content_w - 2.0 - gutter_w).max(480.0);
            let hunk_ctx = HunkCtx {
                root: root.to_string(),
                ops: git_diff_hunk_ops(d),
                starts: Rc::new(
                    d.hunks
                        .iter()
                        .enumerate()
                        .map(|(n, h)| (h.range.start, n))
                        .collect(),
                ),
                active: active_hunk,
            };
            // 只给真实存在的普通工作区文件显示入口。已删除文件和子模块没有能由
            // FILES 面板读取的路径，不能把用户带到不可读占位页。
            let open_full_file = d.worktree_file.clone().map(|full_path| {
                let ws_open = ws.clone();
                div()
                    .id("diff-view-full-file")
                    .flex_none()
                    .px_2()
                    .py(px(1.0))
                    .text_xs()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(fg))
                    .child("在右侧 FILES 打开")
                    .on_click(move |_ev, window, cx| {
                        let path = full_path.clone();
                        ws_open.update(cx, |wsx, cx| {
                            // 完整文件固定在右侧 FILES 面板打开。若 Git 已展开到中央，
                            // stage_cover 继续保留 Git，从而形成中央 Git + 右侧 Files。
                            wsx.tool_panel_tab = crate::tool_panel::ToolPanelTab::Files;
                            wsx.set_tool_panel_open(true);
                            wsx.view_file(path, window, cx);
                            wsx.save_state(cx);
                        });
                    })
            });

            // 聚合 diff 的文件标题固定在滚动区顶部。标题状态直接从共享滚动句柄
            // 推导，滚动时 GPUI 会通知当前 view，因此不会额外引入轮询或状态字段。
            let comment_insert_at = (!derived.split && d.aggregate && diff_comment_open)
                .then(|| {
                    diff_selected.iter().max().and_then(|last_selected| {
                        derived.rows.iter().position(|row| {
                            matches!(row, DiffReviewRow::Line(index) if index == last_selected)
                        })
                    })
                })
                .flatten();
            let sticky_header = d
                .aggregate
                .then(|| {
                    aggregate_sticky_header(
                        &derived.sticky_headers,
                        diff_scroll.offset().y.as_f32(),
                        comment_insert_at,
                    )
                })
                .flatten();

            let list: AnyElement = if derived.split {
                let rows = derived.split_rows.clone();
                let lines2 = lines;
                let sel2 = diff_selected.clone();
                let ws2 = ws.clone();
                let hc = hunk_ctx;
                let sizes = Rc::new(
                    rows.iter()
                        .map(|_| size(px(content_w), px(FILE_LINE_H)))
                        .collect::<Vec<_>>(),
                );
                v_virtual_list(
                    ws.clone(),
                    "git-diff-split",
                    sizes,
                    move |_, range, _, _| {
                        range
                            .map(|i| {
                                div()
                                    .min_w(px(content_w))
                                    .child(render_split_row(
                                        i, &rows[i], &lines2, &sel2, &ws2, &hc, gutter_w,
                                    ))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .track_scroll(diff_scroll)
                .overflow_x_hidden()
                .flex_1()
                .min_h_0()
                .into_any_element()
            } else {
                // 基础行来自派生缓存；评论器按当前选行动态插入（选中行变化频繁，
                // 不进缓存）。
                let mut rows = derived.rows.clone();
                if diff_comment_open {
                    insert_comment_composer(
                        Rc::make_mut(&mut rows),
                        diff_selected,
                        diff_comment_open,
                    );
                }
                let sel2 = diff_selected.clone();
                let ws2 = ws.clone();
                let hc = hunk_ctx;
                let collapsed_files = collapsed_diff_files.clone();
                let input = diff_comment_input.cloned();
                let code_scroll = diff_code_scroll.clone();
                let sizes = Rc::new(
                    rows.iter()
                        .map(|row| match row {
                            DiffReviewRow::Header { .. } => {
                                size(px(content_w), px(FILE_LINE_H + 8.))
                            }
                            DiffReviewRow::Line(_) => size(px(content_w), px(FILE_LINE_H)),
                            DiffReviewRow::CommentComposer => {
                                size(px(content_w), px(COMMENT_COMPOSER_H))
                            }
                        })
                        .collect::<Vec<_>>(),
                );
                v_virtual_list(ws.clone(), "git-diff", sizes, move |_, range, _, cx| {
                    range
                        .map(|i| match &rows[i] {
                            DiffReviewRow::Header { path, adds, dels } => {
                                let path = path.clone();
                                let collapsed = collapsed_files.contains(&path);
                                let toggle = ws2.clone();
                                render_aggregate_diff_header(
                                    ("aggregate-diff-file", i),
                                    path,
                                    *adds,
                                    *dels,
                                    Some(collapsed),
                                    Some(toggle),
                                    None,
                                )
                                .into_any_element()
                            }
                            DiffReviewRow::Line(line_ix) => div()
                                .w_full()
                                .child(render_diff_line(
                                    *line_ix,
                                    &lines[*line_ix],
                                    DiffLineParams {
                                        selected: sel2.contains(line_ix),
                                        // `+` 是松开后才出现的确认控件；按住拖拽时只保留
                                        // 选区高亮，避免它在指针下突然弹出抢走视觉焦点。
                                        show_comment_anchor: !diff_selection_dragging
                                            && diff_selection_cursor == Some(*line_ix),
                                        workspace: &ws2,
                                        hunks: &hc,
                                        gutter_width: gutter_w,
                                        code_scroll: &code_scroll,
                                        code_width: code_w,
                                    },
                                ))
                                .into_any_element(),
                            // 评论器不是一条“代码行”。把它放在正文列里，左侧留出与
                            // diff 色条 + 行号栏等宽的轨道：行号仍可读、范围仍能从
                            // gutter 拖选，而评论卡和代码正文严格对齐。
                            DiffReviewRow::CommentComposer => div()
                                // 虚拟列表会以当前可视宽度布局每一个纵向条目。评论器
                                // 是固定的阅读/输入界面，不能沿用代码行的超宽画布；否则
                                // 一旦代码横向滚动，卡片会被撑到数千像素宽。
                                .w_full()
                                .flex()
                                .items_stretch()
                                .child(
                                    div()
                                        .w(px(2. + gutter_w))
                                        .flex_none()
                                        .bg(rgb(crate::ui_theme::bg_hover())),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .px_3()
                                        .py_3()
                                        // 代码画布可以很宽以容纳横向滚动，但评论是阅读和
                                        // 输入界面，限制在舒服的行长内，不能被推到画布右端。
                                        .child(div().w_full().max_w(px(960.)).child(
                                            diff_comment_bar(&sel2, &lines, input.as_ref(), cx),
                                        )),
                                )
                                .into_any_element(),
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(diff_scroll)
                .overflow_x_hidden()
                .flex_1()
                .min_h_0()
                .into_any_element()
            };

            // 「统一 / 并排」切换按钮。
            let toggle = div()
                .id("diff-split-toggle")
                .flex_none()
                .px_2()
                .py(px(1.0))
                .text_xs()
                .whitespace_nowrap()
                .rounded_sm()
                .cursor_pointer()
                .text_color(fg)
                .bg(accent)
                .hover(|d| d.opacity(0.8))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.diff_split = !this.diff_split;
                    cx.notify();
                }))
                .child(
                    if derived.split {
                        "并排 ⇄"
                    } else {
                        "统一 ☰"
                    }
                    .to_string(),
                );
            let collapse_all = (d.aggregate && !derived.split).then(|| {
                let paths = aggregate_file_header_paths(&d.lines);
                let all_collapsed = aggregate_all_files_collapsed(&paths, collapsed_diff_files);
                div()
                    .id("diff-collapse-all")
                    .flex_none()
                    .px_2()
                    .py(px(1.0))
                    .text_xs()
                    .whitespace_nowrap()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(fg)
                    .hover(|d| d.bg(rgb(crate::ui_theme::bg_hover())))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_all_aggregate_diff_files(cx);
                    }))
                    .child(if all_collapsed {
                        "全部展开"
                    } else {
                        "全部收起"
                    })
            });

            let list_finish_ws = ws.clone();
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                // diff 是代码阅读面，不应继承工作区的正文大字号。字体与行号宽度
                // 的估算统一为同一套等宽小号字，窄 Tool Panel 下仍能完整显示行号。
                .font_family(crate::terminal_view::font_family())
                .text_sm()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(muted)
                        .border_b_1()
                        .border_color(border)
                        .justify_end()
                        .children(open_full_file)
                        .children(collapse_all)
                        .child(toggle),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .min_w_0()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(muted)
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .whitespace_nowrap()
                                .child(name),
                        ),
                )
                // 可变高度虚拟列表持有唯一滚动句柄；评论卡片插入后仍只渲染可见行。
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .flex()
                        .flex_col()
                        // sticky 标题是滚动列表上方的装饰层。裁剪在视口内，才能让
                        // 下一个文件标题把当前标题逐步顶出，而不是露到面板外。
                        .overflow_hidden()
                        // 行与行之间、以及滚动区底部的空白也要能结束拖拽；否则
                        // 用户松开鼠标的位置恰好不在行号格时，状态会悬挂到下一次操作。
                        .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                            list_finish_ws
                                .update(cx, |this, cx| this.finish_diff_selection(window, cx));
                        })
                        .child(list)
                        .children(sticky_header.map(|header| {
                            let sticky_top = header.top
                                + if comment_insert_at
                                    .is_some_and(|insert_at| header.row_index > insert_at)
                                {
                                    COMMENT_COMPOSER_H
                                } else {
                                    0.0
                                };
                            let (collapsed, toggle) = if derived.split {
                                (None, None)
                            } else {
                                (
                                    Some(collapsed_diff_files.contains(&header.path)),
                                    Some(ws.clone()),
                                )
                            };
                            render_aggregate_diff_header(
                                "aggregate-diff-sticky-header",
                                header.path,
                                header.adds,
                                header.dels,
                                collapsed,
                                toggle,
                                Some((diff_scroll.clone(), sticky_top, header.body_height)),
                            )
                            .absolute()
                            .top(px(header.offset_y))
                            .left_0()
                            .right_0()
                            .block_mouse_except_scroll()
                            .shadow_md()
                        }))
                        .vertical_scrollbar(diff_scroll)
                        .horizontal_scrollbar(diff_code_scroll),
                )
        }
    }
}

/// 交互式 diff 评论器：选中增删行后在正文列内展开，自动聚焦输入框。
/// 它由 `DiffReviewRow::CommentComposer` 插到选区末尾，行号轨道不被卡片覆盖。
pub(crate) fn diff_comment_bar(
    selected: &HashSet<usize>,
    lines: &[DiffLine],
    input: Option<&Entity<gpui_component::input::TextareaState>>,
    cx: &mut Context<Workspace>,
) -> Div {
    let (muted, border) = {
        let t = cx.theme();
        (t.muted_foreground, t.border)
    };
    let n = selected.len();
    let ws = cx.entity();
    if n == 0 {
        return div()
            .flex()
            .items_center()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child("点选增删行即可评论；反馈会附带文件和行号发送给当前终端");
    }

    let clear_ws = ws.clone();
    let mut line_numbers = selected
        .iter()
        .filter_map(|&i| lines.get(i).and_then(|line| line.new_ln.or(line.old_ln)))
        .collect::<Vec<_>>();
    line_numbers.sort_unstable();
    let range_label = match (line_numbers.first(), line_numbers.last()) {
        (Some(first), Some(last)) if first != last => {
            format!("对第 L{first} 行至第 L{last} 行发表评论")
        }
        (Some(line), _) => format!("对第 L{line} 行发表评论"),
        _ => format!("对选中 {n} 行发表评论"),
    };
    v_flex()
        .gap_3()
        .p_4()
        .bg(rgb(crate::ui_theme::bg_card()))
        .border_1()
        .border_color(border)
        .rounded_lg()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(26.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(rgb(crate::ui_theme::bg_hover()))
                                .child(Icon::new(IconName::Bot).size(px(15.))),
                        )
                        .child(div().text_sm().font_semibold().child("本机留言")),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(range_label),
                ),
        )
        .children(input.map(|state| {
            div()
                .w_full()
                .child(gpui_component::input::Textarea::new(state))
        }))
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("diff-comment-cancel")
                        .small()
                        .label("取消")
                        .on_click(move |_ev, window, cx| {
                            clear_ws.update(cx, |this, cx| {
                                this.clear_diff_comment_selection(window, cx)
                            });
                        }),
                )
                .child(Button::new("diff-send").small().label("发送评论").on_click(
                    move |_ev, window, cx| {
                        ws.update(cx, |this, cx| this.send_diff_comments(window, cx));
                    },
                )),
        )
}
