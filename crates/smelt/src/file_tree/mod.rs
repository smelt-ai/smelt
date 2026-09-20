//! 文件树 + 文件内容查看/编辑：目录浏览、打开/保存、项目内搜索。
//!
//! 数据修改在 `workspace.rs`，页面在 `view.rs`。字段仍由 main.rs 的 Workspace 持有。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui::*;

#[cfg(test)]
mod tests;
mod view;
mod workspace;

pub(crate) use view::FileTreeParams;
pub use view::{file_content_parts, file_tree, search_results_view};

// ===================== 类型 =====================

pub(super) type DirCache = HashMap<String, (Instant, Rc<Vec<(String, bool)>>)>;

/// 文件树右键「删除文件」的二次确认目标。
#[derive(Clone)]
pub struct DeleteFileTarget {
    pub path: String,
    pub is_dir: bool,
    /// 弹窗里展示的文件/文件夹名（从 path 取 basename）。
    pub label: String,
}

/// 打开查看的文件：路径 + 可编辑的代码编辑器状态（gpui-component 的 Editor：
/// tree-sitter 语法高亮 + 行号 + 搜索，直接可编辑，不再是只读预览）。
pub struct OpenFile {
    pub path: String,
    pub(crate) editor: Entity<gpui_component::input::EditorState>,
    /// 磁盘上（或最近一次保存后）的内容快照，跟编辑器当前内容一比就知道是否有未保存
    /// 改动——不用额外订阅 InputEvent::Change 维护一个脏标记，render 时比一下字符串就行。
    pub(super) saved_content: Rc<String>,
    /// 最近一次保存失败 / 不允许保存的原因；成功保存或重新打开文件后清空。
    pub(super) save_error: Option<String>,
    /// 文件是否按文本成功读取过。读取完成前 / 读取失败（比如二进制文件）时为 false，
    /// 禁止保存——避免误按 Cmd+S 把「无法读取」占位文案写回去覆盖了原文件。
    pub(super) readable: bool,
    /// 上次保存时检测到磁盘内容跟 saved_content 对不上（外部改过）。为 true 时
    /// 再按一次 Cmd+S 会跳过冲突检查强制覆盖——用"再按一次"当作用户已确认覆盖。
    pub(super) conflict_pending: bool,
    /// markdown 文件的「预览」开关（仅 .md 生效，见 file_content_parts）；切换打开的
    /// 文件不带过去，open_file_now 每次按文件类型重置（Markdown 默认进预览）。
    pub(super) preview: bool,
}

/// 保存一次的结果：分 Saved / 检测到外部改动的 Conflict / 其它 IO 错误。
enum SaveOutcome {
    Saved,
    Conflict,
    Error(String),
}

/// 文件树搜索的一条命中。
struct SearchHit {
    /// 命中文件的绝对路径（点击时用它 view_file）。
    path: String,
    /// 相对项目根的展示路径。
    rel: String,
    /// 内容命中时的首个匹配行：(行号从 1 起, 该行文本预览)；仅文件名命中时为 None。
    line: Option<(usize, String)>,
}

/// 文件树搜索的一次结果快照。后台遍历项目填充，render 只读。
pub struct SearchState {
    /// 触发本次结果的查询串（用于判断是否需要重跑）。
    query: String,
    /// 后台遍历是否已跑完（false 时列表顶部显示「搜索中…」）。
    done: bool,
    /// 命中列表（文件名命中在前、内容命中在后，各自按路径序）。
    hits: Vec<SearchHit>,
    /// 是否因命中数触顶而截断（列表底部提示还有更多）。
    truncated: bool,
}

/// 搜索命中数上限：触顶即停并标记截断，避免超大仓遍历/渲染失控。
const SEARCH_HIT_LIMIT: usize = 200;
/// 内容搜索跳过的单文件大小上限（512KB）：更大的多半是数据/构建产物，逐行扫不划算。
const SEARCH_MAX_FILE_BYTES: u64 = 512 * 1024;

// ===================== 搜索 =====================

/// 后台遍历项目搜索 query（大小写不敏感）：文件名命中或文件内容逐行命中。
/// 使用 `ignore` crate 遵循 `.gitignore`、`.git/info/exclude`、全局 gitignore 与隐藏文件规则，
/// 兜底跳过 .git/node_modules/target/.DS_Store，并采用流式行读取避免大文件占用过多内存。
/// 返回 (命中列表, 是否因触顶截断)；文件名命中排在内容命中前。绝不在此之外做 UI 调用。
fn search_project(root: &str, query: &str) -> (Vec<SearchHit>, bool) {
    let needle = query.to_lowercase();
    let mut name_hits: Vec<SearchHit> = Vec::new();
    let mut content_hits: Vec<SearchHit> = Vec::new();
    let root_path = std::path::Path::new(root);
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .ignore(true)
        .parents(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if matches!(
                name.as_ref(),
                ".git" | "node_modules" | "target" | ".DS_Store"
            ) {
                return false;
            }
            true
        })
        .build();

    for entry in walker.flatten() {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        let rel = path
            .strip_prefix(root_path)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let abs = path.to_string_lossy().to_string();

        // 1. 文件名命中：直接记一条（不再看内容），命中行留空
        if name.to_lowercase().contains(&needle) {
            name_hits.push(SearchHit {
                path: abs,
                rel,
                line: None,
            });
            if name_hits.len() + content_hits.len() >= SEARCH_HIT_LIMIT {
                truncated = true;
                break;
            }
            continue;
        }

        // 2. 内容命中：跳过大文件
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > SEARCH_MAX_FILE_BYTES {
            continue;
        }

        // 流式逐行扫描，避免将大文件全部读入内存；含 NUL 视为二进制并中断
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut reader = std::io::BufReader::new(file);
        let mut line_buf = String::new();
        let mut line_no = 0usize;

        use std::io::BufRead as _;
        while let Ok(n) = reader.read_line(&mut line_buf) {
            if n == 0 {
                break;
            }
            line_no += 1;
            // 含 NUL 视为二进制，不逐行扫
            if line_buf.as_bytes().contains(&0) {
                break;
            }
            if line_buf.to_lowercase().contains(&needle) {
                // 预览行去掉首尾空白并截断，避免超长行撑爆列表
                let preview: String = line_buf.trim().chars().take(200).collect();
                content_hits.push(SearchHit {
                    path: abs,
                    rel,
                    line: Some((line_no, preview)),
                });
                if name_hits.len() + content_hits.len() >= SEARCH_HIT_LIMIT {
                    truncated = true;
                }
                break;
            }
            line_buf.clear();
        }

        if truncated {
            break;
        }
    }

    // 保持相对路径顺序稳定
    name_hits.sort_by(|a, b| a.rel.cmp(&b.rel));
    content_hits.sort_by(|a, b| a.rel.cmp(&b.rel));

    name_hits.extend(content_hits);
    (name_hits, truncated)
}

/// 搜索结果的临时目录树。只含命中文件和它们的祖先目录，不触碰磁盘；索引指向
/// `SearchState::hits`，因此仍能保留内容命中对应的跳转行号。
#[derive(Default)]
struct SearchTreeDir {
    dirs: BTreeMap<String, SearchTreeDir>,
    files: Vec<usize>,
}

fn build_search_tree(hits: &[SearchHit]) -> SearchTreeDir {
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
