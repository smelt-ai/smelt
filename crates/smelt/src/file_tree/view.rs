//! 文件树页面：树/搜索结果/内容区，以及删除确认弹窗。
//!
//! 只组 UI。写缓存、打开文件、删盘的方法留在 `workspace.rs`。

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use gpui_component::*;
use std::path::Path;

use crate::git_panel::GitStatusData;
use crate::{SendSelectionToTerminal, Workspace, placeholder_view};

use super::*;

/// 把只有一个子目录且没有文件的路径压成一行，例如 `smelt-core / src`。这和参考
/// 文件筛选器一致：保留层级语义，但不会为了单一路径浪费一串空行。
struct SearchTreeContext<'a> {
    hits: &'a [SearchHit],
    view: Entity<Workspace>,
    muted: Hsla,
    fg: Hsla,
    hover: Hsla,
}

fn search_tree_rows(
    dir: &SearchTreeDir,
    depth: usize,
    row_id: &mut usize,
    context: &SearchTreeContext<'_>,
) -> Vec<AnyElement> {
    let mut rows = Vec::new();

    for (name, child) in &dir.dirs {
        let mut label = name.clone();
        let mut visible_child = child;
        while visible_child.files.is_empty() && visible_child.dirs.len() == 1 {
            let (next_name, next_child) = visible_child
                .dirs
                .iter()
                .next()
                .expect("single child directory must exist");
            label.push_str(" / ");
            label.push_str(next_name);
            visible_child = next_child;
        }

        let id = *row_id;
        *row_id += 1;
        rows.push(
            div()
                .id(("search-dir", id))
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
                .child(div().min_w_0().truncate().child(label))
                .into_any_element(),
        );
        rows.extend(search_tree_rows(visible_child, depth + 1, row_id, context));
    }

    for &ix in &dir.files {
        let hit = &context.hits[ix];
        let name = hit
            .rel
            .rsplit('/')
            .next()
            .unwrap_or(hit.rel.as_str())
            .to_string();
        let path = hit.path.clone();
        let line = hit.line.as_ref().map(|(line, _)| *line);
        let id = *row_id;
        *row_id += 1;
        let open_view = context.view.clone();
        rows.push(
            div()
                .id(("search-file", id))
                .flex()
                .items_center()
                .gap_1()
                .pl(px(8. + depth as f32 * 16. + 14.))
                .pr_2()
                .py(px(2.))
                .text_sm()
                .text_color(context.fg)
                .cursor_pointer()
                .hover(move |row| row.bg(context.hover))
                .child(
                    Icon::new(IconName::File)
                        .size(px(16.))
                        .text_color(context.muted),
                )
                .child(div().min_w_0().truncate().child(name))
                .on_click(move |_event, window, cx| {
                    open_view.update(cx, |workspace, cx| {
                        workspace.view_file_at(path.clone(), line, window, cx);
                    });
                })
                .into_any_element(),
        );
    }
    rows
}

/// 文件树搜索结果视图：保留命中文件的目录层级，而不是把相对路径拼成扁平文本。
/// 点击文件仍会跳到内容命中行。
pub fn search_results_view(
    state: &SearchState,
    scroll: &ScrollHandle,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (muted, fg, hover) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.accent)
    };
    // 顶栏状态：搜索中 / 无结果 / N 项命中(是否截断)。
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
    let mut row_id = 0;
    let context = SearchTreeContext {
        hits: &state.hits,
        view: cx.entity(),
        muted,
        fg,
        hover,
    };
    let rows = search_tree_rows(&tree, 0, &mut row_id, &context);

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
                .track_scroll(scroll)
                .vertical_scrollbar(scroll)
                .children(rows),
        )
        .into_any_element()
}

// ===================== 目录树 =====================

/// 估算文件名在文件树列里是否会被 `.truncate()` 裁切，用来决定要不要挂 hover tooltip。
fn name_likely_truncated(name: &str, depth: usize, panel_w: f32) -> bool {
    // 左内边距 8 + 每层缩进 14 + 箭头 14 + 图标 14 + gap + 右内边距 8
    let chrome = 56.0 + depth as f32 * 14.0;
    let text_w = (panel_w - chrome).max(0.0);
    // text_sm 约 7–8px/字符；略保守，避免短文件名也弹 tooltip。
    let max_chars = (text_w / 7.5).floor() as usize;
    name.chars().count() > max_chars
}

/// 只读缓存的递归收集目录条目（仅进入已展开且已缓存的文件夹）；绝不做任何 fs 调用。
/// 展开了但尚未缓存的目录会被跳过——render 每帧检查并后台补齐，下一帧自动出现。
pub(super) fn walk_dir_cached(
    dir: &str,
    dir_cache: &DirCache,
    expanded: &HashSet<String>,
    depth: usize,
    out: &mut Vec<(usize, String, bool, String, bool)>,
) {
    let Some((_, entries)) = dir_cache.get(dir) else {
        return;
    };
    for (name, is_dir) in entries.iter() {
        let path = Path::new(dir).join(name).to_string_lossy().to_string();
        let is_expanded = expanded.contains(&path);
        out.push((depth, name.clone(), *is_dir, path.clone(), is_expanded));
        if *is_dir && is_expanded {
            walk_dir_cached(&path, dir_cache, expanded, depth + 1, out);
        }
    }
}

/// 文件树视图：只读目录列表缓存渲染（ensure_dir_listing 后台刷新，绝不在这里碰
/// 文件系统），已展开的文件夹递归显示，点击文件夹展开/收起、点击文件打开。
///
/// 未用 uniform_list 虚拟滚动：实测它对这里的行内容（含 Icon）的孤立测量会算出
/// 异常偏大的行高，导致可视区间被判定只能塞下 1 行——已用隔离实验定位到具体是
/// uniform_list 的度量逻辑而非容器高度链的问题。文件树条目量级远小于 git diff，
/// 虚拟滚动只是锦上添花而非必需，故改走普通可滚动列表（与 git-files 同款写法），
/// 优先保证正确显示；虚拟滚动作为后续可选优化记在 docs/roadmap.md。
/// porcelain 两位状态码 → 简标 + 颜色（M 改 / A 增 / D 删 / ? 未跟踪）。
fn git_status_badge(code: &str) -> Option<(char, gpui::Hsla)> {
    // 取 index + worktree 两位里「更严重」的那个：D > A/? > M > 其它
    let chars: String = code.chars().take(2).collect();
    if chars.contains('D') {
        Some(('D', gpui::rgb(crate::ui_theme::red()).into()))
    } else if chars.contains('A') {
        Some(('A', gpui::rgb(crate::ui_theme::green()).into()))
    } else if chars.contains('?') {
        Some(('U', gpui::rgb(crate::ui_theme::blue()).into())) // untracked
    } else if chars.chars().any(|c| c != ' ' && c != '?') {
        Some(('M', gpui::rgb(crate::ui_theme::yellow()).into()))
    } else {
        None
    }
}

/// 文件树：单根时行为跟以前一致（根隐式展开、不显示根标题行）；工作区挂了 ≥2 个
/// 项目目录时，每个根渲染成一个可折叠的顶层标题行、其下子项缩进一级——对齐 VSCode
/// 的 multi-root workspace，右侧文件不用再切项目来回换。根集合由调用方（main.rs）按
/// 项目分组聚合，每个根各查各自的 git status 标 M/A/D，互不串味。
pub(crate) struct FileTreeParams<'a> {
    pub roots: &'a [String],
    pub expanded: &'a HashSet<String>,
    /// 用户主动折叠起来的根目录（默认全展开，只有落在这个集合里的才收起）。
    pub collapsed_roots: &'a HashSet<String>,
    pub dir_cache: &'a DirCache,
    pub scroll: &'a ScrollHandle,
    pub open_path: Option<&'a str>,
    /// 键盘选中的条目路径（高亮边框，区别于「当前打开文件」的底色）。
    pub selected_path: Option<&'a str>,
    pub panel_w: f32,
    /// 各根的 git status（root 绝对路径 → 状态数据）。
    pub git_status: &'a HashMap<String, (Instant, GitStatusData)>,
    /// 文件树自己的键盘焦点。只有点进树后，工作区才把方向键交给文件树。
    pub focus_handle: &'a FocusHandle,
}

pub fn file_tree(params: FileTreeParams<'_>, cx: &mut Context<Workspace>) -> AnyElement {
    let FileTreeParams {
        roots,
        expanded,
        collapsed_roots,
        dir_cache,
        scroll,
        open_path,
        selected_path,
        panel_w,
        git_status,
        focus_handle,
    } = params;
    let (muted, fg, hover, active_bg, accent) = {
        let t = cx.theme();
        (
            t.muted_foreground,
            t.foreground,
            t.accent,
            t.border,
            t.primary,
        )
    };
    if roots.is_empty() {
        return placeholder_view("无项目目录", muted).into_any_element();
    }
    let multi = roots.len() > 1;
    // 单根且尚未缓存：保持老行为，整块居中「加载中…」。
    if !multi && !dir_cache.contains_key(&roots[0]) {
        return placeholder_view("加载中…", muted).into_any_element();
    }

    let this = cx.entity();
    let focus_handle = focus_handle.clone();

    // 渲染一个文件/目录行。抽成闭包：多根时要在每个根的循环里各调一遍，且各传各的
    // `row_root`（strip_prefix 基准）和 `changed`（该根的改动列表）。`i` 是全局唯一
    // 行号，保证 ElementId 不撞。
    let render_entry = |i: usize,
                        depth: usize,
                        name: String,
                        is_dir: bool,
                        path: String,
                        is_expanded: bool,
                        row_root: &str,
                        changed: Option<&[(String, String)]>|
     -> AnyElement {
        let indent = px(8.0 + depth as f32 * 14.0);
        // 展开箭头：目录用 chevron（展开朝下 / 收起朝右），文件留等宽占位对齐。
        let arrow = if is_dir {
            div()
                .w(px(14.))
                .flex()
                .justify_center()
                .child(
                    Icon::new(if is_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(12.))
                    .text_color(muted),
                )
                .into_any_element()
        } else {
            div().w(px(14.)).into_any_element()
        };
        // 类型图标：目录（展开 / 收起用不同文件夹图标）与文件区分。
        let type_icon = Icon::new(if is_dir {
            if is_expanded {
                IconName::FolderOpen
            } else {
                IconName::Folder
            }
        } else {
            IconName::File
        })
        .size(px(14.))
        .text_color(if is_dir { fg } else { muted });
        let this = this.clone();
        let p = path.clone();
        let this_menu = this.clone();
        let p_menu = p.clone();
        // 当前在右侧内容面板打开的文件：文件树里对应行常驻高亮，不用靠记忆去找。
        let is_open = !is_dir && open_path == Some(path.as_str());
        // git 状态：M/A/D/U 字母色标（只标文件，不往目录冒泡）。
        let git_badge = if !is_dir {
            changed.and_then(|files| {
                Path::new(&path)
                    .strip_prefix(row_root)
                    .ok()
                    .and_then(|rel| rel.to_str())
                    .and_then(|rel| {
                        files
                            .iter()
                            .find(|(_, p)| p == rel)
                            .and_then(|(code, _)| git_status_badge(code))
                    })
            })
        } else {
            None
        };
        let is_selected = selected_path == Some(path.as_str());
        let name_tip: SharedString = name.clone().into();
        let show_name_tip = name_likely_truncated(&name, depth, panel_w);
        div()
            .id(("file", i))
            .flex()
            .items_center()
            .gap_1()
            .pl(indent)
            .pr_2()
            .py(px(1.0))
            .text_sm()
            .text_color(if is_dir { fg } else { muted })
            .when(is_open, |el| el.bg(active_bg))
            .when(is_selected, |el| {
                el.border_l_2().border_color(accent).pl(indent - px(2.0))
            })
            .hover(move |s| s.bg(hover))
            .on_click(move |_ev, window, cx| {
                this.update(cx, |ws, cx| {
                    ws.file_tree_selected = Some(p.clone());
                    if is_dir {
                        ws.toggle_expand(p.clone(), cx);
                    } else {
                        ws.view_file(p.clone(), window, cx);
                    }
                });
            })
            .context_menu(move |menu, _window, _cx| {
                let this_term = this_menu.clone();
                let p_term = p_menu.clone();
                let this_copy = this_menu.clone();
                let p_copy = p_menu.clone();
                let this_finder = this_menu.clone();
                let p_finder = p_menu.clone();
                let this_del = this_menu.clone();
                let p_del = p_menu.clone();
                menu.item(
                    PopupMenuItem::new("发送到终端").on_click(move |_ev, _window, cx| {
                        this_term.update(cx, |ws, cx| ws.send_path_to_terminal(p_term.clone(), cx));
                    }),
                )
                .item(
                    PopupMenuItem::new("复制文件路径").on_click(move |_ev, _window, cx| {
                        this_copy.update(cx, |ws, cx| {
                            ws.copy_file_path_to_clipboard(p_copy.clone(), cx)
                        });
                    }),
                )
                .item(
                    PopupMenuItem::new("在 Finder 中显示").on_click(move |_ev, _window, cx| {
                        this_finder.update(cx, |ws, cx| {
                            ws.reveal_path_in_finder(p_finder.clone(), cx);
                        });
                    }),
                )
                .item(
                    PopupMenuItem::new("删除文件").on_click(move |_ev, _window, cx| {
                        this_del
                            .update(cx, |ws, cx| ws.start_delete_file(p_del.clone(), is_dir, cx));
                    }),
                )
            })
            .child(arrow)
            .child(type_icon)
            // tooltip 只挂在文件名格子上，且仅当可能被截断时才显示——避免像
            // Cargo.toml 这种短名也弹 tooltip，跟右键菜单叠在一起。
            .child(
                div()
                    .id(("file-name", i))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(name)
                    .when(show_name_tip, |el| {
                        el.tooltip(move |window, cx| {
                            Tooltip::new(name_tip.clone()).build(window, cx)
                        })
                    }),
            )
            .children(git_badge.map(|(ch, color)| {
                div()
                    .flex_none()
                    .text_xs()
                    .font_bold()
                    .text_color(color)
                    .child(ch.to_string())
            }))
            .into_any_element()
    };

    // 根标题行（仅多根时渲染）：可折叠，点击切换 collapsed_roots。样式比子项醒目
    // 一档（加粗 + 常驻文件夹图标），一眼分出「这是一个项目根」。
    let render_root_header = |i: usize, root: &str, root_open: bool| -> AnyElement {
        let name = root
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(root)
            .to_string();
        let this = this.clone();
        let rp = root.to_string();
        div()
            .id(("root", i))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py(px(2.0))
            .text_sm()
            .font_semibold()
            .text_color(fg)
            .hover(move |s| s.bg(hover))
            .on_click(move |_ev, _window, cx| {
                this.update(cx, |ws, cx| ws.toggle_root_collapsed(rp.clone(), cx));
            })
            .child(
                div().w(px(14.)).flex().justify_center().child(
                    Icon::new(if root_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(12.))
                    .text_color(muted),
                ),
            )
            .child(Icon::new(IconName::FolderOpen).size(px(14.)).text_color(fg))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .into_any_element()
    };

    let mut rows: Vec<AnyElement> = Vec::new();
    let mut i = 0usize;
    for root in roots {
        let changed = git_status.get(root).map(|(_, d)| d.files.as_slice());
        let root_open = !collapsed_roots.contains(root);
        if multi {
            rows.push(render_root_header(i, root, root_open));
            i += 1;
            if !root_open {
                continue;
            }
        }
        // 根目录尚未缓存：多根时在标题下给一行「加载中…」（单根已在上面早退兜底）。
        if !dir_cache.contains_key(root) {
            rows.push(
                div()
                    .id(("root-loading", i))
                    .pl(px(if multi { 22.0 } else { 8.0 }))
                    .py(px(1.0))
                    .text_sm()
                    .text_color(muted)
                    .child("加载中…")
                    .into_any_element(),
            );
            i += 1;
            continue;
        }
        // 多根时子项缩进一级，给根标题让位；单根从 0 起（跟以前一致）。
        let base_depth = if multi { 1 } else { 0 };
        let mut flat: Vec<(usize, String, bool, String, bool)> = Vec::new();
        walk_dir_cached(root, dir_cache, expanded, base_depth, &mut flat);
        for (depth, name, is_dir, path, is_expanded) in flat {
            rows.push(render_entry(
                i,
                depth,
                name,
                is_dir,
                path,
                is_expanded,
                root,
                changed,
            ));
            i += 1;
        }
    }

    div()
        .id("file-tree")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .py_1()
        .track_focus(&focus_handle)
        .on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
            window.focus(&focus_handle, cx);
        })
        .track_scroll(scroll)
        .vertical_scrollbar(scroll)
        .children(rows)
        .into_any_element()
}

// ===================== 文件内容面板 =====================

/// 面包屑一段：展示名 + 该段对应的绝对路径。
/// 点目录段时用 path 在文件树里定位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BreadcrumbSeg {
    pub label: String,
    pub path: String,
}

/// 按工作区根拆出「项目名 > 中间目录 > 文件名」。
/// 多根时用最长匹配的根；对不上任何根就只显示文件名。
pub(super) fn breadcrumb_segments(path: &str, roots: &[String]) -> Vec<BreadcrumbSeg> {
    let Some(root) = roots
        .iter()
        .filter(|r| path == **r || path.starts_with(&format!("{r}/")))
        .max_by_key(|r| r.len())
    else {
        let name = path.rsplit('/').next().unwrap_or(path);
        return vec![BreadcrumbSeg {
            label: name.to_string(),
            path: path.to_string(),
        }];
    };
    let root_name = root
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(root.as_str());
    let rel = path.strip_prefix(root.as_str()).unwrap_or(path);
    let mut segs = vec![BreadcrumbSeg {
        label: root_name.to_string(),
        path: root.clone(),
    }];
    let mut acc = root.clone();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        acc = format!("{acc}/{part}");
        segs.push(BreadcrumbSeg {
            label: part.to_string(),
            path: acc.clone(),
        });
    }
    segs
}

/// 文件扩展名 → Editor 的语法高亮语言名。gpui-component 的 `Language::from_name`
/// 本身就认常见扩展名（"rs"/"py"/"md" 等），这里只需把扩展名传过去；识别不了的
/// 名字组件会自动回退成纯文本，不会 panic。没有扩展名的文件（Makefile 等）退而
/// 用文件名本身（能命中 "makefile" 这类按文件名匹配的语言）。
pub(super) fn editor_language_for_path(path: &str) -> String {
    let p = Path::new(path);
    match p.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext.to_lowercase(),
        None => p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("text")
            .to_lowercase(),
    }
}

/// 内置文件查看器可以直接预览的图片格式。ICNS 不在 GPUI 的图片解码格式里，
/// 仍按二进制文件处理，避免显示一个加载失败的空白画布。
pub(super) fn is_previewable_image(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "svg")
    )
}

/// 打开的文件内容拆成路径栏和内容体，供 Tool Panel 把路径栏放在文件树分栏上方。
pub struct FileContentParts {
    pub header: Option<AnyElement>,
    pub body: AnyElement,
}

/// 构建文件路径栏与内容体。直接用 gpui-component 的 Editor（InputState code_editor
/// 模式），自带语法高亮、行号、搜索、大文件下的增量编辑，不用再自己管虚拟滚动。
pub fn file_content_parts(
    open_file: &Option<OpenFile>,
    roots: &[String],
    file_tree_open: bool,
    cx: &mut Context<Workspace>,
) -> FileContentParts {
    let (muted, fg, border, warning) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.border, t.warning)
    };
    match open_file {
        None => FileContentParts {
            header: None,
            body: placeholder_view("文件", muted).into_any_element(),
        },
        Some(of) => {
            // 面包屑：项目名（根目录名）> 中间目录 > 文件名，参考 Codex App——
            // 光看文件名分不清「这是哪个项目/哪层目录下的文件」，尤其多根工作区。
            let breadcrumb_segs = breadcrumb_segments(&of.path, roots);
            let is_image = is_previewable_image(&of.path);
            let dirty = of.readable
                && !is_image
                && of.editor.read(cx).value().as_ref() != of.saved_content.as_str();
            // Markdown 用一个动作切换源码/预览，其它文件类型没有预览这一说。
            let is_md = editor_language_for_path(&of.path) == "md";
            let preview = of.preview && is_md;
            let last_idx = breadcrumb_segs.len().saturating_sub(1);
            let this = cx.entity();
            let breadcrumb = h_flex()
                .id("file-breadcrumb")
                .items_center()
                .gap_1()
                .min_w_0()
                .children(breadcrumb_segs.iter().enumerate().flat_map(|(i, seg)| {
                    let is_last = i == last_idx;
                    let path = seg.path.clone();
                    let is_dir = !is_last;
                    let this = this.clone();
                    let seg_el = div()
                        .id(("file-breadcrumb-seg", i))
                        .flex_shrink_0()
                        .px(px(8.))
                        .py(px(5.))
                        .min_h(px(28.))
                        .rounded_md()
                        .flex()
                        .items_center()
                        .text_size(px(14.))
                        .line_height(relative(1.2))
                        .cursor_pointer()
                        .when(is_last, |el| el.text_color(fg).font_semibold())
                        .when(!is_last, |el| el.text_color(muted))
                        .hover(move |el| el.bg(border).text_color(fg))
                        .child(seg.label.clone())
                        .on_click(move |_ev, _window, cx| {
                            this.update(cx, |ws, cx| {
                                ws.reveal_from_breadcrumb(path.clone(), is_dir, cx);
                            });
                        });
                    let sep = (!is_last).then(|| {
                        div()
                            .flex_shrink_0()
                            .text_size(px(14.))
                            .text_color(muted)
                            .child(">")
                    });
                    std::iter::once(seg_el.into_any_element())
                        .chain(sep.map(|s| s.into_any_element()))
                }));
            let header = h_flex()
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
                        // 未保存改动：文件名后一个小圆点，Cmd+S 保存后消失。
                        .when(dirty, |el| {
                            el.child(div().size(px(6.)).rounded_full().bg(warning))
                        })
                        // 保存失败 / 不支持保存的提示。
                        .children(
                            of.save_error
                                .clone()
                                .map(|msg| div().text_xs().text_color(warning).child(msg)),
                        ),
                )
                .when(is_md, |el| {
                    el.child(
                        div()
                            .id("markdown-view-toggle")
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(fg)
                            .cursor_pointer()
                            .hover(|el| el.opacity(0.75))
                            .child(if preview {
                                "查看原始码"
                            } else {
                                "查看预览"
                            })
                            .on_click(cx.listener(move |ws, _ev, _window, cx| {
                                ws.set_file_preview(!preview, cx)
                            })),
                    )
                })
                .child(
                    div()
                        .id("file-tree-toggle")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_6()
                        .rounded_md()
                        .flex_shrink_0()
                        .cursor_pointer()
                        .text_color(fg)
                        .hover(|el| el.bg(border))
                        .child(
                            Icon::new(if file_tree_open {
                                IconName::PanelRight
                            } else {
                                IconName::PanelLeft
                            })
                            .size_4(),
                        )
                        .tooltip(move |window, cx| {
                            Tooltip::new(if file_tree_open {
                                "收起文件树"
                            } else {
                                "展开文件树"
                            })
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|ws, _ev, _window, cx| {
                            ws.toggle_file_tree(cx);
                        })),
                );
            let body: AnyElement = if is_image {
                div()
                    .id("image-file-preview")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(crate::ui_theme::bg_stage()))
                    .child(
                        img(std::path::PathBuf::from(&of.path))
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element()
            } else if preview {
                div()
                    .id("md-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .p_3()
                    .child(div().text_sm().text_color(fg).child(
                        crate::markdown_mermaid::markdown_view(
                            "md-preview-body",
                            of.editor.read(cx).value().to_string(),
                        ),
                    ))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        // 自定义 context_menu 在 InputState 自身的右键事件回调里执行，此时
                        // 该 entity 正处于 update 中——绝不能在这里 editor.read(cx)，否则
                        // 触发 gpui 的重入借用 panic（在 FFI 边界不可 unwind，直接 abort
                        // 崩整个 App）。剪切/复制/发送都在真正执行时（Cut/Copy 的默认实现、
                        // send_open_file_selection）各自判空早退，这里不需要提前查询选中状态
                        // 来控制 disabled，牺牲一点「没选中时置灰」的观感换取不崩。
                        gpui_component::input::Editor::new(&of.editor)
                            .h_full()
                            // code editor 自带行号 gutter；清掉 Input 尺寸预设额外加的
                            // 横向 padding，避免窄侧栏里行号左边再空出一截。
                            .px_0()
                            .context_menu(move |menu, _window, cx| {
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
                                    .menu("发送选中内容到终端", Box::new(SendSelectionToTerminal))
                            }),
                    )
                    .into_any_element()
            };
            FileContentParts {
                header: Some(header.into_any_element()),
                body,
            }
        }
    }
}

impl Workspace {
    /// 「删除文件」二次确认弹窗。
    pub fn render_delete_file_confirm(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some(target) = self.delete_file_target.as_ref() else {
            return div();
        };
        let fg = cx.theme().foreground;

        let (title, body) = if target.is_dir {
            (
                "确定删除这个文件夹吗？",
                format!(
                    "将永久删除「{}」及其全部内容，此操作不可撤销。",
                    target.label
                ),
            )
        } else {
            (
                "确定删除这个文件吗？",
                format!("将永久删除「{}」，此操作不可撤销。", target.label),
            )
        };

        let content = v_flex()
            .child(Self::modal_title(fg, title))
            .child(div().text_sm().text_color(muted).child(body))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-file",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_file(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-file",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_file(cx),
                        cx,
                    )),
            );
        Self::modal_shell(360., true, content, cx)
    }
}
