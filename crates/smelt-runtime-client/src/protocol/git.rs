//! git 领域的 wire 类型，以及只依赖这些类型的纯函数（diff 解析、文件树行）。
//!
//! 运行时执行 git（`smelt-git`），客户端只拿这里的结果做展示，不自己起 git 进程。
//! 这里不许出现子进程和文件读写：手机端也要编它。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub prunable: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeListData {
    pub entries: Vec<WorktreeEntry>,
    pub main_root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Del,
    Context,
    Meta,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub old_ln: Option<u32>,
    pub new_ln: Option<u32>,
    pub kind: DiffKind,
    pub text: String,
    pub segments: Option<Vec<(String, bool)>>,
    pub file_header: bool,
}

#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub range: std::ops::Range<usize>,
    pub raw: String,
}

pub struct ParsedDiff {
    pub lines: Vec<DiffLine>,
    pub header: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusData {
    pub ok: bool,
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<(String, String)>,
    pub stash_count: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl GitStatusData {
    pub fn branch_name(&self) -> &str {
        &self.branch
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchList {
    pub local: Vec<String>,
    pub remote: Vec<String>,
}

impl BranchList {
    pub fn local_names(&self) -> &[String] {
        &self.local
    }

    pub fn remote_names(&self) -> &[String] {
        &self.remote
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub git_dir: String,
    pub common_dir: String,
    pub branch: String,
}

impl RepoInfo {
    pub fn is_worktree(&self) -> bool {
        self.git_dir != self.common_dir
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffScope {
    All,
    Staged,
    Unstaged,
}

impl DiffScope {
    pub fn args(self) -> &'static [&'static str] {
        match self {
            // 同 `load_git_status`：不强制 `--ignore-submodules`，跟随仓库配置。
            // diff 与 status 必须用同一套口径，否则列表里有的条目点开是空的。
            Self::All => &["diff", "HEAD", "--"],
            Self::Staged => &["diff", "--staged", "--"],
            Self::Unstaged => &["diff", "--"],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Staged => "已暂存",
            Self::Unstaged => "未暂存",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitTreeRow {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub status: Option<String>,
}

pub fn build_git_tree(files: &[(String, String)], collapsed: &HashSet<String>) -> Vec<GitTreeRow> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: Vec<(String, String)>,
    }

    fn walk(
        node: &Node,
        prefix: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        out: &mut Vec<GitTreeRow>,
    ) {
        for (name, child) in &node.dirs {
            let mut label = name.clone();
            let mut path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mut node = child;
            while node.files.is_empty() && node.dirs.len() == 1 {
                let (next_name, next_node) = node.dirs.iter().next().unwrap();
                label = format!("{label}/{next_name}");
                path = format!("{path}/{next_name}");
                node = next_node;
            }
            let is_collapsed = collapsed.contains(&path);
            out.push(GitTreeRow {
                depth,
                name: label,
                path: path.clone(),
                status: None,
            });
            if !is_collapsed {
                walk(node, &path, depth + 1, collapsed, out);
            }
        }
        for (name, status) in &node.files {
            out.push(GitTreeRow {
                depth,
                name: name.clone(),
                path: if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                },
                status: Some(status.clone()),
            });
        }
    }

    let mut root = Node::default();
    for (status, path) in files {
        let mut parts = path.split('/').collect::<Vec<_>>();
        let Some(name) = parts.pop() else { continue };
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        if !name.is_empty() {
            node.files.push((name.to_string(), status.clone()));
        }
    }

    let mut rows = Vec::new();
    walk(&root, "", 0, collapsed, &mut rows);
    rows
}

pub fn full_file_path(root: &str, path: &str, aggregate: bool) -> Option<String> {
    (!aggregate).then(|| Path::new(root).join(path).to_string_lossy().into_owned())
}

pub fn hunk_patch(header: &str, hunk: &DiffHunk) -> String {
    format!("{header}{}", hunk.raw)
}

pub fn repo_label_from_common_dir(common_dir: &str) -> Option<String> {
    Path::new(common_dir)
        .parent()?
        .file_name()?
        .to_str()
        .map(String::from)
}

pub fn main_repo_root_from_common_dir(common_dir: &str) -> Option<String> {
    Path::new(common_dir).parent()?.to_str().map(String::from)
}

/// 仓库在工作区里的身份。UI 用它区分图标与分组标题，写操作不看它——
/// 任何 kind 的仓库都是独立的提交单位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    /// 项目根自己。
    Root,
    /// 父仓 `.gitmodules` 声明的 submodule。
    Submodule,
    /// 落在项目根内、但不是 submodule 的独立仓库（含被 `.gitignore` 忽略的，
    /// 以及检出在项目根内的 worktree）。
    Nested,
}

/// 一个已确认的仓库。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredRepo {
    /// 仓库真实根（`git rev-parse --show-toplevel` 的结果，已 canonicalize）。
    pub root: PathBuf,
    /// 相对项目根的路径。项目根自己是空串。仅用于展示与排序。
    pub rel_path: String,
    pub kind: RepoKind,
}

/// 发现结果。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    /// 项目根排第一，其余按相对路径字典序。
    pub repos: Vec<DiscoveredRepo>,
    /// 是否因为 `max_repos` 截断过。UI 要据此提示用户，而不是假装列全了。
    pub truncated: bool,
}

/// 一条提交记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitNode {
    /// 完整 sha，父子关系全靠它对齐。
    pub hash: String,
    /// 短 sha，显示用。
    pub short: String,
    pub author: String,
    /// author date，Unix 秒。
    pub time: i64,
    /// 提交标题（第一行）。
    pub subject: String,
    /// 父提交完整 sha。第一个是主线父，其余是被合并进来的。
    pub parents: Vec<String>,
    /// 指向本提交的引用名（分支 / tag），已剥掉 `HEAD -> ` 之类前缀。
    pub refs: Vec<String>,
}

/// 日志看哪一段历史。
///
/// 默认是 [`LogScope::Head`]——**当前分支**，跟 JetBrains 一致。一进来就 `--all`
/// 会把所有分支的提交混在一起按时间排，跟「我这条线上都干了啥」完全是两回事。
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogScope {
    /// 当前检出的分支（`git log` 不带 ref，默认就是 HEAD）。
    #[default]
    Head,
    /// 所有分支（`--all`），看完整拓扑用。
    All,
    /// 指定分支。
    Branch(String),
}

/// `git log` 的输出格式：用 \x00 分隔字段、\x1e 分隔记录，免得提交标题里的
/// 空格换行把解析搞乱。
pub const LOG_FORMAT: &str = "--pretty=format:%H%x00%h%x00%an%x00%at%x00%s%x00%P%x00%D%x1e";

/// 解析 `git log` 的输出（格式见 [`LOG_FORMAT`]）。
pub fn parse_log(text: &str) -> Vec<CommitNode> {
    text.split('\x1e')
        .map(str::trim_start) // 记录间的换行
        .filter(|r| !r.is_empty())
        .filter_map(|rec| {
            let mut f = rec.split('\x00');
            let hash = f.next()?.to_string();
            let short = f.next()?.to_string();
            let author = f.next()?.to_string();
            let time = f.next()?.parse().unwrap_or(0);
            let subject = f.next()?.to_string();
            let parents = f
                .next()?
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            // %D 形如 "HEAD -> main, origin/main, tag: v1.0"。
            // 两个前缀都剥掉：`HEAD -> ` 只是说明当前检出的是谁，`tag: ` 在列表里
            // 是纯噪音（CI 生成的 tag 名本来就长，再顶个前缀更挤）。
            let refs = f
                .next()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.rsplit(" -> ").next().unwrap_or(s))
                .map(|s| s.strip_prefix("tag: ").unwrap_or(s).to_string())
                .collect();
            Some(CommitNode {
                hash,
                short,
                author,
                time,
                subject,
                parents,
                refs,
            })
        })
        .collect()
}

/// 解析 `git show --name-status` 的文件部分：每行 `M\tpath` / `A\tpath` /
/// `R100\told\tnew`（重命名取新名）。
pub fn parse_name_status(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let st = f.next()?.trim();
            if st.is_empty() {
                return None;
            }
            // 重命名/复制有两个路径，取最后一个（新路径）。
            let path = f.next_back()?.trim();
            if path.is_empty() {
                return None;
            }
            // 只取首字母：R100 → R
            Some((st.chars().next()?.to_string(), path.to_string()))
        })
        .collect()
}

pub fn parse_diff(text: &str) -> ParsedDiff {
    parse_diff_inner(text, false)
}

pub fn parse_diff_with_file_headers(text: &str) -> ParsedDiff {
    parse_diff_inner(text, true)
}

fn parse_diff_inner(text: &str, show_file_headers: bool) -> ParsedDiff {
    let make = |old_ln, new_ln, kind, text: &str| DiffLine {
        old_ln,
        new_ln,
        kind,
        text: text.to_string(),
        segments: None,
        file_header: false,
    };
    if text.trim().is_empty() {
        return ParsedDiff {
            lines: vec![make(None, None, DiffKind::Meta, "（无差异）")],
            header: String::new(),
            hunks: Vec::new(),
        };
    }

    let mut old_ln = 0;
    let mut new_ln = 0;
    let mut lines = Vec::new();
    let mut header = String::new();
    let mut hunks: Vec<DiffHunk> = Vec::new();
    for line in text.lines() {
        if let Some(hunk) = hunks.last_mut()
            && hunk.range.end == usize::MAX
        {
            if line.starts_with("@@") || line.starts_with("diff ") || line.starts_with("Submodule ")
            {
                hunk.range.end = lines.len();
            } else {
                hunk.raw.push_str(line);
                hunk.raw.push('\n');
            }
        }
        if line.starts_with("@@") {
            (old_ln, new_ln) = parse_hunk(line);
            hunks.push(DiffHunk {
                range: lines.len()..usize::MAX,
                raw: format!("{line}\n"),
            });
        } else if let Some(path) = submodule_diff_path(line) {
            if show_file_headers {
                let mut title = make(None, None, DiffKind::Meta, path);
                title.file_header = true;
                lines.push(title);
            }
            lines.push(make(None, None, DiffKind::Meta, line));
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("similarity")
            || line.starts_with("rename ")
        {
            if hunks.is_empty() {
                header.push_str(line);
                header.push('\n');
            }
            if show_file_headers && line.starts_with("diff --git ") {
                let path = line
                    .split_whitespace()
                    .nth(3)
                    .and_then(|path| path.strip_prefix("b/"))
                    .unwrap_or(line);
                let mut title = make(None, None, DiffKind::Meta, path);
                title.file_header = true;
                lines.push(title);
            }
            let noise = line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line == "--- /dev/null"
                || line == "+++ /dev/null";
            if !noise {
                lines.push(make(None, None, DiffKind::Meta, line));
            }
        } else if let Some(text) = line.strip_prefix('+') {
            lines.push(make(None, Some(new_ln), DiffKind::Add, text));
            new_ln += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            lines.push(make(Some(old_ln), None, DiffKind::Del, text));
            old_ln += 1;
        } else {
            let text = line.strip_prefix(' ').unwrap_or(line);
            lines.push(make(Some(old_ln), Some(new_ln), DiffKind::Context, text));
            old_ln += 1;
            new_ln += 1;
        }
    }
    if let Some(hunk) = hunks.last_mut()
        && hunk.range.end == usize::MAX
    {
        hunk.range.end = lines.len();
    }
    mark_inline(&mut lines);
    ParsedDiff {
        lines,
        header,
        hunks,
    }
}

/// `git diff --submodule` 短格式：`Submodule path abc..def:` 或
/// `Submodule path contains modified content`。没有 `diff --git` 头，
/// 全部改动列表必须单独认，否则子模块改动在聚合视图里会消失。
fn submodule_diff_path(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("Submodule ")?;
    if let Some((path, _)) = rest.split_once(" contains ") {
        let path = path.trim();
        return (!path.is_empty()).then_some(path);
    }
    let (path, tail) = rest.rsplit_once(' ')?;
    let tail = tail.trim_end_matches(':');
    let looks_like_sha_range = tail.contains("..") || tail.chars().all(|ch| ch.is_ascii_hexdigit());
    looks_like_sha_range
        .then_some(path.trim())
        .filter(|path| !path.is_empty())
}

fn parse_hunk(line: &str) -> (u32, u32) {
    let mut old = 0;
    let mut new = 0;
    for token in line.split_whitespace() {
        if let Some(value) = token.strip_prefix('-') {
            old = value
                .split(',')
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
        } else if let Some(value) = token.strip_prefix('+') {
            new = value
                .split(',')
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
        }
    }
    (old, new)
}

fn mark_inline(lines: &mut [DiffLine]) {
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != DiffKind::Del {
            index += 1;
            continue;
        }
        let deleted = index;
        while index < lines.len() && lines[index].kind == DiffKind::Del {
            index += 1;
        }
        let added = index;
        while index < lines.len() && lines[index].kind == DiffKind::Add {
            index += 1;
        }
        for offset in 0..(added - deleted).min(index - added) {
            let (delete_index, add_index) = (deleted + offset, added + offset);
            let (old, new) = (
                lines[delete_index].text.clone(),
                lines[add_index].text.clone(),
            );
            if old.len() + new.len() <= 4000 {
                let (old_segments, new_segments) = inline_segments(&old, &new);
                lines[delete_index].segments = Some(old_segments);
                lines[add_index].segments = Some(new_segments);
            }
        }
    }
}

type InlineSegments = Vec<(String, bool)>;

fn inline_segments(old: &str, new: &str) -> (InlineSegments, InlineSegments) {
    let diff = similar::TextDiff::from_chars(old, new);
    let mut old_segments = InlineSegments::new();
    let mut new_segments = InlineSegments::new();
    let push = |segments: &mut InlineSegments, value: &str, changed: bool| {
        if let Some(last) = segments.last_mut()
            && last.1 == changed
        {
            last.0.push_str(value);
            return;
        }
        segments.push((value.to_string(), changed));
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Equal => {
                push(&mut old_segments, change.value(), false);
                push(&mut new_segments, change.value(), false);
            }
            similar::ChangeTag::Delete => push(&mut old_segments, change.value(), true),
            similar::ChangeTag::Insert => push(&mut new_segments, change.value(), true),
        }
    }
    (old_segments, new_segments)
}

// ===================== 控制方法 =====================
//
// 粒度按「一次界面动作一次往返」定：一个 status、一份 diff 在运行时里可能要跑好几条
// git 命令，客户端不关心。失败一律是 `operation_failed`，message 是给人看的原因
// （通常就是 git 的 stderr）。

macro_rules! git_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

git_calls! {
    /// 项目根里有哪些仓库（项目根自己、子模块、嵌套仓库）。
    GitDiscoverCall => GitDiscover(GitRootParams) -> Discovery;
    /// 仓库身份：git-dir、common-dir、当前分支。非 git 目录返回 `None`。
    GitRepoInfoCall => GitRepoInfo(GitRootParams) -> GitRepoInfoResult;
    GitStatusCall => GitStatus(GitRootParams) -> GitStatusData;
    GitBranchesCall => GitBranches(GitRootParams) -> BranchList;
    /// 工作区有没有未提交改动（删除 worktree 前的提醒用）。
    GitIsDirtyCall => GitIsDirty(GitRootParams) -> GitIsDirtyResult;
    GitCheckoutCall => GitCheckout(GitBranchParams) -> EmptyParams;
    GitBranchDeleteCall => GitBranchDelete(GitBranchDeleteParams) -> EmptyParams;
    GitMergeCall => GitMerge(GitBranchParams) -> EmptyParams;
    /// `git add -- <paths>`。
    GitStageCall => GitStage(GitPathsParams) -> EmptyParams;
    /// `git reset -- <paths>`。
    GitUnstageCall => GitUnstage(GitPathsParams) -> EmptyParams;
    /// 丢弃一个文件的全部改动：已跟踪的还原暂存区和工作区，未跟踪的直接删掉。
    GitDiscardFileCall => GitDiscardFile(GitDiscardFileParams) -> EmptyParams;
    /// 按块暂存、取消暂存或丢弃。
    GitApplyPatchCall => GitApplyPatch(GitApplyPatchParams) -> EmptyParams;
    GitDiscardAllCall => GitDiscardAll(GitRootParams) -> EmptyParams;
    GitCommitCall => GitCommit(GitCommitParams) -> EmptyParams;
    GitPushCall => GitPush(GitBranchParams) -> EmptyParams;
    GitFetchCall => GitFetch(GitRootParams) -> EmptyParams;
    GitPullCall => GitPull(GitRootParams) -> EmptyParams;
    GitStashPushCall => GitStashPush(GitRootParams) -> EmptyParams;
    GitStashPopCall => GitStashPop(GitRootParams) -> EmptyParams;
    GitWorktreeListCall => GitWorktreeList(GitRootParams) -> WorktreeListData;
    /// 建 worktree，并按用户设置把主仓库的未跟踪配置链接过去。返回检出目录。
    GitWorktreeCreateCall => GitWorktreeCreate(GitWorktreeCreateParams) -> GitWorktreeCreateResult;
    GitWorktreeRemoveCall => GitWorktreeRemove(GitWorktreeRemoveParams) -> EmptyParams;
    GitWorktreePruneCall => GitWorktreePrune(GitRootParams) -> EmptyParams;
    /// 单个文件的 diff 原文，以及展示它要用到的几项事实。
    GitFileDiffCall => GitFileDiff(GitFileDiffParams) -> GitFileDiffResult;
    /// 一个仓库在某个范围里的全部改动，未跟踪文件也拼进去。
    GitAllDiffCall => GitAllDiff(GitAllDiffParams) -> GitTextResult;
    GitLogCall => GitLog(GitLogParams) -> GitLogResult;
    /// 一个提交改了哪些文件：`(状态首字母, 路径)`。
    GitCommitFilesCall => GitCommitFiles(GitCommitRefParams) -> GitCommitFilesResult;
    /// 一个文件在某个提交里的 diff 原文。
    GitCommitFileDiffCall => GitCommitFileDiff(GitCommitFileParams) -> GitTextResult;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRootParams {
    pub root: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchParams {
    pub root: String,
    pub branch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchDeleteParams {
    pub root: String,
    pub branch: String,
    /// 远程分支（`origin/x`）走 `git push --delete`，本地分支走 `git branch -D`。
    pub remote: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPathsParams {
    pub root: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiscardFileParams {
    pub root: String,
    pub path: String,
    pub untracked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPatchAction {
    /// `git apply --cached`：把这一块放进暂存区。
    Stage,
    /// `git apply --cached --reverse`：把这一块撤出暂存区。
    Unstage,
    /// `git apply --reverse`：从工作区文件里丢掉这一块。会真的改文件。
    Discard,
}

impl GitPatchAction {
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::Stage => &["apply", "--cached", "-"],
            Self::Unstage => &["apply", "--cached", "--reverse", "-"],
            Self::Discard => &["apply", "--reverse", "-"],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitApplyPatchParams {
    pub root: String,
    pub action: GitPatchAction,
    pub patch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitParams {
    pub root: String,
    pub message: String,
    pub push: bool,
    /// 推送用的当前分支名；首次推送时据此设上游。
    pub branch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeCreateParams {
    /// 主仓库根。
    pub root: String,
    /// 空表示用目录名新建分支。
    pub branch: Option<String>,
    /// 检出目录，相对主仓库根或绝对路径。
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeCreateResult {
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeRemoveParams {
    /// 主仓库根。`git worktree remove` 不能从待删目录自己发起。
    pub root: String,
    pub path: String,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileDiffParams {
    pub root: String,
    pub path: String,
    pub untracked: bool,
    pub scope: DiffScope,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileDiffResult {
    /// diff 原文。git 跑不起来时是一句说明，界面照样显示它。
    pub text: String,
    /// 这个文件是否有已暂存的部分。
    pub has_staged: bool,
    /// 这是子模块指针的变化，不能按块操作。
    pub is_gitlink: bool,
    /// 工作区里对应的普通文件（能在编辑器里打开）；目录、子模块、已删除时为 `None`。
    pub worktree_file: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAllDiffParams {
    pub root: String,
    pub scope: DiffScope,
    /// 要拼进去的未跟踪路径。客户端从它手上的 status 里取，免得两次 status 对不上。
    pub untracked_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitTextResult {
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepoInfoResult {
    pub info: Option<RepoInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIsDirtyResult {
    pub dirty: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLogParams {
    pub root: String,
    pub scope: LogScope,
    pub limit: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLogResult {
    pub commits: Vec<CommitNode>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitRefParams {
    pub root: String,
    pub hash: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitFileParams {
    pub root: String,
    pub hash: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitFilesResult {
    pub files: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submodule_short_diff_gets_a_file_header() {
        let parsed = parse_diff_with_file_headers(
            "Submodule vendor/lib 1234567..89abcde:\n  > Fix the bug\n",
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.file_header && line.text == "vendor/lib"),
            "子模块 SHA 变化必须出现在全部改动里：{:?}",
            parsed
                .lines
                .iter()
                .map(|line| (line.file_header, line.text.as_str()))
                .collect::<Vec<_>>()
        );
        let parsed =
            parse_diff_with_file_headers("Submodule skills/fx-deploy contains modified content\n");
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.file_header && line.text == "skills/fx-deploy")
        );
        let parsed = parse_diff_with_file_headers(
            "diff --git a/skills/fx-deploy b/skills/fx-deploy\nnew file mode 000000\n--- /dev/null\n+++ b/skills/fx-deploy\n",
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.file_header && line.text == "skills/fx-deploy")
        );
    }

    #[test]
    fn diff_parser_keeps_patch_material() {
        let parsed = parse_diff(
            "diff --git a/f b/f\nindex 111..222 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n",
        );
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.hunks.len(), 1);
        assert!(parsed.header.starts_with("diff --git"));
        assert!(hunk_patch(&parsed.header, &parsed.hunks[0]).contains("@@ -1 +1 @@"));
    }

    #[test]
    fn parses_log_records_including_refs_and_parents() {
        let text = "aaa\u{0}aa\u{0}Zoey\u{0}1700000000\u{0}feat: 加个东西\u{0}bbb ccc\u{0}HEAD -> main, origin/main, tag: v1.0\u{1e}\
                    bbb\u{0}bb\u{0}Matt\u{0}1699999999\u{0}fix: 修一下\u{0}\u{0}\u{1e}";
        let got = parse_log(text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].author, "Zoey");
        assert_eq!(got[0].subject, "feat: 加个东西");
        assert_eq!(got[0].parents, vec!["bbb", "ccc"], "两个父说明是 merge");
        assert_eq!(
            got[0].refs,
            vec!["main", "origin/main", "v1.0"],
            "`HEAD -> ` 和 `tag: ` 两个前缀都该被剥掉"
        );
        assert_eq!(got[1].time, 1699999999);
        assert!(got[1].parents.is_empty(), "根提交没有父");
    }

    #[test]
    fn parses_name_status_including_renames() {
        let text = "M\tsrc/main.rs\nA\tsrc/new.rs\nD\tsrc/old.rs\nR100\tsrc/a.rs\tsrc/b.rs\n";
        let got = parse_name_status(text);
        assert_eq!(
            got,
            vec![
                ("M".to_string(), "src/main.rs".to_string()),
                ("A".to_string(), "src/new.rs".to_string()),
                ("D".to_string(), "src/old.rs".to_string()),
                // 重命名取新路径，状态取首字母
                ("R".to_string(), "src/b.rs".to_string()),
            ]
        );
    }
}
