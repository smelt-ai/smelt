//! 变更栏正文用 gpui-component 的 Diff。日志页的单文件提交 diff 也用它，见 `git_log_view`。
//!
//! 文件列表、暂存范围、丢弃确认留在面板里。块操作没有 hunk 头槽位，
//! 挂在该块第一行的注释下面。当前文件名由 Diff 钉在视口顶上。
//!
//! 正文是单独的缓存视图。渲染时不能 `update` Workspace，否则这次读取
//! 会记进缓存视图的依赖，文件列表一滚动就把长 diff 再排一遍。
//! 工具条和批注用的快照在装补丁、改评论时写进来。

use std::rc::Rc;

use super::*;
use gpui_component::button::Toggle;
use gpui_component::diff::{Diff, DiffAnnotation, DiffLinePosition, DiffLineRange, DiffSide};

/// 块操作挂在哪一行。优先新文件侧的行号，纯删除才退回旧文件侧。
///
/// 返回 `(true, 行号)` 表示 Modified，`(false, 行号)` 表示 Original。行号从 1 起。
pub(crate) fn hunk_line_anchor(lines: &[DiffLine], hunk: &DiffHunk) -> Option<(bool, usize)> {
    let slice = lines.get(hunk.range.clone())?;
    for line in slice {
        if let Some(n) = line.new_ln.filter(|n| *n > 0) {
            return Some((true, n as usize));
        }
    }
    for line in slice {
        if let Some(n) = line.old_ln.filter(|n| *n > 0) {
            return Some((false, n as usize));
        }
    }
    None
}

/// 选中行和评论拼成写进当前终端的文本。不附带回车。
pub(crate) fn diff_feedback_text(
    path: &str,
    start: usize,
    end: usize,
    selected: &str,
    comment: &str,
) -> String {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let mut msg = format!("对 {path} 的这几行有反馈：\n");
    if start == end {
        msg.push_str(&format!("L{start}\n"));
    } else {
        msg.push_str(&format!("L{start}-L{end}\n"));
    }
    if !selected.is_empty() {
        msg.push_str(selected);
        if !selected.ends_with('\n') {
            msg.push('\n');
        }
    }
    if !comment.is_empty() {
        msg.push_str(&format!("\n{comment}\n"));
    }
    msg
}

/// 右侧状态路径在组件里对应的 [`DiffFile::path`]。
///
/// 删除文件的展示路径是旧路径，重命名则是新路径。状态栏点的是 porcelain 路径，
/// 可能是其中任意一侧。对不上就返回 `None`，调用方不要拿标题行的文本去滚。
pub(crate) fn diff_scroll_target<'a>(
    files: &'a [gpui_component::diff::DiffFile],
    requested: &str,
) -> Option<&'a str> {
    files.iter().find_map(|file| {
        let show = file.path().as_ref();
        let hit = show == requested
            || file
                .modified_path()
                .is_some_and(|path| path.as_ref() == requested)
            || file
                .original_path()
                .is_some_and(|path| path.as_ref() == requested);
        hit.then_some(show)
    })
}

/// 画工具条和批注需要的那一份快照。行和块是 `Rc`，装补丁时克隆指针即可。
struct DiffChrome {
    view: Entity<gpui_component::diff::DiffState>,
    name: String,
    aggregate: bool,
    root: String,
    path: String,
    worktree_file: Option<String>,
    lines: Rc<Vec<DiffLine>>,
    hunks: Rc<Vec<DiffHunk>>,
    ops: HunkOps,
    split: bool,
    comment_range: Option<DiffLineRange>,
    comment_input: Option<Entity<gpui_component::input::TextareaState>>,
}

/// 变更栏 diff 正文。Workspace 持有它，它只弱引用 Workspace，避免互相留住。
/// 渲染只读自己的快照和 `DiffState`。
pub(crate) struct GitDiffBody {
    host: WeakEntity<Workspace>,
    chrome: Option<DiffChrome>,
}

impl GitDiffBody {
    pub(crate) fn new(host: WeakEntity<Workspace>) -> Self {
        Self { host, chrome: None }
    }

    fn set_chrome(&mut self, chrome: DiffChrome, cx: &mut Context<Self>) {
        self.chrome = Some(chrome);
        cx.notify();
    }
}

impl Render for GitDiffBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_chrome(cx).into_any_element()
    }
}

/// 当前聚合 diff 是否已经具备文件树导航所需的数据。
///
/// 只看 Smelt 自己的文件标题行，不读 Diff 组件的滚动位置。
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

/// 解析失败时给面板看的说明。空补丁不是失败；不像 diff 的原文（git 报错、子模块）原样显示。
pub(crate) fn diff_parse_failure(patch: &str, message: &str) -> String {
    let trimmed = patch.trim();
    if trimmed.is_empty() {
        return "（无差异）".into();
    }
    let looks_like_diff = trimmed.lines().any(|line| {
        line.starts_with("diff ") || line.starts_with("--- ") || line.starts_with("@@")
    });
    if looks_like_diff {
        message.to_string()
    } else {
        trimmed.to_string()
    }
}

impl Workspace {
    /// 变更栏左侧的 diff 正文。加载和报错留在这里；已装上的 Diff 交给缓存视图。
    ///
    /// 这里不能 `read` DiffState。render 期间读到的实体，之后每次 `notify`
    /// 都会把整个 Workspace 标脏，右侧文件列表就跟着 diff 的高亮和滚动重排。
    pub(crate) fn git_diff_pane(&self, empty_hint: &str, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let Some(diff) = self.git_diff.as_ref() else {
            return placeholder_view(empty_hint, muted);
        };
        if diff.patch.is_none() {
            return placeholder_view("正在读取…", muted);
        }
        if let Some(err) = self.diff_parse_error.as_deref() {
            return diff_status_note(err, muted);
        }
        if self.diff_view.is_none() {
            return placeholder_view("（无差异）", muted);
        }
        let Some(body) = self.diff_body.clone() else {
            return placeholder_view("（无差异）", muted);
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(body.cached(StyleRefinement::default().size_full()))
    }

    pub(crate) fn publish_diff_body(&mut self, cx: &mut Context<Self>) {
        let Some(body) = self.diff_body.clone() else {
            return;
        };
        let Some(view) = self.diff_view.clone() else {
            return;
        };
        let Some(diff) = self.git_diff.as_ref() else {
            return;
        };
        let name = diff
            .path
            .rsplit('/')
            .next()
            .unwrap_or(diff.path.as_str())
            .to_string();
        let chrome = DiffChrome {
            view,
            name,
            aggregate: diff.aggregate,
            root: diff.root.clone(),
            path: diff.path.clone(),
            worktree_file: diff.worktree_file.clone(),
            lines: Rc::clone(&diff.lines),
            hunks: Rc::clone(&diff.hunks),
            ops: git_diff_hunk_ops(diff),
            split: self.diff_split,
            comment_range: self
                .diff_comment_open
                .then(|| self.diff_comment_range.clone())
                .flatten(),
            comment_input: self.diff_comment_input.clone(),
        };
        body.update(cx, |body, cx| body.set_chrome(chrome, cx));
    }

    fn toggle_diff_split(&mut self, cx: &mut Context<Self>) {
        self.diff_split = !self.diff_split;
        let mode = self.current_diff_mode();
        if let Some(view) = self.diff_view.clone() {
            view.update(cx, |state, cx| state.set_mode(mode, cx));
        }
        self.publish_diff_body(cx);
    }

    fn open_diff_comment(
        &mut self,
        range: DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_comment_range = Some(range);
        self.diff_comment_open = true;
        if let Some(input) = self.diff_comment_input.clone() {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        self.publish_diff_body(cx);
    }
}

impl GitDiffBody {
    fn render_chrome(&self, cx: &mut Context<Self>) -> Div {
        let (muted, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return placeholder_view("（无差异）", muted);
        };
        let view = chrome.view.clone();
        let (file_path, all_collapsed) = {
            let state = view.read(cx);
            let files = state.files();
            if files.is_empty() {
                return placeholder_view("（无差异）", muted);
            }
            let all_collapsed = !files.is_empty()
                && files
                    .iter()
                    .all(|file| state.is_file_collapsed(file.path().as_ref()));
            let file_path = (files.len() == 1).then(|| files[0].path().clone());
            (file_path, all_collapsed)
        };
        let name = chrome.name.clone();
        let host = self.host.clone();
        let ops = chrome.ops;
        let annotations = diff_annotations(
            &chrome.lines,
            &chrome.hunks,
            !chrome.aggregate && ops != HunkOps::None,
            file_path.as_deref(),
            chrome.comment_range.as_ref(),
        );
        let comment_label = chrome.comment_range.as_ref().map(comment_range_label);
        let input = chrome.comment_input.clone();
        let open_full_file = chrome.worktree_file.clone().map(|full_path| {
            let host = host.clone();
            Button::new("diff-view-full-file")
                .quiet()
                .icon_size()
                .flex_none()
                .label("在右侧 FILES 打开")
                .text_color(muted)
                .on_click(move |_ev, window, cx| {
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    let path = full_path.clone();
                    host.update(cx, |this, cx| {
                        this.tool_panel_tab = crate::tool_panel::ToolPanelTab::Files;
                        this.set_tool_panel_open(true, cx);
                        this.schedule_visible_refresh(cx);
                        this.view_file(path, window, cx);
                        this.save_state(cx);
                    });
                })
        });
        let collapse_all = chrome.aggregate.then(|| {
            let host = host.clone();
            Button::new("diff-collapse-all")
                .quiet()
                .icon_size()
                .flex_none()
                .label(if all_collapsed {
                    "全部展开"
                } else {
                    "全部收起"
                })
                .on_click(move |_ev, _window, cx| {
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    host.update(cx, |this, cx| this.toggle_all_aggregate_diff_files(cx));
                })
        });
        // 并排是开关：按下是并排，抬起是统一。
        let toggle = Toggle::new("diff-split-toggle")
            .xsmall()
            .flex_none()
            .px_2()
            .label("并排")
            .checked(chrome.split)
            .tooltip("并排对照新旧两侧；关掉是统一视图")
            .on_click({
                let host = host.clone();
                move |_, _window, cx| {
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    host.update(cx, |this, cx| this.toggle_diff_split(cx));
                }
            });
        let stage_path = (!chrome.aggregate).then(|| chrome.path.clone());
        let root = chrome.root.clone();
        let annotation_root = root.clone();

        // 缓存视图按给定边界做根布局，flex_1 在根上不会撑满。
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
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
            .child(
                div().flex_1().min_h_0().w_full().child(
                    Diff::new(&view)
                        .size_full()
                        .annotations(annotations)
                        .render_header_suffix({
                            let root = root.clone();
                            let host = host.clone();
                            move |file, _window, _cx| {
                                diff_header_suffix(file, ops, stage_path.clone(), &root, &host)
                            }
                        })
                        .render_annotation({
                            let host = host.clone();
                            move |annotation, _window, cx| {
                                render_diff_annotation(
                                    annotation,
                                    ops,
                                    annotation_root.clone(),
                                    comment_label.clone(),
                                    input.clone(),
                                    host.clone(),
                                    cx,
                                )
                            }
                        })
                        .on_add_annotation({
                            let host = host.clone();
                            move |range, window, cx| {
                                let Some(host) = host.upgrade() else {
                                    return;
                                };
                                let range = range.clone();
                                host.update(cx, |this, cx| {
                                    this.open_diff_comment(range, window, cx)
                                });
                            }
                        }),
                ),
            )
    }
}

fn diff_status_note(text: &str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .p_4()
        .text_sm()
        .whitespace_normal()
        .text_color(muted)
        .child(text.to_string())
}

fn comment_range_label(range: &DiffLineRange) -> String {
    let start = range.start().min(range.end());
    let end = range.start().max(range.end());
    if start == end {
        format!("对第 L{start} 行发表评论")
    } else {
        format!("对第 L{start} 行至第 L{end} 行发表评论")
    }
}

fn diff_annotations(
    lines: &[DiffLine],
    hunks: &[DiffHunk],
    show_hunks: bool,
    file_path: Option<&str>,
    comment: Option<&DiffLineRange>,
) -> Vec<DiffAnnotation> {
    let mut items = Vec::new();
    if show_hunks && let Some(path) = file_path {
        for (idx, hunk) in hunks.iter().enumerate() {
            let Some((modified, line)) = hunk_line_anchor(lines, hunk) else {
                continue;
            };
            let side = if modified {
                DiffSide::Modified
            } else {
                DiffSide::Original
            };
            items.push(DiffAnnotation::line(
                ElementId::named_usize("hunk", idx),
                DiffLinePosition::new(path, side, line),
            ));
        }
    }
    if let Some(range) = comment {
        items.push(DiffAnnotation::line(
            ElementId::Name("diff-comment".into()),
            DiffLinePosition::new(range.path().clone(), range.end_side(), range.end()),
        ));
    }
    items
}

fn diff_header_suffix(
    file: &gpui_component::diff::DiffFile,
    ops: HunkOps,
    stage_path: Option<String>,
    root: &str,
    host: &WeakEntity<Workspace>,
) -> Div {
    let mut row = div().flex().items_center().gap_2();
    if let Some(path) = stage_path {
        let label = match ops {
            HunkOps::StageDiscard => Some("暂存"),
            HunkOps::Unstage => Some("取消暂存"),
            HunkOps::None => None,
        };
        if let Some(label) = label {
            let host = host.clone();
            let root = root.to_string();
            row = row.child(
                Button::new("diff-stage-file")
                    .quiet()
                    .icon_size()
                    .label(label)
                    .on_click(move |_ev, _window, cx| {
                        let Some(host) = host.upgrade() else {
                            return;
                        };
                        let (root, path) = (root.clone(), path.clone());
                        host.update(cx, |this, cx| match ops {
                            HunkOps::Unstage => this.unstage_file(root, path, cx),
                            _ => this.stage_file(root, path, cx),
                        });
                    }),
            );
        }
    }
    if let Some(rel) = file.modified_path() {
        let Some(full) = full_file_path(root, rel, false) else {
            return row;
        };
        let host = host.clone();
        row = row.child(
            Button::new((ElementId::from("diff-open-header"), rel.clone()))
                .quiet()
                .icon_size()
                .label("打开")
                .on_click(move |_ev, window, cx| {
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    let path = full.clone();
                    host.update(cx, |this, cx| {
                        this.tool_panel_tab = crate::tool_panel::ToolPanelTab::Files;
                        this.set_tool_panel_open(true, cx);
                        this.schedule_visible_refresh(cx);
                        this.view_file(path, window, cx);
                        this.save_state(cx);
                    });
                }),
        );
    }
    row
}

fn render_diff_annotation(
    annotation: &DiffAnnotation,
    ops: HunkOps,
    root: String,
    comment_label: Option<String>,
    input: Option<Entity<gpui_component::input::TextareaState>>,
    host: WeakEntity<Workspace>,
    cx: &mut App,
) -> AnyElement {
    match annotation.id() {
        ElementId::NamedInteger(name, ix) if name.as_ref() == "hunk" => {
            hunk_ops_row(*ix as usize, ops, root, host, cx).into_any_element()
        }
        ElementId::Name(name) if name.as_ref() == "diff-comment" => {
            let Some(label) = comment_label else {
                return div().into_any_element();
            };
            diff_comment_composer(label, input, host, cx).into_any_element()
        }
        _ => div().into_any_element(),
    }
}

fn hunk_ops_row(
    idx: usize,
    ops: HunkOps,
    root: String,
    host: WeakEntity<Workspace>,
    _cx: &App,
) -> Div {
    let quiet = |id: &'static str, label: &'static str| {
        Button::new((id, idx)).quiet().icon_size().label(label)
    };
    let danger = |id: &'static str, label: &'static str| {
        Button::new((id, idx)).danger().icon_size().label(label)
    };
    let row = div().flex().items_center().gap_2();
    match ops {
        HunkOps::None => row,
        HunkOps::StageDiscard => {
            let (stage_host, stage_root) = (host.clone(), root.clone());
            let (discard_host, discard_root) = (host, root);
            row.child(
                quiet("hunk-stage", "暂存块").on_click(move |_ev, _window, cx| {
                    let Some(host) = stage_host.upgrade() else {
                        return;
                    };
                    let root = stage_root.clone();
                    host.update(cx, |this, cx| this.stage_hunk(root, idx, cx));
                }),
            )
            .child(
                danger("hunk-discard", "丢弃块").on_click(move |_ev, _window, cx| {
                    let Some(host) = discard_host.upgrade() else {
                        return;
                    };
                    let root = discard_root.clone();
                    host.update(cx, |this, cx| this.start_discard_hunk(root, idx, cx));
                }),
            )
        }
        HunkOps::Unstage => row.child(quiet("hunk-unstage", "取消暂存块").on_click(
            move |_ev, _window, cx| {
                let Some(host) = host.upgrade() else {
                    return;
                };
                let root = root.clone();
                host.update(cx, |this, cx| this.unstage_hunk(root, idx, cx));
            },
        )),
    }
}

fn diff_comment_composer(
    range_label: String,
    input: Option<Entity<gpui_component::input::TextareaState>>,
    host: WeakEntity<Workspace>,
    cx: &mut App,
) -> Div {
    let (muted, border) = {
        let theme = cx.theme();
        (theme.muted_foreground, theme.border)
    };
    let clear_host = host.clone();
    v_flex()
        .gap_3()
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
                .child(gpui_component::input::Textarea::new(&state))
        }))
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("diff-comment-cancel")
                        .compact()
                        .label("取消")
                        .on_click(move |_ev, window, cx| {
                            let Some(host) = clear_host.upgrade() else {
                                return;
                            };
                            host.update(cx, |this, cx| {
                                this.clear_diff_comment_selection(window, cx);
                            });
                        }),
                )
                .child(
                    Button::new("diff-send")
                        .compact()
                        .label("发送评论")
                        .on_click(move |_ev, window, cx| {
                            let Some(host) = host.upgrade() else {
                                return;
                            };
                            host.update(cx, |this, cx| this.send_diff_comments(window, cx));
                        }),
                ),
        )
        .border_color(border)
}
