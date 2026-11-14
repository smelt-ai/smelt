//! 文件视图的界面：内容在左、树在右，路径栏横跨整个视图。只读内存，不做 IO。

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_virtual_list,
};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::client::protocol::files::is_previewable_image;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::{Button, IconAction as _};

use crate::model::{
    FileTreeRow, OpenFile, PreviewKind, SearchState, SearchTreeDir, breadcrumb_segments,
    build_search_tree, file_tree_row_height, file_tree_row_id, name_likely_truncated,
};
use crate::{FilesPanel, MAX_TREE_WIDTH, MIN_TREE_WIDTH, TREE_KEY_CONTEXT, ui_theme};

fn placeholder_view(text: &str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_center()
                .text_color(muted)
                .child(text.to_string()),
        )
}

fn git_mark_color(mark: char) -> Hsla {
    match mark {
        'D' => rgb(ui_theme::red()).into(),
        'A' => rgb(ui_theme::green()).into(),
        'U' => rgb(ui_theme::blue()).into(),
        _ => rgb(ui_theme::yellow()).into(),
    }
}

impl FilesPanel {
    pub(crate) fn render_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_query = !self.filter.read(cx).value().trim().is_empty();
        let tree_w = self.tree_w.clamp(MIN_TREE_WIDTH, MAX_TREE_WIDTH);
        let tree = if has_query {
            match &self.search {
                Some(state) => self.search_results_view(state, cx),
                None => div().flex_1().into_any_element(),
            }
        } else {
            self.file_tree(tree_w, window, cx)
        };
        let local = self.tree_runtime() == RuntimeKey::Local;
        let search_box = h_flex()
            .gap_1()
            .px_2()
            .py(px(6.))
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(div().flex_1().min_w_0().child(Input::new(&self.filter).small()))
            // 只能钉本机目录：远程项目的树里不混进本机目录。
            .when(local, |el| {
                el.child(
                    Button::new("files-pin-root")
                        .quiet()
                        .compact()
                        .flex_shrink_0()
                        .icon(IconName::Plus)
                        .icon_action("把目录加到文件树")
                        .on_click(cx.listener(|this, _, _, cx| this.pick_pinned_roots(cx))),
                )
            });
        let tree = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(search_box)
            .child(tree)
            .into_any_element();
        let (header, content) = self.file_content_parts(cx);
        let resize_listener = self.tree_open.then(|| self.tree_resize_listener(cx));
        let tree_side = self.tree_open.then(|| {
            div()
                .h_full()
                .flex()
                .flex_none()
                .child(self.tree_resize_handle(cx))
                .child(
                    div()
                        .w(px(tree_w))
                        .flex_none()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .flex()
                        .bg(rgb(ui_theme::bg_stage()))
                        .child(div().size_full().min_h_0().flex().flex_col().child(tree)),
                )
        });
        let body = div()
            .flex_1()
            .relative()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .children(resize_listener)
            .child(
                div()
                    .size_full()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .flex()
                            .child(content),
                    )
                    .children(tree_side),
            );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(header)
            .child(body)
            .into_any_element()
    }

    /// 右上角收起 / 展开树。
    fn toggle_tree(&mut self, cx: &mut Context<Self>) {
        self.tree_open = !self.tree_open;
        self.persist_tree_layout();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    // ===================== 搜索结果 =====================

    /// 保留命中文件的目录层级；点文件跳到内容命中的那一行。
    fn search_results_view(&self, state: &SearchState, cx: &mut Context<Self>) -> AnyElement {
        let (muted, fg, hover) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.foreground, theme.accent)
        };
        let status = if !state.done {
            "搜索中…".to_string()
        } else if state.hits.is_empty() {
            "无匹配".to_string()
        } else if state.truncated {
            format!("命中 {}+ 项（已截断）", state.hits.len())
        } else {
            format!("命中 {} 项", state.hits.len())
        };
        let tree = build_search_tree(&state.hits);
        let context = SearchTreeContext {
            state,
            view: cx.entity(),
            muted,
            fg,
            hover,
        };
        let rows = search_tree_rows(&tree, 0, "", &context);
        div()
            .id("search-results")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .child(status),
            )
            .child(
                div()
                    .id("search-results-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .pb_1()
                    .track_scroll(&self.search_scroll)
                    .vertical_scrollbar(&self.search_scroll)
                    .children(rows),
            )
            .into_any_element()
    }

    // ===================== 目录树 =====================

    /// 单根时根隐式展开、不画根标题行；多根时每个根一行可收起的标题，子项缩进一级。
    fn file_tree(&self, panel_w: f32, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = RowColors {
            muted: theme.muted_foreground,
            fg: theme.foreground,
            hover: theme.accent,
            active_bg: theme.border,
            accent: theme.primary,
        };
        let roots = self.tree_roots();
        if roots.is_empty() {
            return placeholder_view("无项目目录", colors.muted).into_any_element();
        }
        let runtime = self.tree_runtime();
        let cached = |root: &str| {
            self.dir_cache
                .get(&runtime)
                .is_some_and(|cache| cache.contains_key(root))
        };
        if roots.len() == 1 && !cached(&roots[0]) {
            return placeholder_view("加载中…", colors.muted).into_any_element();
        }
        let rows = Rc::new(self.tree_rows(cx));
        let row_h = px(file_tree_row_height(f32::from(window.rem_size())));
        // 行宽用列宽，免得虚拟列表按内容量出超宽的行。
        let row_w = px(panel_w.max(1.0));
        let sizes = Rc::new(vec![size(row_w, row_h); rows.len()]);
        let view = cx.entity();
        let paint = Rc::new(RowPaint {
            view: view.clone(),
            focus: self.tree_focus.clone(),
            open_path: self.open_file.as_ref().map(|of| of.path.clone()),
            selected: self.selected.clone(),
            local: runtime == RuntimeKey::Local,
            panel_w,
            row_w,
            row_h,
            colors,
        });
        let list = v_virtual_list(view, "file-tree", sizes, move |_, range, _, _| {
            range
                .map(|ix| render_tree_row(&rows[ix], &paint))
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.tree_scroll)
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_x_hidden();

        // 滚动条是 absolute inset_0，父级必须 relative，不然它对齐到外面的定位祖先。
        // 方向键挂在树自己的焦点上：点进树里才归树。
        let focus = self.tree_focus.clone();
        div()
            .id("file-tree-host")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .key_context(TREE_KEY_CONTEXT)
            .track_focus(&self.tree_focus)
            .on_action(cx.listener(Self::on_tree_up))
            .on_action(cx.listener(Self::on_tree_down))
            .on_action(cx.listener(Self::on_tree_left))
            .on_action(cx.listener(Self::on_tree_right))
            .on_action(cx.listener(Self::on_tree_activate))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx);
            })
            .child(list)
            .vertical_scrollbar(&self.tree_scroll)
            .into_any_element()
    }

    // ===================== 文件内容 =====================

    /// 路径栏（有打开的文件时）和内容体。
    fn file_content_parts(&self, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let muted = cx.theme().muted_foreground;
        match &self.open_file {
            None => (None, placeholder_view("文件", muted).into_any_element()),
            Some(of) => (Some(self.file_header(of, cx)), self.file_body(of, cx)),
        }
    }

    fn file_header(&self, of: &OpenFile, cx: &mut Context<Self>) -> AnyElement {
        let (muted, fg, border, warning) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.foreground,
                theme.border,
                theme.warning,
            )
        };
        // 面包屑：项目名 > 中间目录 > 文件名。光看文件名分不清是哪个项目、哪层目录下的，
        // 多根时尤其如此。只有树那台机器上的文件对得上根。
        let roots = if of.runtime == self.tree_runtime() {
            self.tree_roots()
        } else {
            Vec::new()
        };
        let segs = breadcrumb_segments(self.path_style(&of.runtime, cx), &of.path, &roots);
        let is_image = is_previewable_image(&of.path);
        let dirty = !is_image && of.dirty(cx);
        let preview_kind = PreviewKind::for_path(&of.path);
        let preview = of.preview && preview_kind.is_some();
        let last = segs.len().saturating_sub(1);
        let on_tree_machine = of.runtime == self.tree_runtime();
        let view = cx.entity();
        let breadcrumb = h_flex()
            .id("file-breadcrumb")
            .items_center()
            .gap_1()
            .min_w_0()
            .children(segs.iter().enumerate().flat_map(|(ix, seg)| {
                let is_last = ix == last;
                let path = seg.path.clone();
                let view = view.clone();
                let seg_el = div()
                    .id((ElementId::from("file-breadcrumb-seg"), path.clone()))
                    .flex_shrink_0()
                    .px(px(8.))
                    .py(px(5.))
                    .min_h(px(28.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .text_size(px(14.))
                    .line_height(relative(1.2))
                    .when(is_last, |el| el.text_color(fg).font_semibold())
                    .when(!is_last, |el| el.text_color(muted))
                    .hover(move |el| el.bg(border).text_color(fg))
                    .child(seg.label.clone())
                    // 别的机器上的文件在树里定位不到。
                    .when(on_tree_machine, |el| {
                        el.on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.reveal_from_breadcrumb(path.clone(), !is_last, cx);
                            });
                        })
                    });
                let sep = (!is_last).then(|| {
                    div()
                        .flex_shrink_0()
                        .text_size(px(14.))
                        .text_color(muted)
                        .child(">")
                });
                std::iter::once(seg_el.into_any_element())
                    .chain(sep.map(IntoElement::into_any_element))
            }));
        let tree_open = self.tree_open;
        h_flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .child(breadcrumb)
                    // 没保存的改动：文件名后一个小圆点。
                    .when(dirty, |el| {
                        el.child(div().size(px(6.)).rounded_full().bg(warning))
                    })
                    .children(
                        of.save_error
                            .clone()
                            .map(|message| div().text_xs().text_color(warning).child(message)),
                    ),
            )
            // 浏览器只能打开这台电脑上的文件。
            .when(
                preview_kind == Some(PreviewKind::Html) && of.runtime == RuntimeKey::Local,
                |el| {
                    el.child(
                        Button::new("file-open-in-browser")
                            .quiet()
                            .icon_size()
                            .flex_shrink_0()
                            .label("浏览器打开")
                            .on_click(cx.listener(|this, _, _, cx| this.open_file_in_browser(cx))),
                    )
                },
            )
            .when(preview_kind.is_some(), |el| {
                el.child(
                    Button::new("file-preview-toggle")
                        .quiet()
                        .icon_size()
                        .flex_shrink_0()
                        .label(if preview { "查看原始码" } else { "查看预览" })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_file_preview(!preview, window, cx)
                        })),
                )
            })
            .child(
                Button::new("file-tree-toggle")
                    .quiet()
                    .compact()
                    .flex_shrink_0()
                    .icon(if tree_open {
                        IconName::PanelRight
                    } else {
                        IconName::PanelLeft
                    })
                    .icon_action(if tree_open {
                        "收起文件树"
                    } else {
                        "展开文件树"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_tree(cx))),
            )
            .into_any_element()
    }

    fn file_body(&self, of: &OpenFile, cx: &mut Context<Self>) -> AnyElement {
        let fg = cx.theme().foreground;
        let preview = of.preview && PreviewKind::for_path(&of.path).is_some();
        // 图片过大、读不出来时没有字节，落到编辑器分支显示原因。
        if let Some(image) = of.image.as_ref() {
            // 栅格图只画降采样预览，没好之前是空盒子。
            smelt_plugin_api::ui::image::ensure_preview(image, cx);
            return div()
                .id("image-file-preview")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(ui_theme::bg_stage()))
                .child(smelt_plugin_api::ui::image::image_surface(image))
                .into_any_element();
        }
        if let (true, Some(html)) = (preview, of.html_preview.clone()) {
            // 原生网页视图盖得住宿主的遮罩层：开着时不画它，它会自己藏起来（见
            // html_preview）。本插件自己的对话框它自己看。
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .when(!self.overlay_open, |el| el.child(html))
                .into_any_element();
        }
        if preview {
            return div()
                .id("md-preview")
                .flex_1()
                .min_h_0()
                .overflow_x_scroll()
                .overflow_y_scroll()
                .p_3()
                .child(div().text_sm().text_color(fg).child(
                    smelt_plugin_api::ui::markdown_mermaid::markdown_view(
                        "md-preview-body",
                        of.preview_source.clone(),
                    ),
                ))
                .into_any_element();
        }
        div()
            .flex_1()
            .min_h_0()
            .child(
                // 菜单在编辑器自己的右键回调里建，那时编辑器正在 update：这里不能读它，
                // 不然重入借用直接崩。剪切、复制、发送执行时各自判空。
                gpui_component::input::Editor::new(&of.editor)
                    .h_full()
                    // 编辑器自带行号栏，去掉 Input 预设的横向内边距。
                    .px_0()
                    .context_menu(move |menu, _, cx| {
                        let has_paste = cx.read_from_clipboard().is_some();
                        menu.menu("剪切", Box::new(gpui_component::input::Cut))
                            .menu("复制", Box::new(gpui_component::input::Copy))
                            .menu_with_disabled(
                                "粘贴",
                                !has_paste,
                                Box::new(gpui_component::input::Paste),
                            )
                            .separator()
                            .menu("全选", Box::new(gpui_component::input::SelectAll))
                            .separator()
                            .menu(
                                "发送选中内容到会话",
                                Box::new(smelt_plugin_api::RunCommand::new(format!(
                                    "{}/send-selection",
                                    crate::COMMAND_OWNER
                                ))),
                            )
                    }),
            )
            .into_any_element()
    }

    // ===================== 分隔条 =====================

    /// 内容和树之间的分隔条。拖它才改树的宽度，拖外面的面板只让内容伸缩。
    fn tree_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        let id = "files-tree-split";
        div()
            .id(id)
            .relative()
            .w(px(6.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .group(id)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(3.))
                    .w(px(1.))
                    .h_full()
                    .bg(rgb(ui_theme::border_dim()))
                    .group_hover(id, |line| line.bg(rgb(ui_theme::text_faint()))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.tree_drag_start = Some((
                        f32::from(event.position.x),
                        this.tree_w.clamp(MIN_TREE_WIDTH, MAX_TREE_WIDTH),
                    ));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 窗口级监听：拖动时指针越过 6px 的分隔条也接着生效。松手时记下宽度。
    fn tree_resize_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.bubble() || event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    move_view.update(cx, |this, cx| {
                        let Some((start_x, start_w)) = this.tree_drag_start else {
                            return;
                        };
                        let width = (start_w + start_x - f32::from(event.position.x))
                            .clamp(MIN_TREE_WIDTH, MAX_TREE_WIDTH);
                        if (this.tree_w - width).abs() > 0.5 {
                            this.tree_w = width;
                            cx.notify();
                        }
                    });
                });
                let up_view = view;
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    up_view.update(cx, |this, cx| {
                        if this.tree_drag_start.take().is_some() {
                            this.persist_tree_layout();
                            cx.notify();
                        }
                    });
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

// ===================== 搜索结果的行 =====================

struct SearchTreeContext<'a> {
    state: &'a SearchState,
    view: Entity<FilesPanel>,
    muted: Hsla,
    fg: Hsla,
    hover: Hsla,
}

/// 只有一个子目录、没有文件的目录压成一行（如 `smelt-core / src`），不为单一路径浪费
/// 一串空行。
fn search_tree_rows(
    dir: &SearchTreeDir,
    depth: usize,
    prefix: &str,
    context: &SearchTreeContext<'_>,
) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (name, child) in &dir.dirs {
        let mut rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let mut label = name.clone();
        let mut visible = child;
        while visible.files.is_empty() && visible.dirs.len() == 1 {
            let Some((next_name, next)) = visible.dirs.iter().next() else {
                break;
            };
            label.push_str(" / ");
            label.push_str(next_name);
            rel.push('/');
            rel.push_str(next_name);
            visible = next;
        }
        rows.push(
            div()
                .id((ElementId::from("search-dir"), rel.clone()))
                .flex()
                .items_center()
                .gap_1()
                .pl(px(8. + depth as f32 * 16.))
                .pr_2()
                .py(px(2.))
                .text_sm()
                .text_color(context.fg)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(14.))
                        .text_color(context.muted),
                )
                .child(
                    Icon::new(IconName::FolderOpen)
                        .size(px(16.))
                        .text_color(context.fg),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .child(truncated_line(label)),
                )
                .into_any_element(),
        );
        rows.extend(search_tree_rows(visible, depth + 1, &rel, context));
    }
    for &ix in &dir.files {
        let hit = &context.state.hits[ix];
        let name = hit
            .rel
            .rsplit('/')
            .next()
            .unwrap_or(hit.rel.as_str())
            .to_string();
        let path = hit.path.clone();
        let line = hit.line.as_ref().map(|(line, _)| *line);
        let view = context.view.clone();
        let hover = context.hover;
        rows.push(
            div()
                .id((ElementId::from("search-file"), hit.rel.clone()))
                .flex()
                .items_center()
                .gap_1()
                .pl(px(8. + depth as f32 * 16. + 14.))
                .pr_2()
                .py(px(2.))
                .text_sm()
                .text_color(context.fg)
                .hover(move |row| row.bg(hover))
                .child(
                    Icon::new(IconName::File)
                        .size(px(16.))
                        .text_color(context.muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .child(truncated_line(name)),
                )
                .on_click(move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.view_file_at(path.clone(), line, window, cx);
                    });
                })
                .into_any_element(),
        );
    }
    rows
}

// ===================== 树的行 =====================

#[derive(Clone, Copy)]
struct RowColors {
    muted: Hsla,
    fg: Hsla,
    hover: Hsla,
    active_bg: Hsla,
    accent: Hsla,
}

/// 画每一行都一样的东西，虚拟列表每次取可见范围时共用。
struct RowPaint {
    view: Entity<FilesPanel>,
    focus: FocusHandle,
    open_path: Option<String>,
    selected: Option<String>,
    /// 路径在这台电脑上。「在 Finder 中显示」只对它有意义。
    local: bool,
    panel_w: f32,
    row_w: Pixels,
    row_h: Pixels,
    colors: RowColors,
}

fn row_element_id(row: &FileTreeRow) -> (ElementId, String) {
    let (kind, key) = file_tree_row_id(row);
    (ElementId::from(kind), key.to_string())
}

fn chevron(open: bool, muted: Hsla) -> Div {
    div().w(px(14.)).flex().justify_center().child(
        Icon::new(if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .size(px(12.))
        .text_color(muted),
    )
}

fn render_tree_row(row: &FileTreeRow, paint: &RowPaint) -> AnyElement {
    let RowColors {
        muted,
        fg,
        hover,
        active_bg,
        accent,
    } = paint.colors;
    let focus = paint.focus.clone();
    let base = div()
        .id(row_element_id(row))
        .w(paint.row_w)
        .h(paint.row_h)
        .flex()
        .items_center()
        .text_sm()
        // 点行也把焦点交给树，方向键接着用。
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.focus(&focus, cx);
        });
    let is_selected = |path: &str| paint.selected.as_deref() == Some(path);
    match row {
        FileTreeRow::Loading { depth, .. } => base
            .pl(px(8.0 + *depth as f32 * 14.0))
            .pr_2()
            .text_color(muted)
            .child("加载中…")
            .into_any_element(),
        FileTreeRow::RootHeader { path, open, pinned } => {
            let name = path
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(path)
                .to_string();
            let toggle_view = paint.view.clone();
            let menu_view = paint.view.clone();
            let root = path.clone();
            let menu_root = path.clone();
            let header = base
                .gap_1()
                .px_2()
                .font_semibold()
                .text_color(fg)
                .when(is_selected(path), |el| el.border_l_2().border_color(accent))
                .hover(move |el| el.bg(hover))
                .on_click(move |_, _, cx| {
                    toggle_view.update(cx, |this, cx| {
                        this.selected = Some(root.clone());
                        this.toggle_root_collapsed(root.clone(), cx);
                    });
                })
                .child(chevron(*open, muted))
                .child(Icon::new(IconName::FolderOpen).size(px(14.)).text_color(fg))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(truncated_line(name)),
                );
            // 当前项目的根移不掉，只有钉进来的根有菜单。
            if !*pinned {
                return header.into_any_element();
            }
            header
                .context_menu(move |menu, _, _| {
                    let view = menu_view.clone();
                    let root = menu_root.clone();
                    menu.item(
                        PopupMenuItem::new("从文件树移除").on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.unpin_root(&root, cx));
                        }),
                    )
                })
                .into_any_element()
        }
        FileTreeRow::Entry {
            depth,
            name,
            is_dir,
            path,
            expanded,
            git_mark,
        } => {
            let indent = px(8.0 + *depth as f32 * 14.0);
            let arrow = if *is_dir {
                chevron(*expanded, muted)
            } else {
                div().w(px(14.))
            };
            let type_icon = Icon::new(match (*is_dir, *expanded) {
                (true, true) => IconName::FolderOpen,
                (true, false) => IconName::Folder,
                (false, _) => IconName::File,
            })
            .size(px(14.))
            .text_color(if *is_dir { fg } else { muted });
            let is_open = !*is_dir && paint.open_path.as_deref() == Some(path.as_str());
            let show_name_tip = name_likely_truncated(name, *depth, paint.panel_w);
            let name_label: SharedString = name.clone().into();
            let click_view = paint.view.clone();
            let click_path = path.clone();
            let menu_view = paint.view.clone();
            let menu_path = path.clone();
            let is_dir = *is_dir;
            let local = paint.local;
            base.gap_1()
                .pl(indent)
                .pr_2()
                .overflow_hidden()
                .text_color(if is_dir { fg } else { muted })
                .when(is_open, |el| el.bg(active_bg))
                .when(is_selected(path), |el| {
                    el.border_l_2().border_color(accent).pl(indent - px(2.0))
                })
                .hover(move |el| el.bg(hover))
                .on_click(move |_, window, cx| {
                    click_view.update(cx, |this, cx| {
                        this.selected = Some(click_path.clone());
                        if is_dir {
                            this.toggle_expand(click_path.clone(), cx);
                        } else {
                            this.view_file(click_path.clone(), window, cx);
                        }
                    });
                })
                .context_menu(move |menu, _, _| {
                    let send = (menu_view.clone(), menu_path.clone());
                    let copy = menu_path.clone();
                    let finder = menu_path.clone();
                    let delete = (menu_view.clone(), menu_path.clone());
                    menu.item(PopupMenuItem::new("发送到会话").on_click(move |_, _, cx| {
                        send.0
                            .update(cx, |this, cx| this.send_path_to_session(&send.1, cx));
                    }))
                    .item(
                        PopupMenuItem::new("复制文件路径").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        }),
                    )
                    .when(local, |menu| {
                        menu.item(
                            PopupMenuItem::new(if cfg!(target_os = "macos") {
                                "在 Finder 中显示"
                            } else {
                                "在文件管理器中显示"
                            })
                            .on_click(move |_, _, _| FilesPanel::reveal_path_in_finder(&finder)),
                        )
                    })
                    .item(
                        PopupMenuItem::new(if is_dir {
                            "删除文件夹"
                        } else {
                            "删除文件"
                        })
                        .on_click(move |_, window, cx| {
                            delete.0.update(cx, |this, cx| {
                                this.start_delete(delete.1.clone(), is_dir, window, cx)
                            });
                        }),
                    )
                })
                .child(arrow)
                .child(type_icon)
                .child(
                    div()
                        .id("file-name")
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(truncated_line(name_label.clone()))
                        .when(show_name_tip, |el| {
                            el.tooltip(move |window, cx| {
                                Tooltip::new(name_label.clone()).build(window, cx)
                            })
                        }),
                )
                .children(git_mark.map(|mark| {
                    div()
                        .flex_none()
                        .text_xs()
                        .font_bold()
                        .text_color(git_mark_color(mark))
                        .child(mark.to_string())
                }))
                .into_any_element()
        }
    }
}
