//! 文件视图的数据：目录缓存、打开的文件、搜索结果，以及从缓存展平出可见的行。
//!
//! 这里的函数只读内存，render 和键盘导航共用。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, SharedString};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::client::protocol::git::GitStatusData;
use smelt_plugin_api::model::runtime_path::PathStyle;

use crate::html_preview::HtmlPreview;

/// 一台机器上的目录列表缓存：目录绝对路径 → 已排序过滤的直接子项。
pub(crate) type DirCache = HashMap<String, DirListing>;

/// 一个目录的直接子项 (名, 是否目录)。
///
/// 没有过期时间：运行时监听着项目，`project.changed` 到了才标 `stale`，下一次刷新重取，
/// 取回来之前旧列表照常显示。远程机器上按时间重取就是每隔几秒过一次网络。
pub(crate) struct DirListing {
    pub(crate) entries: Rc<Vec<(String, bool)>>,
    pub(crate) stale: bool,
}

/// 一个根的 git status，给文件树标改过、新增、未跟踪。取回来之前旧值照常显示。
pub(crate) struct GitMarks {
    pub(crate) value: GitStatusData,
    pub(crate) stale: bool,
}

/// 某台机器上的一个文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileRef {
    pub(crate) runtime: RuntimeKey,
    pub(crate) path: String,
}

/// 打开着的文件：可编辑的代码编辑器（tree-sitter 高亮、行号、搜索），或者图片。
pub(crate) struct OpenFile {
    /// 这一次打开的编号。后台读写回来按它找到自己的文件：文件可能已经随项目停放了。
    pub(crate) id: u64,
    /// 文件在哪台机器上。读写都发给它，跟当前项目在哪台机器上无关。
    pub(crate) runtime: RuntimeKey,
    pub(crate) path: String,
    pub(crate) editor: Entity<gpui_component::input::EditorState>,
    /// 磁盘上（或最近一次保存后）的内容，跟编辑器当前内容一比就知道有没有没保存的改动。
    pub(crate) saved_content: Rc<String>,
    /// 运行时给的内容版本，跟 `saved_content` 同步；保存时带回去做冲突检测。
    pub(crate) saved_version: Option<String>,
    /// 可预览图片的字节。读完之前、读失败或不是图片时是 `None`。
    pub(crate) image: Option<Arc<gpui::Image>>,
    /// 最近一次保存失败、不允许保存的原因；保存成功或重新打开后清空。
    pub(crate) save_error: Option<String>,
    /// 按文本读到过。读完之前、读失败（比如二进制文件）时是 false，不许保存：免得把
    /// 「无法读取」的占位文字写回去盖掉原文件。
    pub(crate) readable: bool,
    /// 上次保存时运行时说磁盘上已经不是打开时那一版（外部改过）。这时再按一次保存就
    /// 不带版本强制覆盖：「再按一次」当作用户确认了。
    pub(crate) conflict_pending: bool,
    /// 项目里有文件变了，正在重读这个文件看它变没变。同一时刻只读一次。
    pub(crate) reload_inflight: bool,
    /// 重读期间又来了变化通知：读完再读一次，不并发。
    pub(crate) reload_again: bool,
    /// 显示预览（只对 [`PreviewKind`] 认得的文件有效）。每次打开按文件类型重置，能预览的
    /// 默认进预览。
    pub(crate) preview: bool,
    /// 预览用的正文。编辑器每个按键都会变，这里停手后才更新，免得每个字都重解析 Markdown、
    /// 重载网页。
    pub(crate) preview_source: SharedString,
    /// HTML 预览的网页视图。只在 HTML 文件开着预览时有，关掉预览就整个丢掉。
    pub(crate) html_preview: Option<Entity<HtmlPreview>>,
}

impl OpenFile {
    /// 编辑器里有没保存的改动。
    pub(crate) fn dirty(&self, cx: &gpui::App) -> bool {
        self.readable && self.editor.read(cx).value().as_ref() != self.saved_content.as_str()
    }

    /// 把预览正文交给 HTML 预览；不是 HTML 或没开预览时什么也不做。
    pub(crate) fn sync_html_preview(&self, cx: &mut gpui::App) {
        if let Some(html) = &self.html_preview {
            let source = self.preview_source.clone();
            html.update(cx, |preview, cx| preview.set_html(&source, cx));
        }
    }
}

/// 能预览的文件类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewKind {
    Markdown,
    Html,
}

impl PreviewKind {
    pub(crate) fn for_path(path: &str) -> Option<Self> {
        match editor_language_for_path(path).as_str() {
            "md" => Some(Self::Markdown),
            "html" | "htm" => Some(Self::Html),
            _ => None,
        }
    }
}

/// 文件扩展名 → 编辑器的语法高亮语言名。组件库的 `Language::from_name` 认常见扩展名，
/// 认不出的回退成纯文本。没有扩展名的（Makefile 等）用文件名本身。
pub(crate) fn editor_language_for_path(path: &str) -> String {
    match PathStyle::LOCAL.extension(path) {
        Some(ext) => ext.to_lowercase(),
        None => PathStyle::LOCAL
            .file_name(path)
            .unwrap_or("text")
            .to_lowercase(),
    }
}

// ===================== 搜索 =====================

/// 搜索的一条命中。
pub(crate) struct SearchHit {
    /// 命中文件的绝对路径。
    pub(crate) path: String,
    /// 相对项目根的路径。
    pub(crate) rel: String,
    /// 内容命中时的第一处：(行号从 1 起, 那一行)；只是文件名命中时是 `None`。
    pub(crate) line: Option<(usize, String)>,
}

/// 一次搜索的结果。后台跑，render 只读。
pub(crate) struct SearchState {
    /// 在哪个项目里搜的、搜的什么：两样都对得上才不用重搜。
    pub(crate) root: crate::ProjectRef,
    pub(crate) query: String,
    /// 跑完了（没跑完时顶上显示「搜索中…」）。
    pub(crate) done: bool,
    /// 文件名命中在前、内容命中在后，各自按路径排。
    pub(crate) hits: Vec<SearchHit>,
    /// 命中数到顶被截断了。
    pub(crate) truncated: bool,
}

/// 搜索结果的目录树：只有命中的文件和它们的上级目录。下标指向 [`SearchState::hits`]，
/// 点了还能跳到内容命中的那一行。
#[derive(Default)]
pub(crate) struct SearchTreeDir {
    pub(crate) dirs: BTreeMap<String, SearchTreeDir>,
    pub(crate) files: Vec<usize>,
}

pub(crate) fn build_search_tree(hits: &[SearchHit]) -> SearchTreeDir {
    let mut root = SearchTreeDir::default();
    for (ix, hit) in hits.iter().enumerate() {
        let mut parts = hit
            .rel
            .split('/')
            .filter(|part| !part.is_empty())
            .peekable();
        let mut dir = &mut root;
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                dir = dir.dirs.entry(part.to_string()).or_default();
            } else {
                dir.files.push(ix);
            }
        }
    }
    root
}

// ===================== 目录树 =====================

/// 文件树一行。顺序就是虚拟列表和键盘 `scroll_to_item` 的下标，两边都用
/// [`collect_file_tree_rows`]，不各自走目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileTreeRow {
    RootHeader {
        path: String,
        open: bool,
        /// 钉进来的根（不是当前项目），菜单里能移除。
        pinned: bool,
    },
    Loading {
        depth: usize,
        root: String,
    },
    Entry {
        depth: usize,
        name: String,
        is_dir: bool,
        path: String,
        expanded: bool,
        /// M / A / D / U。颜色在画的时候按主题取。
        git_mark: Option<char>,
    },
}

impl FileTreeRow {
    pub(crate) fn path(&self) -> Option<&str> {
        match self {
            FileTreeRow::RootHeader { path, .. } | FileTreeRow::Entry { path, .. } => Some(path),
            FileTreeRow::Loading { .. } => None,
        }
    }
}

/// 行的元素身份是路径：上面插入或展开目录，后面行的按压状态不会被挪走。
pub(crate) fn file_tree_row_id(row: &FileTreeRow) -> (&'static str, &str) {
    match row {
        FileTreeRow::Loading { root, .. } => ("root-loading", root.as_str()),
        FileTreeRow::RootHeader { path, .. } => ("root", path.as_str()),
        FileTreeRow::Entry { path, .. } => ("file", path.as_str()),
    }
}

/// text_sm 是 0.875rem，默认行高是黄金比例；再留 2px，免得字形被行盒裁掉。图标固定
/// 14px，行高至少装得下它。
pub(crate) fn file_tree_row_height(rem_px: f32) -> f32 {
    let line = rem_px * 0.875 * std::f32::consts::GOLDEN_RATIO;
    line.max(16.0) + 2.0
}

/// 估一下文件名在树里会不会被截短，会才挂悬停提示。
pub(crate) fn name_likely_truncated(name: &str, depth: usize, panel_w: f32) -> bool {
    // 左内边距 8 + 每层缩进 14 + 箭头 14 + 图标 14 + 间距 + 右内边距 8
    let chrome = 56.0 + depth as f32 * 14.0;
    let text_w = (panel_w - chrome).max(0.0);
    // text_sm 大约每字 7–8px；略保守，免得短文件名也弹提示。
    let max_chars = (text_w / 7.5).floor() as usize;
    name.chars().count() > max_chars
}

/// porcelain 两位状态码 → 简标（M 改 / A 增 / D 删 / U 未跟踪）。
pub(crate) fn git_status_mark(code: &str) -> Option<char> {
    let chars: String = code.chars().take(2).collect();
    if chars.contains('D') {
        Some('D')
    } else if chars.contains('A') {
        Some('A')
    } else if chars.contains('?') {
        Some('U')
    } else if chars.chars().any(|c| c != ' ' && c != '?') {
        Some('M')
    } else {
        None
    }
}

fn git_mark_index(files: &[(String, String)]) -> HashMap<&str, char> {
    let mut marks = HashMap::new();
    for (code, path) in files {
        if let Some(mark) = git_status_mark(code) {
            marks.insert(path.as_str(), mark);
        }
    }
    marks
}

/// 只读缓存，递归收集已展开、已缓存的目录。展开了但还没缓存的跳过：刷新会去补，缓存
/// 写回后再画出来。
fn walk_dir_cached(
    style: PathStyle,
    dir: &str,
    dir_cache: &DirCache,
    expanded: &HashSet<String>,
    depth: usize,
    out: &mut Vec<(usize, String, bool, String, bool)>,
) {
    let Some(listing) = dir_cache.get(dir) else {
        return;
    };
    for (name, is_dir) in listing.entries.iter() {
        let path = style.join(dir, name);
        let is_expanded = expanded.contains(&path);
        out.push((depth, name.clone(), *is_dir, path.clone(), is_expanded));
        if *is_dir && is_expanded {
            walk_dir_cached(style, &path, dir_cache, expanded, depth + 1, out);
        }
    }
}

/// 树要挂出来的根和它们的折叠、钉住状态。
pub(crate) struct TreeRoots<'a> {
    /// 第一个是当前项目（有的话），后面是钉住的。
    pub(crate) roots: &'a [String],
    pub(crate) pinned: &'a [String],
    pub(crate) collapsed: &'a HashSet<String>,
}

/// 展平当前可见的树。只读缓存。单根不插标题行；多根才有可折叠的根标题。
/// `style` 是这些路径所在机器的规则；`git_status` 按根取那个仓库的状态。
pub(crate) fn collect_file_tree_rows<'a>(
    style: PathStyle,
    roots: &TreeRoots<'_>,
    expanded: &HashSet<String>,
    dir_cache: &DirCache,
    git_status: &dyn Fn(&str) -> Option<&'a GitStatusData>,
) -> Vec<FileTreeRow> {
    let multi = roots.roots.len() > 1;
    let mut rows = Vec::new();
    for root in roots.roots {
        let marks = git_status(root).map(|data| git_mark_index(&data.files));
        let root_open = !roots.collapsed.contains(root);
        if multi {
            rows.push(FileTreeRow::RootHeader {
                path: root.clone(),
                open: root_open,
                pinned: roots.pinned.contains(root),
            });
            if !root_open {
                continue;
            }
        }
        if !dir_cache.contains_key(root) {
            rows.push(FileTreeRow::Loading {
                depth: usize::from(multi),
                root: root.clone(),
            });
            continue;
        }
        let mut flat = Vec::new();
        walk_dir_cached(
            style,
            root,
            dir_cache,
            expanded,
            usize::from(multi),
            &mut flat,
        );
        for (depth, name, is_dir, path, is_expanded) in flat {
            let git_mark = if is_dir {
                None
            } else {
                marks.as_ref().and_then(|marks| {
                    style
                        .strip_prefix(&path, root)
                        .and_then(|rel| marks.get(rel.as_str()).copied())
                })
            };
            rows.push(FileTreeRow::Entry {
                depth,
                name,
                is_dir,
                path,
                expanded: is_expanded,
                git_mark,
            });
        }
    }
    rows
}

/// 面包屑一段：显示的名字和这一段的绝对路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BreadcrumbSeg {
    pub(crate) label: String,
    pub(crate) path: String,
}

/// 按根拆出「项目名 > 中间目录 > 文件名」。多根时取最长的那个根；对不上任何根就只显示
/// 文件名。路径按 `style` 切。
pub(crate) fn breadcrumb_segments(
    style: PathStyle,
    path: &str,
    roots: &[String],
) -> Vec<BreadcrumbSeg> {
    let Some(root) = roots
        .iter()
        .filter(|root| style.starts_with(path, root))
        .max_by_key(|root| root.len())
    else {
        return vec![BreadcrumbSeg {
            label: style.file_name(path).unwrap_or(path).to_string(),
            path: path.to_string(),
        }];
    };
    let mut segs = vec![BreadcrumbSeg {
        label: style.file_name(root).unwrap_or(root.as_str()).to_string(),
        path: root.clone(),
    }];
    let mut acc = root.clone();
    let rel = style.strip_prefix(path, root).unwrap_or_default();
    let separators: &[char] = match style {
        PathStyle::Posix => &['/'],
        PathStyle::Windows => &['/', '\\'],
    };
    for part in rel.split(separators).filter(|part| !part.is_empty()) {
        acc = style.join(&acc, part);
        segs.push(BreadcrumbSeg {
            label: part.to_string(),
            path: acc.clone(),
        });
    }
    segs
}
