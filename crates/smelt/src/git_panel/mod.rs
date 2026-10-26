//! Git 面板：状态/分支/diff 查看、暂存/提交/推送。
//!
//! 从 main.rs 拆出来的 `impl Workspace` 方法 + 独立渲染/解析/git 子进程调用函数，
//! 字段仍然声明在 main.rs 的 `Workspace` struct 里（没有挪成子结构体）。
//! 数据修改在 `workspace.rs`，页面在 `view.rs`。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui::{InteractiveElement, StatefulInteractiveElement};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::resizable::resizable_panel;
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
pub(crate) use smelt_ui::Button;

use crate::{GitTab, Workspace, placeholder_view, resizable_split::h_resizable};
use smelt_runtime_client::protocol::control::ControlCall;
use smelt_runtime_client::protocol::git::*;
pub use smelt_runtime_client::protocol::git::{
    BranchList, DiffLine, DiffScope, GitSeenChange, GitStatusData, GitTreeRow, RepoInfo,
    build_git_tree, full_file_path, parse_diff,
};
use smelt_ui::IconAction as _;

mod diff;
pub(crate) use diff::*;
#[cfg(test)]
mod tests;
mod view;
mod workspace;

// ===================== 运行时调用 =====================

/// 推送、拉取、提交（可能跑 hook、可能顺带推送）这类可能等网络的操作。
const GIT_NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// git 由运行时执行，面板只发请求。必须在后台线程调用。失败给人看的原因：git 的
/// stderr 原样带回来，连不上运行时则是传输错误。
pub(crate) fn git_call<C: ControlCall>(params: C::Params) -> Result<C::Result, String> {
    crate::runtime_call::call::<C>(params, crate::runtime_call::RUNTIME_CALL_TIMEOUT)
        .map_err(crate::runtime_call::error_text)
}

pub(crate) fn git_network_call<C: ControlCall>(params: C::Params) -> Result<C::Result, String> {
    crate::runtime_call::call::<C>(params, GIT_NETWORK_TIMEOUT)
        .map_err(crate::runtime_call::error_text)
}

fn root_params(root: &str) -> GitRootParams {
    GitRootParams {
        root: root.to_string(),
    }
}

// ===================== 类型 =====================

/// diff 预览的视图状态。
pub struct GitDiff {
    pub root: String,
    pub path: String,
    pub aggregate: bool,
    pub worktree_file: Option<String>,
    pub lines: Rc<Vec<DiffLine>>,
    pub header: String,
    pub hunks: Rc<Vec<DiffHunk>>,
    pub patchable: bool,
    pub scope: DiffScope,
    pub has_staged: bool,
    /// 原始补丁。`None` 还在读取；`Some("")` 是已经读完的空差异。
    pub patch: Option<String>,
}

/// 一个项目里发现到的仓库集合。
///
/// 变更栏按仓库分组，每个仓库都是独立的提交单位——这是「子仓改动提交不出去」
/// 那个 bug 的根治办法：文件天生属于某个仓库，不再靠路径反推。
#[derive(Clone, Default)]
pub struct RepoSet {
    pub repos: Vec<DiscoveredRepo>,
    /// 发现数量超上限被截断。UI 要明说，不能假装列全了。
    pub truncated: bool,
}

impl RepoSet {
    pub fn roots(&self) -> impl Iterator<Item = &str> {
        self.repos
            .iter()
            .map(|r| r.root.as_str())
    }
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
        kind: RepoKind,
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
        /// 这个仓库正在跑的 git 写入。有值时提交和推送都不可点。
        git_write: Option<GitWrite>,
        ahead: u32,
        /// 上一次提交/推送失败的原因，直接挂在这个仓库的提交框下面。
        error: Option<String>,
        depth: usize,
        /// 输入框高度。和 `row_h` 一起由调用方算好，虚拟列表的槽位必须和这里一致。
        input_h: f32,
        row_h: f32,
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

/// 窄变更列表里提交框的高度。输入框行数有上限，错误信息在框内滚动，
/// 这样虚拟列表的槽位和实际内容对齐。
fn commit_box_metrics(text: &str, error: Option<&str>, hint: bool) -> (f32, f32) {
    let lines = text.lines().count().clamp(2, 6) as f32;
    let input_h = lines * 18.0 + 16.0;
    let hint_h = if hint { 16.0 } else { 0.0 };
    let err_lines = error
        .map(|err| err.lines().count().clamp(1, 4) as f32)
        .unwrap_or(0.0);
    let err_h = if err_lines > 0.0 {
        8.0 + err_lines * 14.0
    } else {
        0.0
    };
    let row_h = 8.0 + 6.0 + input_h + 28.0 + hint_h + err_h;
    (input_h, row_h)
}

fn narrow_git_row_height(row: &NarrowGitRow) -> f32 {
    match row {
        NarrowGitRow::CommitBox { row_h, .. } => *row_h,
        _ => NARROW_GIT_ROW_H,
    }
}

/// 仓库分组标题前的身份标记。用字符而不是图标：变更栏行高 24px，
/// 再塞图标会把本就窄的一行挤成堆砌。
fn repo_kind_glyph(kind: RepoKind) -> &'static str {
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
    out.extend(tree.into_iter().map(|row| NarrowGitRow::Tree {
        key,
        root: root.clone(),
        is_staged_group,
        depth,
        gitlink: gitlinks.contains(&row.path),
        row,
    }));
}

/// 行 id 用仓库根和路径，不用列表下标。同一个文件可以同时出现在暂存和未暂存，
/// 所以 id 还带上分组 key。下标在刷新或折叠后会指到另一行。
fn git_repo_id(kind: &'static str, root: &str) -> ElementId {
    (ElementId::from(kind), root.to_string()).into()
}

fn git_scoped_id(kind: &'static str, key: &str, root: &str, path: &str) -> ElementId {
    let id = ElementId::from((ElementId::from(kind), key.to_string()));
    let id = ElementId::from((id, root.to_string()));
    ElementId::from((id, path.to_string()))
}

/// 一次会改仓库的 git 进程。同一个仓库根同时只能有一个：`index.lock` 属于这一个仓库。
///
/// 文案在这里，不在调用点拼字符串。释放时必须用同一个值，见 [`release_git_write`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitWrite {
    Commit,
    CommitAndPush,
    Push,
    Fetch,
    Pull,
    Stash,
    StashPop,
    Checkout,
    Merge,
    DeleteBranch,
    DeleteRemoteBranch,
    DiscardFile,
    DiscardChanges,
    Stage,
    Unstage,
    StageHunk,
    UnstageHunk,
    DiscardHunk,
    /// 单文件 add/reset。勾选框的乐观状态另记在 `git_index_pending`，不另占一把锁。
    IndexFile,
}

impl GitWrite {
    /// 同步芯片上的「…中」。
    pub(crate) fn chip(self) -> &'static str {
        match self {
            Self::Commit => "提交中…",
            Self::CommitAndPush => "提交并推送中…",
            Self::Push => "推送中…",
            Self::Fetch => "获取中…",
            Self::Pull => "拉取中…",
            Self::Stash => "储藏中…",
            Self::StashPop => "恢复储藏中…",
            Self::Checkout => "切换分支中…",
            Self::Merge => "合并中…",
            Self::DeleteBranch => "删除分支中…",
            Self::DeleteRemoteBranch => "删除远端分支中…",
            Self::DiscardFile => "丢弃文件中…",
            Self::DiscardChanges => "丢弃更改中…",
            Self::Stage | Self::IndexFile => "暂存中…",
            Self::Unstage => "取消暂存中…",
            Self::StageHunk => "暂存块中…",
            Self::UnstageHunk => "撤出暂存块中…",
            Self::DiscardHunk => "丢弃块中…",
        }
    }
}

/// 占住 `root` 这一个仓库。已经有写入时返回 false，调用方不要再起 git 进程。
pub(crate) fn reserve_git_write(
    writes: &mut HashMap<String, GitWrite>,
    root: &str,
    op: GitWrite,
) -> bool {
    if writes.contains_key(root) {
        return false;
    }
    writes.insert(root.to_string(), op);
    true
}

/// 只释放自己占的那一格。别的操作已经换进去时不动。
pub(crate) fn release_git_write(
    writes: &mut HashMap<String, GitWrite>,
    root: &str,
    op: GitWrite,
) -> bool {
    if writes.get(root) != Some(&op) {
        return false;
    }
    writes.remove(root);
    true
}

/// 树行里的暂存 / 取消暂存。图标档 20，不挤文件名。
fn stage_toggle_button(id: ElementId, staged: bool) -> Button {
    Button::new(id)
        .quiet()
        .icon_size()
        .flex_shrink_0()
        .icon(if staged {
            IconName::Minus
        } else {
            IconName::Plus
        })
        .icon_action(if staged { "取消暂存" } else { "暂存" })
}

/// 提交并推送按钮的文案。
///
/// 只有这次操作自己就是提交或推送时才换成进行中。其它写入按钮不可点，
/// 进行中的名字由这个仓库的同步芯片显示。
pub(crate) fn commit_button_label(op: Option<GitWrite>) -> &'static str {
    match op {
        Some(op @ (GitWrite::Commit | GitWrite::CommitAndPush | GitWrite::Push)) => op.chip(),
        _ => "提交并推送",
    }
}

fn render_narrow_git_row(
    entry: &NarrowGitRow,
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
                .id(git_repo_id("git-repo", root))
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
                    Button::new(git_repo_id("git-repo-caret", root))
                        .quiet()
                        .icon_size()
                        .flex_shrink_0()
                        .icon(if *collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .icon_action(if *collapsed { "展开仓库" } else { "收起仓库" })
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
                        .id(git_repo_id("git-repo-branch", root))
                        .flex_shrink_0()
                        .max_w(px(96.))
                        .truncate()
                        .text_xs()
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
                    Button::new(git_repo_id("git-repo-ops", root))
                            .quiet()
                            .icon_size()
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
            git_write,
            ahead,
            error,
            depth,
            input_h,
            row_h: _,
        } => {
            let (has_text, has_staged, git_write, ahead) =
                (*has_text, *has_staged, *git_write, *ahead);
            let can_commit = has_text && has_staged && git_write.is_none();
            let can_push = ahead > 0 && git_write.is_none();
            let ws_target = ws.clone();
            let root_target = root.to_string();
            let ws_commit = ws.clone();
            let root_commit = root.to_string();
            let ws_push = ws.clone();
            let root_push = root.to_string();
            let commit_label = commit_button_label(git_write);
            v_flex()
                .gap_1p5()
                .pl(px(12. + *depth as f32 * NARROW_GIT_INDENT))
                .pr_2()
                .py_1()
                .child(gpui_component::input::Textarea::new(input).h(px(*input_h)))
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(
                            Button::new(git_repo_id("git-commit-push", root))
                                .commit()
                                .compact()
                                .flex_1()
                                .label(commit_label)
                                .disabled(!can_commit)
                                .on_click(move |_ev, window, cx| {
                                    let root = root_commit.clone();
                                    ws_commit.update(cx, |ws, cx| {
                                        // 按钮点谁就提交谁，不依赖当前选中的是哪个仓库。
                                        ws.set_active_git_repo(root, cx);
                                        ws.commit(true, window, cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new(git_repo_id("git-commit-only", root))
                                .quiet()
                                .compact()
                                .label("仅提交")
                                .disabled(!can_commit)
                                .on_click(move |_ev, window, cx| {
                                    let root = root_target.clone();
                                    ws_target.update(cx, |ws, cx| {
                                        ws.set_active_git_repo(root, cx);
                                        ws.commit(false, window, cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new(git_repo_id("git-push-only", root))
                                .quiet()
                                .compact()
                                .label(format!("↑{ahead}"))
                                // 点击跟可用态同一个条件：↑0 时点了就是一次毫无意义的
                                // push（失败时还会贴出一条报错）。
                                .disabled(!can_push)
                                .on_click(move |_ev, _window, cx| {
                                    let root = root_push.clone();
                                    ws_push.update(cx, |ws, cx| {
                                        ws.set_active_git_repo(root, cx);
                                        ws.push_only(cx);
                                    });
                                }),
                        ),
                )
                .when(has_text && !has_staged, |col| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::text_faint()))
                            .child("先暂存要提交的更改"),
                    )
                })
                // 失败原因贴在按钮正下方：用户的视线本来就在这里，不用去翻通知中心。
                // git 的诊断（hook 报错、缺凭据）常是多行，原样展示才有排查价值。
                .children(error.clone().map(|err| {
                    div()
                        .w_full()
                        .max_h(px(64.))
                        .overflow_hidden()
                        .px_1p5()
                        .py_1()
                        .rounded(ui_theme::row_radius())
                        .bg(rgb(ui_theme::bg_card()))
                        .text_xs()
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
            let header_root = batch
                .as_ref()
                .map(|batch| batch.root.to_string())
                .unwrap_or_default();
            div()
                .id(git_scoped_id("git-header", key, &header_root, title))
                .h(px(NARROW_GIT_ROW_H))
                .flex()
                .items_center()
                .pl(px(12. + *depth as f32 * NARROW_GIT_INDENT))
                .pr_3()
                .text_xs()
                .font_semibold()
                .text_color(rgb(ui_theme::text_faint()))
                .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                .children(batch.as_ref().map(|batch| {
                    let batch = batch.clone();
                    let ws_batch = ws.clone();
                    stage_toggle_button(
                        git_scoped_id("git-group-stage", key, &header_root, title),
                        batch.staged,
                    )
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
                    let batch_action = stage_toggle_button(
                        git_scoped_id("git-dir-stage", key, root, &row.path),
                        batch_staged,
                    )
                    .on_click(move |_ev, _window, cx| {
                        cx.stop_propagation();
                        let root = root_batch.clone();
                        let path = path_batch.clone();
                        ws_batch.update(cx, |workspace, cx| {
                            workspace.stage_paths(root, vec![path], !batch_staged, cx);
                        });
                    });
                    div()
                        .id(git_scoped_id("git-dir", key, root, &row.path))
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
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                        .child(
                            Icon::new(if is_collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(10.))
                            .flex_shrink_0(),
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

                    // 暂存/取消暂存是这一行的主操作，一直显示。
                    // 复选框在这里是错的隐喻：它读起来像“选中若干文件再做点什么”，
                    // 而它其实是立即生效的 git add / git reset。丢弃留在右键菜单。
                    let ws_stage = ws.clone();
                    let root_stage = root.to_string();
                    let path_stage = row.path.clone();
                    // 只在 git 真的还在跑时禁用。等权威 status 确认的那一段必须
                    // 可点：pending 会因为迟到回包、读取失败重试、期间切项目而滞留，
                    // 按"有没有 pending"禁用等于让这个文件的按钮永久失效。
                    let busy = git_index_op_in_flight(pending);
                    let stage_action = stage_toggle_button(
                        git_scoped_id("git-file-stage", key, root, &row.path),
                        staged,
                    )
                    .disabled(busy)
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
                    });

                    let ws_row = ws.clone();
                    let root_row = root.to_string();
                    let path_row = row.path.clone();
                    let ws_menu = ws.clone();
                    let root_menu = root.to_string();
                    let path_menu = row.path.clone();
                    let file_path = smelt_client::runtime_path::PathStyle::LOCAL.join(root, &row.path);
                    let file_path_copy = file_path.clone();
                    let file_path_finder = file_path;
                    let row_el = div()
                        .id(git_scoped_id("git-file", key, root, &row.path))
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
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                        // 子模块指针：改的不是文件内容，而是"父仓记录子仓的哪个 commit"。
                        // 用跟仓库行同一个 ◇ 标记，免得它看上去只是个带 M 的目录。
                        .when(*gitlink, |line| {
                            line.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child(repo_kind_glyph(
                                        RepoKind::Submodule,
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
                                    .text_xs()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child("子模块指针"),
                            )
                        })
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

// ===================== git 子进程调用 =====================

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
