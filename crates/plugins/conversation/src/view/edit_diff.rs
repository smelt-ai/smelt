//! 点改动标签弹出的 diff：一段（或一次工具调用）改过的文件排成一排标签，下面是选中那个
//! 文件的改动。同一文件改了几次就按先后排几块，每块是那一次工具调用给的前后对比。
//!
//! 不显示行号：有的智能体只送被替换的那一小段，行号会从 1 数起，是错的。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, ElementId, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use rust_i18n::t;
use smelt_plugin_api::model::acp_chat::{DiffLineTag, diff_lines};
use smelt_plugin_api::ui::diff::{Diff, DiffFile, DiffHunkSeparator, DiffState};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};
use smelt_plugin_api::ui::{Button, IconAction as _, IconName, ui_theme};
use smelt_plugin_api::{FileLocation, RuntimeKey};

/// 一处修改：那次工具调用给的前后内容。新建的文件没有前。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Edit {
    pub old: Option<String>,
    pub new: String,
}

/// 弹窗里的一个文件。增减是各次修改加起来的，和对话里标签上的数一致。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditFile {
    /// 工具给的路径，多是绝对路径。
    pub path: String,
    /// 标签上的名字。
    pub label: String,
    pub added: usize,
    pub removed: usize,
    pub edits: Vec<Edit>,
}

/// 打开弹窗，选中第 `selected` 个文件。`cwd` 下的文件标题写相对路径。
pub(super) fn open(
    files: Vec<EditFile>,
    selected: usize,
    cwd: Option<String>,
    machine: RuntimeKey,
    window: &mut Window,
    cx: &mut App,
) {
    if files.is_empty() {
        return;
    }
    let dialog = cx.new(|cx| EditDiffDialog::new(files, selected, cwd, machine, cx));
    let focus = dialog.read(cx).focus.clone();
    smelt_plugin_api::ui::dialog::open(window, cx, move |card, _, _| {
        // 宽度够放一行代码；窗口更窄时对话框自己夹到窗口以内。高度跟着内容走，见 render。
        card.w(px(WIDTH))
            .p_0()
            .gap_0()
            .overflow_hidden()
            .child(dialog.clone())
    });
    // 焦点落在弹窗自己身上，←/→ 才能切文件；Esc 仍由外面的对话框接。
    window.defer(cx, move |window, cx| window.focus(&focus, cx));
}

/// 弹窗宽度，约一百多个等宽字符。
const WIDTH: f32 = 960.;
/// 内容再多，弹窗也只占窗口这么高，剩下的在 diff 里滚。
const MAX_HEIGHT_RATIO: f32 = 0.8;

pub(super) struct EditDiffDialog {
    files: Vec<EditFile>,
    selected: usize,
    cwd: Option<String>,
    machine: RuntimeKey,
    diff: Entity<DiffState>,
    /// 选中文件有几块能看的修改（前后一样的那次不算）。
    blocks: usize,
    /// 「在文件里打开」失败的原因，就地显示在按钮旁边。
    open_error: Option<String>,
    focus: FocusHandle,
}

impl EditDiffDialog {
    fn new(
        files: Vec<EditFile>,
        selected: usize,
        cwd: Option<String>,
        machine: RuntimeKey,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = selected.min(files.len() - 1);
        let diff_files = diff_files(&files[selected]);
        let blocks = diff_files.len();
        let diff = cx.new(|cx| DiffState::new(diff_files, cx));
        Self {
            files,
            selected,
            cwd,
            machine,
            diff,
            blocks,
            open_error: None,
            focus: cx.focus_handle(),
        }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == self.selected || ix >= self.files.len() {
            return;
        }
        self.selected = ix;
        self.open_error = None;
        let files = diff_files(&self.files[ix]);
        self.blocks = files.len();
        self.diff.update(cx, |state, cx| state.set_files(files, cx));
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
            return;
        }
        let next = match event.keystroke.key.as_str() {
            "left" => self.selected.checked_sub(1),
            "right" => Some(self.selected + 1),
            _ => None,
        };
        if let Some(next) = next {
            self.select(next, cx);
            cx.stop_propagation();
        }
    }

    fn open_in_file(&mut self, cx: &mut Context<Self>) {
        let location = FileLocation {
            machine: self.machine.clone(),
            path: self.files[self.selected].path.clone(),
            line: None,
        };
        let opened = smelt_plugin_api::open_file(location, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = opened.await {
                this.update(cx, |this, cx| {
                    this.open_error = Some(error);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let file = &self.files[self.selected];
        let (dir, name) = split_title(&file.path, self.cwd.as_deref());
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .pt_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .type_style(TYPE.ui)
                    // 路径太长时截目录，文件名留全。
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(ui_theme::muted()))
                            .child(dir),
                    )
                    .child(
                        div()
                            .flex_none()
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::text()))
                            .child(name),
                    ),
            )
            .when_some(self.open_error.clone(), |row, error| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(320.))
                        .truncate()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::bad()))
                        .child(error),
                )
            })
            .child(
                Button::new("edit-diff-open-in-file")
                    .quiet()
                    .small()
                    .flex_none()
                    .label(t!("diff.open_in_file"))
                    .on_click(cx.listener(|this, _, _, cx| this.open_in_file(cx))),
            )
            .child(
                Button::new("edit-diff-close")
                    .quiet()
                    .small()
                    .flex_none()
                    .icon(IconName::Close)
                    .icon_action(t!("action.close"))
                    .on_click(|_, window, cx| smelt_plugin_api::ui::dialog::close(window, cx)),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_wrap().gap_1p5().px_4().pt_3().children(
            self.files.iter().enumerate().map(|(ix, file)| {
                edit_chip(
                    ("edit-diff-tab", ix),
                    &file.label,
                    file.added,
                    file.removed,
                    ix == self.selected,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            }),
        )
    }
}

impl Focusable for EditDiffDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EditDiffDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("edit-diff-dialog")
            .key_context("EditDiffDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w_full()
            .max_h(window.viewport_size().height * MAX_HEIGHT_RATIO)
            .flex()
            .flex_col()
            .child(self.render_title(cx))
            .when(self.files.len() > 1, |col| col.child(self.render_tabs(cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .mt_3()
                    .border_t_1()
                    .border_color(ui_theme::hair())
                    .map(|body| {
                        if self.blocks == 0 {
                            return body.p_4().child(
                                div()
                                    .type_style(TYPE.ui)
                                    .text_color(rgb(ui_theme::muted()))
                                    .child(SharedString::from(t!("diff.empty"))),
                            );
                        }
                        body.child(
                            Diff::new(&self.diff)
                                .fit_height(true)
                                .line_number(false)
                                .hunk_separator(DiffHunkSeparator::Simple)
                                // 只有一块就不要块头，文件名已经在标题里。
                                .header_visible(self.blocks > 1)
                                .render_header(|file, _, _| {
                                    render_edit_header(file).into_any_element()
                                }),
                        )
                    }),
            )
    }
}

/// 改过的一个文件的标签：等宽的名字，后面绿的加、红的减。对话里段落下的一排和弹窗顶上
/// 的一排是同一个样子；选中的边线加深、垫底色。
pub(super) fn edit_chip(
    id: impl Into<ElementId>,
    label: &str,
    added: usize,
    removed: usize,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap_1p5()
        .px_2()
        .py(px(1.))
        .rounded_md()
        .border_1()
        .border_color(if selected {
            ui_theme::hair_strong()
        } else {
            ui_theme::hair()
        })
        .when(selected, |chip| chip.bg(ui_theme::row_hover()))
        .hover(|chip| chip.border_color(ui_theme::hair_strong()))
        .cursor_pointer()
        .font_family(smelt_plugin_api::model::font_config::font_family())
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::text()))
        .child(SharedString::from(label.to_string()))
        .when(added > 0, |chip| {
            chip.child(
                div()
                    .text_color(rgb(ui_theme::ok()))
                    .child(format!("+{added}")),
            )
        })
        .when(removed > 0, |chip| {
            chip.child(
                div()
                    .text_color(rgb(ui_theme::bad()))
                    .child(format!("−{removed}")),
            )
        })
}

/// 第几次修改的块头：左边序号，右边这一次的增减。
fn render_edit_header(file: &DiffFile) -> Div {
    let nth = edit_ordinal(file.path()).unwrap_or(1);
    div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .child(
            div()
                .flex_1()
                .type_style(TYPE.ui_medium)
                .child(SharedString::from(t!("diff.nth_edit", n = nth))),
        )
        .child(
            div()
                .type_style(TYPE.small)
                .text_color(rgb(ui_theme::muted()))
                .child(change_counts(file.additions(), file.deletions())),
        )
}

/// `+加 −减`，是零的那一半不写，和标签上一样。
fn change_counts(added: usize, removed: usize) -> String {
    match (added, removed) {
        (0, removed) => format!("−{removed}"),
        (added, 0) => format!("+{added}"),
        (added, removed) => format!("+{added} −{removed}"),
    }
}

/// 一个文件的各次修改，转成 diff 组件的文件。组件按路径认文件，同一文件的几次修改
/// 用 `<序号>/<路径>` 区分；路径的扩展名照旧决定语法高亮。前后一样的修改没有内容，跳过。
fn diff_files(file: &EditFile) -> Vec<DiffFile> {
    file.edits
        .iter()
        .enumerate()
        .filter_map(|(ix, edit)| {
            let path = format!("{}/{}", ix + 1, file.path.trim_start_matches('/'));
            unified_patch(&path, edit.old.as_deref().unwrap_or(""), &edit.new)
        })
        .flat_map(|patch| {
            DiffFile::parse(&patch).unwrap_or_else(|error| {
                eprintln!("[conversation] 修改转不成 diff：{error}");
                Vec::new()
            })
        })
        .collect()
}

/// `<序号>/<路径>` 里的序号，见 [`diff_files`]。
fn edit_ordinal(path: &str) -> Option<usize> {
    path.split_once('/')?.0.parse().ok()
}

/// 整个文件做上下文的统一 diff，折叠交给组件，用户想看多少展开多少。逐行结果和对话里
/// 标签上的增减出自同一个 [`diff_lines`]，数对得上。前后一样时没有补丁。
fn unified_patch(path: &str, old: &str, new: &str) -> Option<String> {
    let lines = diff_lines(old, new);
    if lines.iter().all(|line| line.tag == DiffLineTag::Context) {
        return None;
    }
    let old_len = lines.iter().filter(|l| l.tag != DiffLineTag::Added).count();
    let new_len = lines
        .iter()
        .filter(|l| l.tag != DiffLineTag::Removed)
        .count();
    let start = |len: usize| usize::from(len > 0);
    let mut patch = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{},{old_len} +{},{new_len} @@\n",
        start(old_len),
        start(new_len),
    );
    for line in &lines {
        patch.push(match line.tag {
            DiffLineTag::Context => ' ',
            DiffLineTag::Removed => '-',
            DiffLineTag::Added => '+',
        });
        patch.push_str(&line.text);
        patch.push('\n');
        if line.no_newline {
            patch.push_str("\\ No newline at end of file\n");
        }
    }
    Some(patch)
}

/// 标题拆成灰的目录（带结尾的 `/`）和亮的文件名。在 `cwd` 下的写相对路径。
fn split_title(path: &str, cwd: Option<&str>) -> (String, String) {
    let shown = cwd
        .map(|cwd| cwd.trim_end_matches('/'))
        .filter(|cwd| !cwd.is_empty())
        .and_then(|cwd| path.strip_prefix(cwd)?.strip_prefix('/'))
        .unwrap_or(path);
    match shown.rfind('/') {
        Some(ix) => (shown[..=ix].to_string(), shown[ix + 1..].to_string()),
        None => (String::new(), shown.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_relative_to_the_working_directory() {
        assert_eq!(
            split_title("/repo/crates/a/src/view.rs", Some("/repo")),
            ("crates/a/src/".into(), "view.rs".into())
        );
        assert_eq!(
            split_title("/repo/view.rs", Some("/repo/")),
            (String::new(), "view.rs".into())
        );
        // 不在工作目录下（或只是前缀相同的兄弟目录）写原路径。
        assert_eq!(
            split_title("/repository/x.rs", Some("/repo")),
            ("/repository/".into(), "x.rs".into())
        );
        assert_eq!(split_title("x.rs", None), (String::new(), "x.rs".into()));
    }

    #[test]
    fn each_edit_becomes_its_own_numbered_block() {
        let file = EditFile {
            path: "/repo/src/lib.rs".into(),
            label: "lib.rs".into(),
            added: 2,
            removed: 1,
            edits: vec![
                Edit {
                    old: Some("a\nb\nc\n".into()),
                    new: "a\nB\nc\n".into(),
                },
                Edit {
                    old: None,
                    new: "new\n".into(),
                },
                // 前后一样：没有可看的，跳过。
                Edit {
                    old: Some("same\n".into()),
                    new: "same\n".into(),
                },
            ],
        };
        let files = diff_files(&file);
        let summary: Vec<_> = files
            .iter()
            .map(|f| (edit_ordinal(f.path()), f.additions(), f.deletions()))
            .collect();
        assert_eq!(summary, vec![(Some(1), 1, 1), (Some(2), 1, 0)]);
        assert_eq!(files[0].language().as_ref(), "rust");
    }

    #[test]
    fn change_counts_leave_out_the_zero_side() {
        assert_eq!(change_counts(4, 1), "+4 −1");
        assert_eq!(change_counts(0, 1), "−1");
        assert_eq!(change_counts(3, 0), "+3");
    }
}
