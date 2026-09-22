//! Git 面板：状态/分支/diff 查看、暂存/提交/推送、worktree 新建与删除。
//!
//! 从 main.rs 拆出来的 `impl Workspace` 方法 + 独立渲染/解析/git 子进程调用函数，
//! 字段仍然声明在 main.rs 的 `Workspace` struct 里（没有挪成子结构体）。
//! 数据修改在 `workspace.rs`，页面在 `view.rs`。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui::{InteractiveElement, StatefulInteractiveElement};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::resizable::resizable_panel;
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
use notify::{RecursiveMode, Watcher};

use crate::{GitTab, Workspace, placeholder_view, resizable_split::h_resizable, ui_theme};
use smelt_core::fs::{FileSystem, LocalFs};
pub use smelt_git::{
    BranchList, DiffKind, DiffLine, DiffScope, GitDiff, GitStatusData, GitTreeRow, RepoInfo,
    WorktreeEntry, build_git_tree, full_file_path, list_worktrees, main_repo_root_from_common_dir,
    parse_diff, remove_worktree, repo_label_from_common_dir, run_git,
};
use smelt_git::{
    apply_patch as apply_git_patch, checkout_branch as checkout_git_branch, commit_and_maybe_push,
    delete_branch as delete_git_branch, discard_all, fetch_remote, git_err, hunk_patch,
    load_branches, load_git_status, load_repo_info, merge_branch as merge_git_branch,
    parse_diff_with_file_headers, pull_rebase, push_current, stage_file as stage_git_file,
    stash_pop, stash_push, unstage_file as unstage_git_file,
};
#[cfg(test)]
use smelt_git::{parse_worktree_list, run_git_stdin};

mod diff;
pub(crate) use diff::*;
#[cfg(test)]
mod tests;
mod view;
mod workspace;

// ===================== 类型 =====================

/// 一个项目里发现到的仓库集合。
///
/// 变更栏按仓库分组，每个仓库都是独立的提交单位——这是「子仓改动提交不出去」
/// 那个 bug 的根治办法：文件天生属于某个仓库，不再靠路径反推。
#[derive(Clone, Default)]
pub struct RepoSet {
    pub repos: Vec<smelt_git::discovery::DiscoveredRepo>,
    /// 发现数量超上限被截断。UI 要明说，不能假装列全了。
    pub truncated: bool,
}

impl RepoSet {
    pub fn roots(&self) -> impl Iterator<Item = &str> {
        self.repos
            .iter()
            .map(|r| r.root.to_str().unwrap_or_default())
    }
}

/// 「删除 Worktree」弹窗要删的目标：path 是待删的 worktree 检出目录；main_root 是
/// 同仓库下另一个稳定存在的目录（主仓库根），`git worktree remove` 必须从别处发起，
/// 不能从待删目录自己发起。dirty = None 表示后台「有没有未提交改动」还没探测完，
/// 弹窗先显示"检查中"。
#[derive(Clone)]
pub struct DeleteWorktreeTarget {
    pub path: String,
    pub main_root: String,
    pub branch: String,
    pub dirty: Option<bool>,
}

/// 「关联 Worktree」弹窗状态（字段声明在 main.rs 的 Workspace 上）。
/// entries=None 且 error=None = 正在后台加载。
pub struct WorktreeListState {
    /// 右键点击的项目根（主仓库或任意 worktree 都行，git 自己定位 common dir）。
    pub root: String,
    /// 同仓库主仓库根，删除/清理操作从它发起；加载完成后填上。
    pub main_root: String,
    pub entries: Option<Vec<WorktreeEntry>>,
    pub error: Option<String>,
}

/// 「新建 Worktree」弹窗状态（字段声明在 main.rs 的 Workspace 上）。
/// 两个输入框是独立的 InputState 实体，随弹窗关闭一起 drop。
pub struct NewWorktreeState {
    /// 同仓库主仓库根，`git worktree add` 从它发起（session_list 用已缓存的
    /// repo_info 算好传进来，不再重复跑 git）。
    pub main_root: String,
    /// 创建时的基准分支（仅提示用；创建本身总是基于当前 HEAD）。
    pub base_branch: String,
    /// 分支名输入框（留空 = detached HEAD）。
    pub branch_input: Entity<gpui_component::input::InputState>,
    /// 目标检出目录输入框。
    pub path_input: Entity<gpui_component::input::InputState>,
    /// 创建进行中（按钮置灰防连点）。
    pub busy: bool,
    /// 上次创建失败的错误文案（显示在弹窗里）。
    pub error: Option<String>,
    /// 输入框事件订阅，随 state 一起释放。
    pub _subs: Vec<Subscription>,
}

#[derive(Clone)]
pub(crate) enum AggregateDiffRow {
    Header {
        path: String,
        adds: usize,
        dels: usize,
    },
    Line(usize),
}

/// 可变高度虚拟列表的显示行。评论卡片是选区后的真实一行，而非覆盖在底部的
/// 全局输入框，因此视觉和交互都牢牢锚定在被审查的代码范围上。
#[derive(Clone)]
pub(crate) enum DiffReviewRow {
    Header {
        path: String,
        adds: usize,
        dels: usize,
    },
    Line(usize),
    CommentComposer,
}

/// 聚合 diff 中一个文件的原始标题行及统计。
#[derive(Clone)]
pub(crate) struct AggregateFileHeader {
    adds: usize,
    dels: usize,
}

/// 聚合 diff 中一个文件标题在未插入评论器时的纵向位置。
#[derive(Clone)]
pub(crate) struct AggregateStickyHeader {
    path: String,
    adds: usize,
    dels: usize,
    top: f32,
    /// 下一个文件标题到当前标题的距离，用于模拟 CSS sticky 的顶出过渡。
    height: f32,
    /// 当前文件标题之后、下一个标题之前的内容高度。折叠 sticky 文件时用来
    /// 调整滚动锚点，避免内容收缩把视图直接推到下一个文件。
    body_height: f32,
    /// 当前标题相对滚动区顶部的显示偏移。接近下一个文件时会逐步变为负值。
    offset_y: f32,
    /// 标题在当前视图基础行数组中的位置。统一视图插入评论器后，用它补偿后续
    /// 标题的纵向偏移；并排视图没有评论器，也沿用该字段方便共用定位逻辑。
    row_index: usize,
}

/// diff 视图每帧重算的派生数据缓存。diff 身份（diff_gen + root + path）、
/// 并排/统一（split）、折叠状态（collapsed_gen）任一变化才重算；命中时每帧
/// 复用，避免大 diff 下每帧 O(n) 重算（gutter 宽、内容宽、展开行）掉帧。
/// 字段只在 git_panel 模块内读写；main.rs 仅作为不透明缓存整体存取。
#[derive(Clone)]
pub(crate) struct DiffDerivedCache {
    diff_gen: u64,
    root: String,
    path: String,
    split: bool,
    collapsed_gen: u64,
    gutter_w: f32,
    content_w: f32,
    /// 统一视图的显示行（不含评论器——评论器按当前选行动态插入，见 git_diff_pane）。
    rows: Rc<Vec<DiffReviewRow>>,
    /// 并排视图的显示行。
    split_rows: Rc<Vec<SplitRow>>,
    /// 聚合视图各文件标题的纵向位置，供 sticky 标题按滚动偏移二分定位。
    sticky_headers: Rc<Vec<AggregateStickyHeader>>,
}

fn is_regular_worktree_file(path: &Path) -> bool {
    // 使用 fs 接缝：本地面板和将来的远程 worktree 用同一套判断逻辑。
    smelt_core::fs::is_regular_file(&smelt_core::fs::LocalFs, path)
}

/// 当前视图下 hunk 该给哪些按钮。
fn git_diff_hunk_ops(diff: &GitDiff) -> HunkOps {
    if !diff.patchable {
        return HunkOps::None;
    }
    match diff.scope {
        DiffScope::Unstaged => HunkOps::StageDiscard,
        DiffScope::Staged => HunkOps::Unstage,
        // 混合视图：只有当这个文件压根没暂存过，两层才是同一份差异。
        DiffScope::All if !diff.has_staged => HunkOps::StageDiscard,
        DiffScope::All => HunkOps::None,
    }
}

/// 当前视图下，hunk 上该出现哪些按钮。
#[derive(Clone, Copy, PartialEq)]
enum HunkOps {
    /// 未暂存的改动：可以暂存进索引，也可以直接丢弃。
    StageDiscard,
    /// 已暂存的改动：只能退回工作区（丢弃要去「未暂存」视图做，语义才清楚）。
    Unstage,
    /// 给不了按钮：不可 patch，或「全部」视图下这个文件确实混着两层改动。
    None,
}

/// 窄版 Git 面板的扁平行模型。目录树仍由 `build_git_tree` 生成，但不再把整棵树
/// 一次性变成 GPUI 元素；列表只构造当前视口附近的行。
#[derive(Clone)]
enum NarrowGitRow {
    /// 仓库分组标题。只有多于一个仓库时才出现，单仓工作区不加噪音。
    /// 点它把提交目标切到这个仓库，右侧按钮是该仓库自己的 git 操作。
    RepoHeader {
        root: Rc<str>,
        label: String,
        branch: String,
        kind: smelt_git::discovery::RepoKind,
        active: bool,
        collapsed: bool,
        ahead: u32,
        behind: u32,
        stash_n: u32,
        has_changes: bool,
    },
    /// 仓库自己的提交区：输入框 + 提交按钮。紧跟在它的仓库行下面，
    /// 跟 VS Code 一样——提交信息属于仓库，放在面板底部会让人看不出在往哪提交。
    CommitBox {
        root: Rc<str>,
        input: Entity<gpui_component::input::TextareaState>,
        has_text: bool,
        /// 这个仓库有没有已暂存的改动。
        ///
        /// `git commit` 不带 `-a`，没有暂存内容时它必定失败（退出码 1，还只吐一段
        /// 英文诊断）。与其让用户点一个注定撞墙的按钮，不如先禁用并说明原因。
        has_staged: bool,
        pushing: bool,
        ahead: u32,
        /// 上一次提交/推送失败的原因，直接挂在这个仓库的提交框下面。
        error: Option<String>,
        depth: usize,
    },
    Header {
        key: &'static str,
        title: String,
        /// 属于哪一层：多仓时分组标题要比仓库行缩进一级。
        depth: usize,
        /// 整组批量暂存/撤出的目标；None = 这行只是纯提示（比如"还有 N 项未显示"）。
        batch: Option<HeaderBatch>,
    },
    Tree {
        key: &'static str,
        /// 这行属于哪个仓库。文件不再靠路径反推归属，暂存与提交才不会分家。
        root: Rc<str>,
        is_staged_group: bool,
        /// 仓库分组带来的额外缩进层级。
        depth: usize,
        /// 这条是子模块指针而不是普通文件。
        gitlink: bool,
        row: GitTreeRow,
    },
}

const NARROW_GIT_ROW_H: f32 = 24.0;
/// 仓库分组带来的一级缩进。窄面板里只能给这么多，再大文件名就没地方了。
const NARROW_GIT_INDENT: f32 = 10.0;
/// 单个仓库在变更列表里最多渲染多少行。
///
/// 列表混着仓库行、提交框和文件行，高度不一，用不了等高虚拟列表，所以是常规
/// 布局：行数无上限时超大改动集会让布局树每帧重排。上限之外明确提示还有多少，
/// 比默默卡住诚实。
const NARROW_GIT_MAX_ROWS_PER_REPO: usize = 300;

/// 仓库分组标题前的身份标记。用字符而不是图标：变更栏行高 24px，
/// 再塞图标会把本就窄的一行挤成堆砌。
fn repo_kind_glyph(kind: smelt_git::discovery::RepoKind) -> &'static str {
    use smelt_git::discovery::RepoKind;
    match kind {
        RepoKind::Root => "◆",
        RepoKind::Submodule => "◇",
        RepoKind::Nested => "◌",
    }
}

/// 单文件暂存/取消暂存尚未被一次操作后的权威 `git status` 确认时的 UI 状态。
///
/// 只记录"跑到哪一步了"，不记录"期望变成什么"。方向不需要猜：一行属于
/// 「暂存的更改」组就只能撤出，属于「更改」组就只能加入。曾经按期望状态翻转
/// 图标方向，结果部分暂存的文件（`MM`）在两组各有一行、却共用同一条 pending，
/// 暂存组那行的符号会跟着另一组的操作一起翻，点下去方向正好相反。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingGitIndexOp {
    /// false 表示 git add/reset 仍在执行；true 表示已完成并等待当前代 status 回包。
    completed: bool,
}

/// 这个路径上是否还有**正在执行**的 git 索引操作。
///
/// 只拦真正在跑的那条：`completed = true` 表示 git 已经返回，只是还在等权威
/// status 回包确认。以前这里拦的是"有没有 pending"，于是 status 因为任何原因
/// 没走到确认分支（迟到回包、读取失败重试、期间切了项目），那个路径的按钮就
/// 永久静默失效——表现就是"点了完全没反应"，连错误提示都没有。
fn git_index_op_in_flight(pending: Option<&PendingGitIndexOp>) -> bool {
    pending.is_some_and(|operation| !operation.completed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitStatusResponseAction {
    AcceptCurrent,
    AcceptStaleAndRefresh,
    PreserveAndRetry,
}

fn git_status_response_action(
    current_generation: u64,
    request_generation: u64,
    ok: bool,
) -> GitStatusResponseAction {
    if !ok {
        GitStatusResponseAction::PreserveAndRetry
    } else if current_generation == request_generation {
        GitStatusResponseAction::AcceptCurrent
    } else {
        GitStatusResponseAction::AcceptStaleAndRefresh
    }
}

const GIT_STATUS_MAX_AUTO_RETRIES: u8 = 3;

/// 窄版变更列表为空时的占位文案。
///
/// 已经拿到一份成功的空快照时，即使后台还在刷新，也显示「工作区干净」——否则
/// 文件事件把 inflight 钉住时，干净工作区会永远停在「正在读取改动」。
/// 失败占位（`ok = false`）不能当成干净，那会把读失败伪装成没有改动。
pub(crate) fn git_empty_list_message(
    status: Option<&GitStatusData>,
    refreshing: bool,
    failure_count: u8,
) -> &'static str {
    if status.is_some_and(|status| status.ok && status.files.is_empty()) {
        return "工作区干净";
    }
    if refreshing {
        if failure_count > 0 {
            "Git 状态读取失败，正在重试…"
        } else {
            "正在读取更改…"
        }
    } else if failure_count > 0 {
        "Git 状态读取失败"
    } else if status.is_none() {
        "正在读取更改…"
    } else {
        "工作区干净"
    }
}

/// 成功读到空工作区时关掉聚合「全部改动」。失败快照不能关，否则读失败会把
/// 还在看的 diff 清掉。
pub(crate) fn should_close_aggregate_diff(status: Option<&GitStatusData>, aggregate: bool) -> bool {
    aggregate && status.is_some_and(|status| status.ok && status.files.is_empty())
}

fn git_status_retry_delay(failure_count: u8) -> Option<std::time::Duration> {
    if failure_count == 0 || failure_count > GIT_STATUS_MAX_AUTO_RETRIES {
        return None;
    }
    Some(std::time::Duration::from_millis(
        150 * (1_u64 << (failure_count - 1)),
    ))
}

fn clear_confirmed_git_index_ops(
    pending: &mut HashMap<(String, String), PendingGitIndexOp>,
    root: &str,
) {
    pending.retain(|(pending_root, _), operation| pending_root != root || !operation.completed);
}

/// 选出写操作（提交/推送/分支）的目标仓库。
///
/// 选中的仓库不在发现结果里（被删了、或切了项目）就回退，不能拿着一个不存在的
/// 路径去跑 git。发现还没回来（`known` 为空）时尊重用户的选择：这时把他选的仓库
/// 抢回项目根，反而会让提交落错地方。
///
/// `fallback` 是用户没选过时的默认落点（调用方给的是"第一个有改动的仓库"）。
/// 没有它就只能死认项目根，而项目根常常自己是干净的——改动全在子仓，或者
/// submodule 配了 `ignore`——于是面板会判定"没有改动"，连 diff 预览都不开。
fn resolve_git_write_target(
    project_root: &str,
    selected: Option<&str>,
    known: Option<&RepoSet>,
    fallback: Option<&str>,
) -> String {
    match selected {
        Some(selected)
            if known
                .is_none_or(|set| set.repos.is_empty() || set.roots().any(|r| r == selected)) =>
        {
            selected.to_string()
        }
        _ => fallback.unwrap_or(project_root).to_string(),
    }
}

/// 一组变更文件：(porcelain XY 码, 仓库内相对路径)。
pub(crate) type GitStatusFiles = Vec<(String, String)>;

/// 这个仓库还有没有值得在变更栏里露面的事。
///
/// 「工作区干净」不等于「没事可做」：本地攒着待推送的提交、远端有待拉取的提交、
/// 栈里压着 stash，都得留一行入口。曾经只看有没有改动，于是提交完最后一笔改动，
/// 仓库连同它的推送入口一起从列表里消失，用户再没有任何地方可以 push。
pub(crate) fn repo_needs_attention(
    has_changes: bool,
    ahead: u32,
    behind: u32,
    stash_count: u32,
) -> bool {
    has_changes || ahead > 0 || behind > 0 || stash_count > 0
}

/// porcelain XY 两位码拆分组：X（index 侧）非空非 ? → STAGED；
/// Y（工作区侧）非空或 ?? → CHANGES。部分暂存的文件两组都出现。
fn split_staged_and_changed(files: &[(String, String)]) -> (GitStatusFiles, GitStatusFiles) {
    let mut staged = Vec::new();
    let mut changed = Vec::new();
    for (code, path) in files {
        if code == "??" {
            changed.push((code.clone(), path.clone()));
            continue;
        }
        let mut cs = code.chars();
        let x = cs.next().unwrap_or(' ');
        let y = cs.next().unwrap_or(' ');
        if x != ' ' {
            staged.push((code.clone(), path.clone()));
        }
        if y != ' ' {
            changed.push((code.clone(), path.clone()));
        }
    }
    (staged, changed)
}

/// 分组标题上的批量操作目标。
#[derive(Clone)]
struct HeaderBatch {
    root: Rc<str>,
    /// 组内所有条目的仓库内相对路径。
    ///
    /// 显式列路径而不是对仓库根跑 `git add -- .`：批量操作的范围必须正好等于
    /// 这一组显示出来的东西。用 `.` 的话，列表一旦有过滤、搜索或上限截断，
    /// "看到的"和"操作的"就会对不上，而这种错误是静默的。
    paths: Vec<String>,
    staged: bool,
}

/// 变更列表里的一组改动（STAGED 或 CHANGES），连同它属于哪个仓库、缩在第几级。
struct NarrowGitGroup<'a> {
    title: String,
    files: &'a [(String, String)],
    key: &'static str,
    is_staged_group: bool,
    root: Rc<str>,
    depth: usize,
    /// 本仓库里属于 gitlink（子模块指针）的路径。
    ///
    /// 这类条目在 status 里长得跟普通文件一样，但含义完全不同：改的不是文件内容，
    /// 而是"父仓记录子仓的哪个 commit"。不标出来，用户只会看到一个目录莫名其妙带 M。
    gitlinks: &'a HashSet<String>,
}

fn append_narrow_git_group(
    out: &mut Vec<NarrowGitRow>,
    group: NarrowGitGroup<'_>,
    collapsed: &HashSet<String>,
) {
    let NarrowGitGroup {
        title,
        files,
        key,
        is_staged_group,
        root,
        depth,
        gitlinks,
    } = group;
    out.push(NarrowGitRow::Header {
        key,
        title,
        depth,
        batch: Some(HeaderBatch {
            root: root.clone(),
            paths: files.iter().map(|(_, path)| path.clone()).collect(),
            staged: is_staged_group,
        }),
    });
    let tree = build_git_tree(files, collapsed);
    let total = tree.len();
    out.extend(
        tree.into_iter()
            .take(NARROW_GIT_MAX_ROWS_PER_REPO)
            .map(|row| NarrowGitRow::Tree {
                key,
                root: root.clone(),
                is_staged_group,
                depth,
                gitlink: gitlinks.contains(&row.path),
                row,
            }),
    );
    if total > NARROW_GIT_MAX_ROWS_PER_REPO {
        out.push(NarrowGitRow::Header {
            key,
            title: format!("… 还有 {} 项未显示", total - NARROW_GIT_MAX_ROWS_PER_REPO),
            depth: depth + 1,
            batch: None,
        });
    }
}

/// 渲染一行。`list_index` 是它在整个列表里的下标，只用作 GPUI element id——
/// 多仓时同一个 (key, 组内下标) 会在不同仓库里重复，全局下标天然唯一。
fn render_narrow_git_row(
    entry: &NarrowGitRow,
    list_index: usize,
    ws: &Entity<Workspace>,
    collapsed: &HashSet<String>,
    index_pending: &HashMap<String, HashMap<String, PendingGitIndexOp>>,
) -> AnyElement {
    use crate::ui_theme;

    match entry {
        NarrowGitRow::RepoHeader {
            root,
            label,
            branch,
            kind,
            active,
            collapsed,
            ahead,
            behind,
            stash_n,
            has_changes,
        } => {
            let ws_pick = ws.clone();
            let root_pick = root.to_string();
            let ws_toggle = ws.clone();
            let root_toggle = root.to_string();
            let ws_ops = ws.clone();
            let root_ops = root.to_string();
            let (ahead, behind, stash_n, has_changes) = (*ahead, *behind, *stash_n, *has_changes);
            let sync_label = if ahead + behind > 0 {
                format!("↑{ahead} ↓{behind}")
            } else {
                "⋯".to_string()
            };
            h_flex()
                .id(("narrow-git-repo", list_index))
                .h(px(NARROW_GIT_ROW_H))
                .items_center()
                .gap_1()
                .mx_1()
                .pl_1()
                .pr(px(2.))
                .rounded(ui_theme::row_radius())
                .text_xs()
                .when(*active, |d| d.bg(rgb(ui_theme::bg_hover())))
                .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                // 折叠箭头：未初始化的仓库没有内容可折，留空位保持对齐。
                .child(
                    div()
                        .id(("narrow-git-repo-caret", list_index))
                        .w(px(12.))
                        .flex_shrink_0()
                        .text_size(px(9.))
                        .text_color(rgb(ui_theme::text_faint()))
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(ui_theme::text_bright())))
                        .child(if *collapsed { "▸" } else { "▾" })
                        .on_click(move |_ev, _window, cx| {
                            let root = root_toggle.clone();
                            ws_toggle.update(cx, |workspace, cx| {
                                if !workspace.git_repo_collapsed.remove(&root) {
                                    workspace.git_repo_collapsed.insert(root);
                                }
                                cx.notify();
                            });
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(if *active {
                            ui_theme::accent()
                        } else {
                            ui_theme::text_faint()
                        }))
                        .child(repo_kind_glyph(*kind)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_semibold()
                        .text_color(rgb(ui_theme::text_bright()))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .id(("narrow-git-repo-branch", list_index))
                        .flex_shrink_0()
                        .max_w(px(96.))
                        .truncate()
                        .text_size(px(10.))
                        .text_color(rgb(ui_theme::text_faint()))
                        .child(branch.clone())
                        .tooltip({
                            let b = branch.clone();
                            move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(b.clone())
                                    .build(window, cx)
                            }
                        }),
                )
                // 仓库自己的操作入口：获取/拉取/暂存/丢弃都作用于这一个仓库，
                // 而不是“当前项目”——多仓工作区里后者根本不是一个明确的对象。
                .child(
                    Button::new(("narrow-git-repo-ops", list_index))
                            .ghost()
                            .xsmall()
                            .label(sync_label)
                            .font_family(crate::terminal_view::font_family())
                            .text_color(rgb(if ahead + behind > 0 {
                                ui_theme::green()
                            } else {
                                ui_theme::text_faint()
                            }))
                        .dropdown_menu(move |menu, _window, _cx| {
                            git_ops_menu_items(
                                menu,
                                ws_ops.clone(),
                                root_ops.clone(),
                                ahead,
                                behind,
                                stash_n,
                                has_changes,
                            )
                        }),
                )
                .cursor_pointer()
                .on_click(move |_ev, _window, cx| {
                    let root = root_pick.clone();
                    ws_pick.update(cx, |workspace, cx| {
                        workspace.set_active_git_repo(root, cx);
                    });
                })
                .into_any_element()
        }
        NarrowGitRow::CommitBox {
            root,
            input,
            has_text,
            has_staged,
            pushing,
            ahead,
            error,
            depth,
        } => {
            let (has_text, has_staged, pushing, ahead) = (*has_text, *has_staged, *pushing, *ahead);
            let can_commit = has_text && has_staged && !pushing;
            let ws_target = ws.clone();
            let root_target = root.to_string();
            let ws_commit = ws.clone();
            let root_commit = root.to_string();
            let ws_push = ws.clone();
            let root_push = root.to_string();
            let commit_label = if pushing {
                "推送中…"
            } else {
                "提交并推送"
            };
            v_flex()
                .gap_1p5()
                .pl(px(12. + *depth as f32 * NARROW_GIT_INDENT))
                .pr_2()
                .py_1()
                .child(gpui_component::input::Textarea::new(input))
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(
                            div()
                                .id(("narrow-commit-push", list_index))
                                .flex_1()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(ui_theme::row_radius())
                                .text_xs()
                                .font_semibold()
                                .map(|d| {
                                    if can_commit {
                                        d.bg(rgb(ui_theme::action_fill()))
                                            .text_color(rgb(ui_theme::action_on()))
                                            .cursor_pointer()
                                            .hover(|d| d.opacity(0.9))
                                    } else {
                                        d.bg(rgb(ui_theme::bg_card()))
                                            .text_color(rgb(ui_theme::text_faint()))
                                    }
                                })
                                .child(commit_label)
                                .when(can_commit, |d| {
                                    d.on_click(move |_ev, window, cx| {
                                        let root = root_commit.clone();
                                        ws_commit.update(cx, |ws, cx| {
                                            if ws.pushing {
                                                return;
                                            }
                                            // 按钮点谁就提交谁，不依赖当前选中的是哪个仓库。
                                            ws.set_active_git_repo(root, cx);
                                            ws.commit(true, window, cx);
                                        });
                                    })
                                }),
                        )
                        .child(
                            div()
                                .id(("narrow-commit-only", list_index))
                                .px_2()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .rounded(ui_theme::row_radius())
                                .text_xs()
                                .text_color(rgb(if can_commit {
                                    ui_theme::text_muted()
                                } else {
                                    ui_theme::text_faint()
                                }))
                                .when(can_commit, |d| {
                                    d.cursor_pointer()
                                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                                        .on_click(move |_ev, window, cx| {
                                            let root = root_target.clone();
                                            ws_target.update(cx, |ws, cx| {
                                                ws.set_active_git_repo(root, cx);
                                                ws.commit(false, window, cx);
                                            });
                                        })
                                })
                                .child("仅提交"),
                        )
                        .child(
                            div()
                                .id(("narrow-push-only", list_index))
                                .px_2()
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .rounded(ui_theme::row_radius())
                                .text_xs()
                                .text_color(rgb(if ahead > 0 && !pushing {
                                    ui_theme::text_muted()
                                } else {
                                    ui_theme::text_faint()
                                }))
                                .when(ahead > 0 && !pushing, |d| {
                                    d.cursor_pointer()
                                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                                })
                                .child(format!("↑{ahead}"))
                                .on_click(move |_ev, _window, cx| {
                                    let root = root_push.clone();
                                    ws_push.update(cx, |ws, cx| {
                                        if ws.pushing {
                                            return;
                                        }
                                        ws.set_active_git_repo(root, cx);
                                        ws.push_only(cx);
                                    });
                                }),
                        ),
                )
                .when(has_text && !has_staged, |col| {
                    col.child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(ui_theme::text_faint()))
                            .child("先暂存要提交的更改"),
                    )
                })
                // 失败原因贴在按钮正下方：用户的视线本来就在这里，不用去翻通知中心。
                // git 的诊断（hook 报错、缺凭据）常是多行，原样展示才有排查价值。
                .children(error.clone().map(|err| {
                    div()
                        .w_full()
                        .px_1p5()
                        .py_1()
                        .rounded(ui_theme::row_radius())
                        .bg(rgb(ui_theme::bg_card()))
                        .text_size(px(10.))
                        .font_family(crate::terminal_view::font_family())
                        .text_color(rgb(ui_theme::red()))
                        .child(err)
                }))
                .into_any_element()
        }
        NarrowGitRow::Header {
            key,
            title,
            depth,
            batch,
        } => {
            let group_name = SharedString::from(format!("git-row-{list_index}"));
            div()
                .id((*key, list_index))
                .group(group_name.clone())
                .h(px(NARROW_GIT_ROW_H))
                .flex()
                .items_center()
                .pl(px(12. + *depth as f32 * NARROW_GIT_INDENT))
                .pr_3()
                .text_size(px(10.))
                .font_semibold()
                .text_color(rgb(ui_theme::text_faint()))
                .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                .children(batch.as_ref().map(|batch| {
                    let batch = batch.clone();
                    let ws_batch = ws.clone();
                    div()
                        .id(("git-group-stage", list_index))
                        .flex_shrink_0()
                        .w(px(16.))
                        .text_center()
                        .text_size(px(12.))
                        .text_color(rgb(ui_theme::text_muted()))
                        .invisible()
                        .group_hover(group_name, |d| d.visible())
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(ui_theme::text_bright())))
                        .child(if batch.staged { "−" } else { "+" })
                        .on_click(move |_ev, _window, cx| {
                            cx.stop_propagation();
                            let batch = batch.clone();
                            ws_batch.update(cx, |workspace, cx| {
                                workspace.stage_paths(
                                    batch.root.to_string(),
                                    batch.paths.clone(),
                                    !batch.staged,
                                    cx,
                                );
                            });
                        })
                }))
                .into_any_element()
        }
        NarrowGitRow::Tree {
            key,
            root,
            is_staged_group,
            depth,
            gitlink,
            row,
        } => {
            let index = &list_index;
            let root: &str = root;
            let indent = px(10. + *depth as f32 * NARROW_GIT_INDENT + row.depth as f32 * 12.);
            match row.status.as_deref() {
                None => {
                    let is_collapsed = collapsed.contains(&row.path);
                    let toggle_path = row.path.clone();
                    let ws_toggle = ws.clone();
                    // 目录行的批量暂存：`git add -- <dir>` / `git reset -- <dir>`
                    // 原生就按路径前缀生效，不用自己枚举目录下的文件。
                    // 方向跟着分组走：STAGED 组里的目录只可能是"撤出"，CHANGES 组里只可能是"加入"。
                    //
                    // 走 `stage_paths` 而不是单文件的 `stage_file`：后者带一套为单文件
                    // 设计的 pending 乐观状态，键是具体路径。往里塞一条目录路径会有两个
                    // 后果——重复点击被 pending guard 静默拦掉（看起来就是"点了没反应"），
                    // 以及目录的乐观状态跟它下面文件的真实状态对不上。
                    let batch_staged = *is_staged_group;
                    let ws_batch = ws.clone();
                    let root_batch = root.to_string();
                    let path_batch = row.path.clone();
                    let group_name = SharedString::from(format!("git-row-{list_index}"));
                    let batch_action = div()
                        .id(("git-dir-stage", *index))
                        .flex_shrink_0()
                        .w(px(16.))
                        .text_center()
                        .text_size(px(12.))
                        .text_color(rgb(ui_theme::text_muted()))
                        .invisible()
                        .group_hover(group_name.clone(), |d| d.visible())
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(ui_theme::text_bright())))
                        .child(if batch_staged { "−" } else { "+" })
                        .on_click(move |_ev, _window, cx| {
                            cx.stop_propagation();
                            let root = root_batch.clone();
                            let path = path_batch.clone();
                            ws_batch.update(cx, |workspace, cx| {
                                workspace.stage_paths(root, vec![path], !batch_staged, cx);
                            });
                        });
                    div()
                        .id((*key, *index))
                        .group(group_name)
                        .h(px(NARROW_GIT_ROW_H))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .mx_1()
                        .rounded(ui_theme::row_radius())
                        .pl(indent)
                        .pr_3()
                        .text_xs()
                        .font_family(crate::terminal_view::font_family())
                        .text_color(rgb(ui_theme::text_muted()))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                        .child(
                            div()
                                .w(px(10.))
                                .flex_shrink_0()
                                .text_size(px(9.))
                                .child(if is_collapsed { "▸" } else { "▾" }),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
                        .child(batch_action)
                        .on_click(move |_ev, _window, cx| {
                            let path = toggle_path.clone();
                            ws_toggle.update(cx, |workspace, cx| {
                                if !workspace.git_tree_collapsed.remove(&path) {
                                    workspace.git_tree_collapsed.insert(path);
                                }
                                cx.notify();
                            });
                        })
                        .into_any_element()
                }
                Some(code) => {
                    let untracked = code == "??";
                    let letter = code.trim().chars().next().unwrap_or('M').to_string();
                    let letter_color = if untracked || letter == "A" {
                        rgb(ui_theme::green())
                    } else if letter == "D" {
                        rgb(ui_theme::red())
                    } else {
                        rgb(ui_theme::accent())
                    };
                    // 方向由分组决定：「暂存的更改」里只能撤出，「更改」里只能加入。
                    let staged = *is_staged_group;
                    let pending = index_pending.get(root).and_then(|m| m.get(&row.path));

                    // 暂存/取消暂存做成 hover 出现的图标按钮，跟 VS Code 一致。
                    // 复选框在这里是错的隐喻：它读起来像“选中若干文件再做点什么”，
                    // 而它其实是立即生效的 git add / git reset。
                    let ws_stage = ws.clone();
                    let root_stage = root.to_string();
                    let path_stage = row.path.clone();
                    let group_name = SharedString::from(format!("git-row-{list_index}"));
                    // 只在 git 真的还在跑时禁用。等权威 status 确认的那一段必须
                    // 可点：pending 会因为迟到回包、读取失败重试、期间切项目而滞留，
                    // 按"有没有 pending"禁用等于让这个文件的按钮永久失效。
                    let busy = git_index_op_in_flight(pending);
                    let stage_action = div()
                        .id(("git-row-stage", *index))
                        .flex_shrink_0()
                        .w(px(16.))
                        .text_center()
                        .text_size(px(12.))
                        .text_color(rgb(if busy {
                            ui_theme::text_faint()
                        } else {
                            ui_theme::text_muted()
                        }))
                        .invisible()
                        .group_hover(group_name.clone(), |d| d.visible())
                        .when(!busy, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.text_color(rgb(ui_theme::text_bright())))
                                .on_click(move |_ev, _window, cx| {
                                    cx.stop_propagation();
                                    let root = root_stage.clone();
                                    let path = path_stage.clone();
                                    ws_stage.update(cx, |workspace, cx| {
                                        if staged {
                                            workspace.unstage_file(root, path, cx);
                                        } else {
                                            workspace.stage_file(root, path, cx);
                                        }
                                    });
                                })
                        })
                        .child(if staged { "−" } else { "+" });
                    let ws_discard_icon = ws.clone();
                    let root_discard_icon = root.to_string();
                    let path_discard_icon = row.path.clone();
                    let discard_action = div()
                        .id(("git-row-discard", *index))
                        .flex_shrink_0()
                        .w(px(16.))
                        .text_center()
                        .text_size(px(11.))
                        .text_color(rgb(ui_theme::text_muted()))
                        .invisible()
                        .group_hover(group_name.clone(), |d| d.visible())
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(ui_theme::red())))
                        .on_click(move |_ev, _window, cx| {
                            cx.stop_propagation();
                            let root = root_discard_icon.clone();
                            let path = path_discard_icon.clone();
                            ws_discard_icon.update(cx, |workspace, cx| {
                                workspace.start_discard_file(root, path, untracked, cx)
                            });
                        })
                        .child("↩");

                    let ws_row = ws.clone();
                    let root_row = root.to_string();
                    let path_row = row.path.clone();
                    let ws_menu = ws.clone();
                    let root_menu = root.to_string();
                    let path_menu = row.path.clone();
                    let file_path = Path::new(root)
                        .join(&row.path)
                        .to_string_lossy()
                        .to_string();
                    let file_path_copy = file_path.clone();
                    let file_path_finder = file_path;
                    let row_el = div()
                        .id((*key, *index))
                        .group(group_name)
                        .h(px(NARROW_GIT_ROW_H))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .mx_1()
                        .rounded(ui_theme::row_radius())
                        .pl(indent)
                        .pr_3()
                        .text_xs()
                        .font_family(crate::terminal_view::font_family())
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                        // 子模块指针：改的不是文件内容，而是"父仓记录子仓的哪个 commit"。
                        // 用跟仓库行同一个 ◇ 标记，免得它看上去只是个带 M 的目录。
                        .when(*gitlink, |line| {
                            line.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(10.))
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child(repo_kind_glyph(
                                        smelt_git::discovery::RepoKind::Submodule,
                                    )),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(ui_theme::text()))
                                .child(row.name.clone()),
                        )
                        .when(*gitlink, |line| {
                            line.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(10.))
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child("子模块指针"),
                            )
                        })
                        .child(discard_action)
                        .child(stage_action)
                        .child(div().flex_shrink_0().text_color(letter_color).child(letter))
                        .on_click(move |_ev, _window, cx| {
                            ws_row.update(cx, |workspace, cx| {
                                workspace.open_aggregate_file(
                                    root_row.clone(),
                                    path_row.clone(),
                                    cx,
                                );
                            });
                        });

                    row_el
                        .context_menu(move |menu, _window, _cx| {
                            let ws_discard = ws_menu.clone();
                            let root_discard = root_menu.clone();
                            let path_discard = path_menu.clone();
                            let ws_diff = ws_menu.clone();
                            let root_diff = root_menu.clone();
                            let path_diff = path_menu.clone();
                            let ws_copy = ws_menu.clone();
                            let ws_finder = ws_menu.clone();
                            menu.item(
                                PopupMenuItem::new(if untracked {
                                    "删除文件"
                                } else {
                                    "丢弃更改"
                                })
                                .on_click(
                                    move |_ev, _window, cx| {
                                        ws_discard.update(cx, |workspace, cx| {
                                            workspace.start_discard_file(
                                                root_discard.clone(),
                                                path_discard.clone(),
                                                untracked,
                                                cx,
                                            )
                                        });
                                    },
                                ),
                            )
                            .item(PopupMenuItem::new("打开单文件 Diff").on_click(
                                move |_ev, _window, cx| {
                                    ws_diff.update(cx, |workspace, cx| {
                                        workspace.open_diff(
                                            root_diff.clone(),
                                            path_diff.clone(),
                                            untracked,
                                            cx,
                                        )
                                    });
                                },
                            ))
                            .separator()
                            .item(PopupMenuItem::new("复制文件路径").on_click({
                                let path = file_path_copy.clone();
                                move |_ev, _window, cx| {
                                    ws_copy.update(cx, |workspace, cx| {
                                        workspace.copy_file_path_to_clipboard(path.clone(), cx)
                                    });
                                }
                            }))
                            .item(
                                PopupMenuItem::new("在 Finder 中显示").on_click({
                                    let path = file_path_finder.clone();
                                    move |_ev, _window, cx| {
                                        ws_finder.update(cx, |workspace, cx| {
                                            workspace.reveal_path_in_finder(path.clone(), cx)
                                        });
                                    }
                                }),
                            )
                        })
                        .into_any_element()
                }
            }
        }
    }
}

/// 并排视图的一行：Both = 左(旧侧)/右(新侧)各一行（None 为空侧占位）；
/// Full = 横跨整宽的 hunk/meta 行。存的是 GitDiff.lines 里的索引。
pub(crate) enum SplitRow {
    Both(Option<usize>, Option<usize>),
    Full(usize),
}

/// 文件查看的固定行高（供 diff 视图 uniform_list 虚拟滚动，需每行等高）。
const FILE_LINE_H: f32 = 20.0;

/// 评论器插入到 diff 行之间时占用的固定高度。sticky 的位置和折叠锚点都必须
/// 使用同一个值，否则打开评论后标题会出现一帧错位。
const COMMENT_COMPOSER_H: f32 = 188.0;

/// hunk 头行的 hover 分组名：按钮平时隐藏，鼠标进这一行才显形。
const HUNK_ROW_GROUP: &str = "hunk-row";

/// 行号列宽度：按这份 diff 里最大的行号算，别写死。
///
/// 统一视图只展示一个可评论的行号，宽度必须随最大行号而变。diff 画布使用
/// `text_sm` 的等宽字体，按约 8.4px/数字并额外预留左右内边距，四位数不会被
/// 左色条遮住，也不会在小文件里徒增大片空白。
pub(crate) fn gutter_width(lines: &[DiffLine]) -> f32 {
    let max = lines
        .iter()
        .filter_map(|l| l.new_ln.max(l.old_ln))
        .max()
        .unwrap_or(0);
    // 至少留两位，免得开头几行的窄 gutter 和后面宽的对不齐（宽度是整份 diff 统一的，
    // 这里只是给极短文件一个下限）。
    let digits = max.to_string().len().max(2);
    digits as f32 * 8.4 + 12.0
}

// ===================== git 子进程调用 =====================

/// 文件监听噪音路径：这些目录变化极频繁（构建产物/依赖），git status 结果
/// 基本不受它们影响。
fn is_git_noise_path(p: &std::path::Path) -> bool {
    p.components().any(|c| {
        c.as_os_str().to_str().is_some_and(|s| {
            matches!(
                s,
                "target" | "node_modules" | "dist" | "build" | ".next" | ".cache"
            )
        })
    })
}

/// Core 只负责 git worktree add；Smelt 的未跟踪配置继承是 app 特有副作用。
pub fn create_worktree(
    main_root: &str,
    branch: Option<&str>,
    path: &str,
) -> Result<String, String> {
    let created = smelt_git::create_worktree(main_root, branch, path)?;
    inherit_untracked_into(main_root, path);
    Ok(created)
}

fn inherit_untracked_into(main_root: &str, worktree_dir: &str) {
    let root = std::path::Path::new(main_root);
    let resolved = smelt_git::resolve_worktree_path(main_root, worktree_dir);
    let report = crate::worktree_inherit::inherit_if_enabled(root, &resolved);
    if !report.linked.is_empty() {
        eprintln!(
            "[worktree] 继承未跟踪条目 {} 个 -> {}",
            report.linked.len(),
            resolved.display()
        );
    }
    for warning in &report.warnings {
        eprintln!("[worktree] {warning}");
    }
}

/// Core 完成 git prune。
pub fn prune_stale_worktrees(main_root: &str) -> Result<(), String> {
    smelt_git::prune_stale_worktrees(main_root)
}

/// 把「远端同步 + 本地救场」操作组加进一个下拉菜单。舞台 Git 视图的分支头下拉、
/// Tool Panel 窄面板每个仓库行的操作按钮两处共用——别只在一处加，用户看的是
/// Tool Panel 那个。推送↑N / 获取 / 拉取↓N / 储藏 / 恢复储藏(N) / 丢弃全部，
/// 按当前状态决定哪些出现。
///
/// 每一项都先把提交目标切到 `root` 再执行：菜单挂在某个仓库行上，点它却作用于
/// 「当前活动仓库」的话，在 A 的菜单里点拉取会拉到 B 去。
///
/// stash 叫「储藏」不叫「暂存」：暂存已经是 staging area 的名字，两个概念
/// 撞在一起时用户分不清「暂存改动」到底进的是索引还是 stash 栈。
fn git_ops_menu_items(
    mut menu: gpui_component::menu::PopupMenu,
    ws: Entity<Workspace>,
    root: String,
    ahead: u32,
    behind: u32,
    stash_n: u32,
    has_changes: bool,
) -> gpui_component::menu::PopupMenu {
    /// 菜单项统一入口：先认仓库，再干活。
    fn item(
        ws: &Entity<Workspace>,
        root: &str,
        label: impl Into<gpui::SharedString>,
        run: fn(&mut Workspace, &mut Context<Workspace>),
    ) -> PopupMenuItem {
        let ws = ws.clone();
        let root = root.to_string();
        PopupMenuItem::new(label).on_click(move |_ev, _window, cx| {
            let root = root.clone();
            ws.update(cx, |w, cx| {
                w.set_active_git_repo(root, cx);
                run(w, cx);
            });
        })
    }

    // 推送排在最前：本地攒了提交却没有任何改动时，仓库在列表里只剩这一行，
    // 这个菜单就是它唯一的出口。
    if ahead > 0 {
        menu = menu.item(item(&ws, &root, format!("推送 ↑{ahead}"), |w, cx| {
            w.push_only(cx)
        }));
    }
    menu = menu
        .item(item(&ws, &root, "获取", |w, cx| w.git_fetch(cx)))
        .item(item(
            &ws,
            &root,
            if behind > 0 {
                format!("拉取 ↓{behind}")
            } else {
                "拉取".to_string()
            },
            |w, cx| w.git_pull(cx),
        ));
    if has_changes {
        menu = menu.separator().item(item(&ws, &root, "储藏更改", |w, cx| {
            w.git_stash_push(cx)
        }));
    }
    if stash_n > 0 {
        menu = menu.item(item(
            &ws,
            &root,
            format!("恢复储藏 ({stash_n})"),
            |w, cx| w.git_stash_pop(cx),
        ));
    }
    if has_changes {
        let ws_discard = ws;
        let root_discard = root;
        menu = menu
            .separator()
            .item(
                PopupMenuItem::new("丢弃全部更改").on_click(move |_ev, _window, cx| {
                    let root = root_discard.clone();
                    ws_discard.update(cx, |w, cx| {
                        w.set_active_git_repo(root.clone(), cx);
                        w.start_discard_all(root, cx);
                    });
                }),
            );
    }
    menu
}
