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
    /// 运行时给的内容版本，跟 `saved_content` 同步更新；保存时带回去做冲突检测。
    pub(super) saved_version: Option<String>,
    /// 可预览图片的字节（运行时给的）。读完之前、读失败或不是图片时为 `None`。
    pub(super) image: Option<std::sync::Arc<gpui::Image>>,
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
    /// 预览用的正文。编辑器每个按键都会变，这里按停手后再更新，避免每帧重解析 Markdown。
    pub(super) preview_source: SharedString,
}

/// 保存一次的结果：分 Saved（带新版本）/ 检测到外部改动的 Conflict / 其它错误。
enum SaveOutcome {
    Saved(String),
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

// ===================== 搜索 =====================

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
