//! Git 领域层：仓库发现、状态解析、diff 与写操作。不含 UI，不含面板状态。
//!
//! 所有命令消费者都有 provider-aware 变体，调用方可以换一个 `Subprocess` 实现
//! 把同一套逻辑跑到别的执行世界（远程 / 沙箱）里；便捷包装让本地调用点保持简短。

pub mod discovery;

pub use smelt_core::subprocess::{LocalSubprocess, Output, Subprocess};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub type GitOutput = Output;

pub fn run_git_with(
    subprocess: &dyn Subprocess,
    root: &str,
    args: &[&str],
) -> std::io::Result<GitOutput> {
    smelt_core::subprocess::run_git(subprocess, Path::new(root), args)
}

pub fn run_git(root: &str, args: &[&str]) -> std::io::Result<GitOutput> {
    run_git_with(&LocalSubprocess, root, args)
}

pub fn run_git_stdin_with(
    subprocess: &dyn Subprocess,
    root: &str,
    args: &[&str],
    input: &str,
) -> std::io::Result<GitOutput> {
    smelt_core::subprocess::run_git_stdin(subprocess, Path::new(root), args, input)
}

pub fn run_git_stdin(root: &str, args: &[&str], input: &str) -> std::io::Result<GitOutput> {
    run_git_stdin_with(&LocalSubprocess, root, args, input)
}

pub fn git_err(out: &GitOutput, fallback: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if stderr.is_empty() {
        fallback.to_string()
    } else {
        stderr
    }
}

#[derive(Clone, Debug)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub prunable: bool,
    pub locked: bool,
}

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
}

pub struct ParsedDiff {
    pub lines: Vec<DiffLine>,
    pub header: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Default)]
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

#[derive(Clone, Default)]
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

#[derive(Clone)]
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

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
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

pub fn parse_branch_status_line(b: &str) -> (String, Option<String>, u32, u32) {
    let Some((head, rest)) = b.split_once("...") else {
        return (b.trim().to_string(), None, 0, 0);
    };
    let (upstream, bracket) = match rest.split_once(" [") {
        Some((upstream, tail)) => (
            upstream.trim().to_string(),
            Some(tail.trim_end_matches(']')),
        ),
        None => (rest.trim().to_string(), None),
    };
    let mut ahead = 0;
    let mut behind = 0;
    if let Some(bracket) = bracket {
        for part in bracket.split(", ") {
            if let Some(value) = part.strip_prefix("ahead ") {
                ahead = value.trim().parse().unwrap_or(0);
            } else if let Some(value) = part.strip_prefix("behind ") {
                behind = value.trim().parse().unwrap_or(0);
            }
        }
    }
    (head.trim().to_string(), Some(upstream), ahead, behind)
}

pub fn parse_git_status(text: &str) -> GitStatusData {
    let mut status = GitStatusData {
        ok: true,
        ..Default::default()
    };
    for line in text.lines() {
        if let Some(branch) = line.strip_prefix("## ") {
            let (name, upstream, ahead, behind) = parse_branch_status_line(branch);
            status.branch = name;
            status.upstream = upstream;
            status.ahead = ahead;
            status.behind = behind;
        } else if line.len() >= 3 {
            // 未跟踪目录 / 嵌套 git 仓库（常被当成 submodule）porcelain 会写成
            // `?? path/`。文件树按 `/` 切最后一段，尾斜杠会得到空文件名直接丢掉。
            let path = line[3..].trim_end_matches('/').to_string();
            if !path.is_empty() {
                status.files.push((line[..2].to_string(), path));
            }
        }
    }
    status
}

fn stash_count_with(subprocess: &dyn Subprocess, root: &str) -> u32 {
    run_git_with(subprocess, root, &["stash", "list"])
        .ok()
        .filter(Output::success)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| !line.is_empty())
                .count() as u32
        })
        .unwrap_or(0)
}

fn diff_shortstat_with(subprocess: &dyn Subprocess, root: &str) -> (u32, u32) {
    let Some(out) = run_git_with(subprocess, root, &["diff", "HEAD", "--shortstat"])
        .ok()
        .filter(Output::success)
    else {
        return (0, 0);
    };
    let mut insertions = 0;
    let mut deletions = 0;
    for part in String::from_utf8_lossy(&out.stdout).trim().split(',') {
        let part = part.trim();
        if let Some(value) = part
            .strip_suffix("insertion(+)")
            .or_else(|| part.strip_suffix("insertions(+)"))
        {
            insertions = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = part
            .strip_suffix("deletion(-)")
            .or_else(|| part.strip_suffix("deletions(-)"))
        {
            deletions = value.trim().parse().unwrap_or(0);
        }
    }
    (insertions, deletions)
}

pub fn load_git_status_with(subprocess: &dyn Subprocess, root: &str) -> GitStatusData {
    let Ok(out) = run_git_with(
        subprocess,
        root,
        // 不传 `--ignore-submodules`：让 git 按仓库自己的配置（`.gitmodules` 的
        // `ignore`、`submodule.<name>.ignore`）决定报不报 submodule。
        //
        // 以前强制 `none` 是「靠父仓 status 发现子仓」那套模型的遗留：不强制就一个
        // 子仓都找不到。现在仓库由 discovery 独立发现，强制只剩副作用——用户明明
        // 配了 `ignore = all`，父仓还是塞进来一条 gitlink，跟子仓自己的分组重复。
        &["status", "--porcelain=v1", "--untracked-files=all", "-b"],
    ) else {
        return GitStatusData::default();
    };
    if !out.success() {
        return GitStatusData::default();
    }
    let mut status = parse_git_status(&String::from_utf8_lossy(&out.stdout));
    status.stash_count = stash_count_with(subprocess, root);
    (status.insertions, status.deletions) = diff_shortstat_with(subprocess, root);
    status
}

pub fn load_git_status(root: &str) -> GitStatusData {
    load_git_status_with(&LocalSubprocess, root)
}

pub fn load_branches_with(subprocess: &dyn Subprocess, root: &str) -> BranchList {
    let Ok(out) = run_git_with(
        subprocess,
        root,
        &[
            "for-each-ref",
            "refs/heads",
            "refs/remotes",
            "--format=%(refname)",
        ],
    ) else {
        return BranchList::default();
    };
    if !out.success() {
        return BranchList::default();
    }
    let mut branches = BranchList::default();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            branches.local.push(name.to_string());
        } else if let Some(name) = line.strip_prefix("refs/remotes/")
            && !name.ends_with("/HEAD")
        {
            branches.remote.push(name.to_string());
        }
    }
    branches
}

pub fn load_branches(root: &str) -> BranchList {
    load_branches_with(&LocalSubprocess, root)
}

pub fn load_repo_info_with(subprocess: &dyn Subprocess, root: &str) -> Option<RepoInfo> {
    let out = run_git_with(
        subprocess,
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--abbrev-ref",
            "HEAD",
        ],
    )
    .ok()?;
    if !out.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    Some(RepoInfo {
        git_dir: lines.next()?.to_string(),
        common_dir: lines.next()?.to_string(),
        branch: lines.next().unwrap_or("HEAD").to_string(),
    })
}

pub fn load_repo_info(root: &str) -> Option<RepoInfo> {
    load_repo_info_with(&LocalSubprocess, root)
}

pub fn parse_worktree_list(stdout: &str, main_root_path: Option<&str>) -> Vec<WorktreeEntry> {
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            entries.push(WorktreeEntry {
                path: path.to_string(),
                branch: None,
                is_main: false,
                prunable: false,
                locked: false,
            });
        } else if let Some(entry) = entries.last_mut() {
            if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                entry.branch = Some(branch.to_string());
            } else if line.starts_with("prunable") {
                entry.prunable = true;
            } else if line.starts_with("locked") {
                entry.locked = true;
            }
        }
    }
    for entry in &mut entries {
        entry.is_main = main_root_path == Some(entry.path.as_str());
    }
    entries
}

pub fn list_worktrees_with(
    subprocess: &dyn Subprocess,
    root: &str,
) -> Result<WorktreeListData, String> {
    let common = run_git_with(
        subprocess,
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|error| error.to_string())?;
    if !common.success() {
        return Err(git_err(&common, "读取 git common dir 失败"));
    }
    let common_dir = String::from_utf8_lossy(&common.stdout).trim().to_string();
    let main_root = main_repo_root_from_common_dir(&common_dir).unwrap_or_else(|| root.to_string());

    let out = run_git_with(subprocess, root, &["worktree", "list", "--porcelain"])
        .map_err(|error| error.to_string())?;
    if !out.success() {
        return Err(git_err(&out, "git worktree list 失败"));
    }
    let top = run_git_with(subprocess, root, &["rev-parse", "--show-toplevel"])
        .ok()
        .filter(Output::success)
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    Ok(WorktreeListData {
        entries: parse_worktree_list(&String::from_utf8_lossy(&out.stdout), top.as_deref()),
        main_root,
    })
}

pub fn list_worktrees(root: &str) -> Result<WorktreeListData, String> {
    list_worktrees_with(&LocalSubprocess, root)
}

pub fn remove_worktree_with(
    subprocess: &dyn Subprocess,
    main_root: &str,
    path: &str,
    force: bool,
) -> Result<(), String> {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path);
    checked(
        run_git_with(subprocess, main_root, &args),
        "git worktree remove 失败",
    )
}

pub fn remove_worktree(main_root: &str, path: &str, force: bool) -> Result<(), String> {
    remove_worktree_with(&LocalSubprocess, main_root, path, force)
}

pub fn prune_stale_worktrees_with(
    subprocess: &dyn Subprocess,
    main_root: &str,
) -> Result<(), String> {
    checked(
        run_git_with(subprocess, main_root, &["worktree", "prune"]),
        "git worktree prune 失败",
    )
}

pub fn prune_stale_worktrees(main_root: &str) -> Result<(), String> {
    prune_stale_worktrees_with(&LocalSubprocess, main_root)
}

pub fn create_worktree_with(
    subprocess: &dyn Subprocess,
    main_root: &str,
    branch: Option<&str>,
    path: &str,
) -> Result<String, String> {
    let mut args = vec!["worktree", "add"];
    match branch.filter(|branch| !branch.trim().is_empty()) {
        Some(branch) => {
            let reference = format!("refs/heads/{branch}");
            let exists = run_git_with(
                subprocess,
                main_root,
                &["show-ref", "--verify", "--quiet", &reference],
            )
            .map_err(|error| error.to_string())?;
            if exists.success() {
                let list =
                    run_git_with(subprocess, main_root, &["worktree", "list", "--porcelain"])
                        .map_err(|error| error.to_string())?;
                let occupied = String::from_utf8_lossy(&list.stdout)
                    .lines()
                    .any(|line| line.strip_prefix("branch refs/heads/") == Some(branch));
                if occupied {
                    return Err(format!(
                        "分支「{branch}」已被其他 worktree 检出，不能重复检出"
                    ));
                }
                args.extend([path, branch]);
            } else {
                args.extend(["-b", branch, path, "HEAD"]);
            }
        }
        None => args.extend(["--detach", path]),
    }
    let out = run_git_with(subprocess, main_root, &args).map_err(|error| error.to_string())?;
    if out.success() {
        Ok(path.to_string())
    } else {
        Err(git_err(&out, "git worktree add 失败"))
    }
}

pub fn create_worktree(
    main_root: &str,
    branch: Option<&str>,
    path: &str,
) -> Result<String, String> {
    create_worktree_with(&LocalSubprocess, main_root, branch, path)
}

fn checked(result: std::io::Result<Output>, fallback: &str) -> Result<(), String> {
    let out = result.map_err(|error| error.to_string())?;
    if out.success() {
        Ok(())
    } else {
        Err(git_err(&out, fallback))
    }
}

fn run_git_net_with(subprocess: &dyn Subprocess, root: &str, args: &[&str]) -> Result<(), String> {
    let mut full_args = vec!["-C", root];
    full_args.extend_from_slice(args);
    checked(
        subprocess.run(
            "git",
            &full_args,
            None,
            &[("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")],
        ),
        "git 命令失败",
    )
}

pub fn push_current_with(
    subprocess: &dyn Subprocess,
    root: &str,
    branch: &str,
) -> Result<(), String> {
    let env = [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")];
    let first = subprocess
        .run("git", &["-C", root, "push"], None, &env)
        .map_err(|error| error.to_string())?;
    if first.success() {
        return Ok(());
    }
    if branch.is_empty() {
        return Err(git_err(&first, "git push 失败"));
    }
    checked(
        subprocess.run(
            "git",
            &["-C", root, "push", "-u", "origin", branch],
            None,
            &env,
        ),
        "git push 失败",
    )
}

pub fn push_current(root: &str, branch: &str) -> Result<(), String> {
    push_current_with(&LocalSubprocess, root, branch)
}

pub fn commit_and_maybe_push_with(
    subprocess: &dyn Subprocess,
    root: &str,
    message: &str,
    push: bool,
    branch: &str,
) -> Result<(), String> {
    let commit = run_git_with(subprocess, root, &["commit", "-m", message])
        .map_err(|error| error.to_string())?;
    if !commit.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&commit.stdout).trim().to_string();
        return Err(if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "git commit 失败".to_string()
        });
    }
    if push {
        push_current_with(subprocess, root, branch)
    } else {
        Ok(())
    }
}

pub fn commit_and_maybe_push(
    root: &str,
    message: &str,
    push: bool,
    branch: &str,
) -> Result<(), String> {
    commit_and_maybe_push_with(&LocalSubprocess, root, message, push, branch)
}

pub fn fetch_remote_with(subprocess: &dyn Subprocess, root: &str) -> Result<(), String> {
    run_git_net_with(subprocess, root, &["fetch", "--all", "--prune"])
}

pub fn fetch_remote(root: &str) -> Result<(), String> {
    fetch_remote_with(&LocalSubprocess, root)
}

pub fn pull_rebase_with(subprocess: &dyn Subprocess, root: &str) -> Result<(), String> {
    run_git_net_with(subprocess, root, &["pull", "--rebase"])
}

pub fn pull_rebase(root: &str) -> Result<(), String> {
    pull_rebase_with(&LocalSubprocess, root)
}

pub fn stash_push_with(subprocess: &dyn Subprocess, root: &str) -> Result<(), String> {
    checked(
        run_git_with(subprocess, root, &["stash", "push", "-u"]),
        "git stash 失败",
    )
}

pub fn stash_push(root: &str) -> Result<(), String> {
    stash_push_with(&LocalSubprocess, root)
}

pub fn stash_pop_with(subprocess: &dyn Subprocess, root: &str) -> Result<(), String> {
    checked(
        run_git_with(subprocess, root, &["stash", "pop"]),
        "git stash pop 失败",
    )
}

pub fn stash_pop(root: &str) -> Result<(), String> {
    stash_pop_with(&LocalSubprocess, root)
}

pub fn discard_all_with(subprocess: &dyn Subprocess, root: &str) -> Result<(), String> {
    checked(
        run_git_with(
            subprocess,
            root,
            &["restore", "--staged", "--worktree", "."],
        ),
        "git restore 失败",
    )?;
    checked(
        run_git_with(subprocess, root, &["clean", "-fd"]),
        "git clean 失败",
    )
}

pub fn discard_all(root: &str) -> Result<(), String> {
    discard_all_with(&LocalSubprocess, root)
}

pub fn checkout_branch(root: &str, branch: &str) -> Result<(), String> {
    checked(run_git(root, &["checkout", branch]), "git checkout 失败")
}

pub fn delete_branch(root: &str, branch: &str, remote: bool) -> Result<(), String> {
    if !remote {
        return checked(run_git(root, &["branch", "-d", branch]), "删除分支失败");
    }
    let (remote_name, ref_name) = branch.split_once('/').unwrap_or(("origin", branch));
    checked(
        LocalSubprocess.run(
            "git",
            &["-C", root, "push", remote_name, "--delete", ref_name],
            None,
            &[("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")],
        ),
        "删除分支失败",
    )
}

pub fn merge_branch(root: &str, branch: &str) -> Result<(), String> {
    let out = run_git(root, &["merge", "--no-ff", branch]).map_err(|error| error.to_string())?;
    if out.success() {
        Ok(())
    } else {
        let raw = git_err(&out, "git merge 失败");
        Err(format!(
            "{raw}\n（冲突文件已留在工作区，解决后 git add 再 git commit；\
             想放弃这次合并用 git merge --abort）"
        ))
    }
}

/// `path` 是相对 `root` 这个仓库的路径。调用方拿到文件时就知道它属于哪个仓库，
/// 这里不再按路径反推——反推正是暂存落到子仓、提交却回到父仓的根源。
pub fn stage_file(root: &str, path: &str) -> Result<(), String> {
    checked(run_git(root, &["add", "--", path]), "git add 失败")
}

pub fn unstage_file(root: &str, path: &str) -> Result<(), String> {
    checked(run_git(root, &["reset", "--", path]), "git reset 失败")
}

pub fn apply_patch(root: &str, args: &[&str], patch: &str) -> Result<(), String> {
    checked(run_git_stdin(root, args, patch), "git apply 失败")
}

pub fn parse_diff(text: &str) -> ParsedDiff {
    parse_diff_inner(text, false)
}

pub fn parse_diff_with_file_headers(text: &str) -> ParsedDiff {
    parse_diff_inner(text, true)
}

/// 未跟踪目录 / 嵌套 git 仓库没法走 `diff --no-index`（对目录会失败），
/// 合成一个 `diff --git` 头，让全部改动列表至少能看见这条路径。
pub fn untracked_entry_diff(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return String::new();
    }
    format!("diff --git a/{path} b/{path}\nnew file mode 000000\n--- /dev/null\n+++ b/{path}\n")
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

pub fn resolve_worktree_path(main_root: &str, worktree_dir: &str) -> PathBuf {
    let root = Path::new(main_root);
    let path = Path::new(worktree_dir);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::subprocess::{MockSubprocess, Output};

    /// pre-commit hook 失败时，必须把 git 的诊断原样回传，而不是吞成一句
    /// 「git commit 失败」——用户要靠这段文字才知道是 hook 里的 npm 没找到。
    #[test]
    fn failed_commit_surfaces_the_hook_diagnostics() {
        let stderr = b".husky/pre-commit: line 4: npm: command not found\n\
husky - pre-commit hook exited with code 127 (error)"
            .to_vec();
        let sp = MockSubprocess::new().when(
            "git",
            &["-C", "/repo", "commit", "-m", "msg"],
            Output {
                code: Some(1),
                stdout: Vec::new(),
                stderr,
            },
        );
        let err = commit_and_maybe_push_with(&sp, "/repo", "msg", false, "main")
            .expect_err("hook 非零退出必须报错");
        assert!(err.contains("npm: command not found"), "实际错误：{err}");
    }

    /// commit 失败就绝不能继续 push：否则用户会收到一条与实际状态无关的推送错误。
    #[test]
    fn failed_commit_does_not_attempt_push() {
        let sp = MockSubprocess::new().when(
            "git",
            &["-C", "/repo", "commit", "-m", "msg"],
            Output {
                code: Some(1),
                stdout: Vec::new(),
                stderr: b"hook failed".to_vec(),
            },
        );
        let err = commit_and_maybe_push_with(&sp, "/repo", "msg", true, "main").unwrap_err();
        assert_eq!(err, "hook failed");
    }

    #[test]
    fn status_parser_reads_branch_and_files() {
        let status = parse_git_status(
            "## main...origin/main [ahead 2, behind 1]\n M src/lib.rs\n?? notes.md\n",
        );
        assert_eq!(status.branch, "main");
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(status.files.len(), 2);
    }

    #[test]
    fn status_parser_keeps_submodule_and_nested_repo_paths() {
        let status =
            parse_git_status("## main\n M vendor/lib\n m vendor/dirty\n?? skills/fx-deploy/\n");
        assert_eq!(
            status.files,
            vec![
                (" M".into(), "vendor/lib".into()),
                (" m".into(), "vendor/dirty".into()),
                ("??".into(), "skills/fx-deploy".into()),
            ]
        );
        let rows = build_git_tree(&status.files, &HashSet::new());
        assert!(
            rows.iter()
                .any(|row| row.path == "skills/fx-deploy" && row.status.as_deref() == Some("??")),
            "未跟踪的嵌套仓库必须出现在文件树里，不能因为路径带尾斜杠被丢掉：{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.path == "vendor/lib" && row.status.as_deref() == Some(" M"))
        );
    }

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
        let parsed = parse_diff_with_file_headers(&untracked_entry_diff("skills/fx-deploy"));
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.file_header && line.text == "skills/fx-deploy")
        );
    }

    #[test]
    fn provider_aware_status_uses_subprocess_seam() {
        let status_args = [
            "-C",
            "/repo",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "-b",
        ];
        let subprocess = MockSubprocess::new().when(
            "git",
            &status_args,
            Output {
                code: Some(0),
                stdout: b"## main\n M src/lib.rs\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let status = load_git_status_with(&subprocess, "/repo");
        assert!(status.ok);
        assert_eq!(status.branch, "main");
        assert_eq!(status.files, vec![(" M".into(), "src/lib.rs".into())]);
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

    fn init_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let root = dir.to_str().unwrap().to_string();
        run_git(&root, &["init", "-q"]).unwrap();
        run_git(&root, &["config", "user.email", "t@t"]).unwrap();
        run_git(&root, &["config", "user.name", "t"]).unwrap();
        root
    }

    /// 每个仓库只报自己的文件。
    ///
    /// 旧实现在这里把子仓文件摊回父仓列表（`vendor/lib/a.txt`），结果文件失去
    /// 仓库身份：暂存按路径落到子仓，提交却回到父仓执行，子仓改动永远提交不出去。
    /// 现在父仓只看到一条 gitlink，子仓的文件由子仓自己的 status 负责。
    #[test]
    fn nested_repository_files_stay_in_their_own_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = init_repo(tmp.path());
        let nested = Path::new(&parent).join("vendor/lib");
        let nested_root = init_repo(&nested);
        std::fs::write(nested.join("a.txt"), "one\n").unwrap();
        run_git(&nested_root, &["add", "a.txt"]).unwrap();
        run_git(&nested_root, &["commit", "-qm", "sub"]).unwrap();
        run_git(&parent, &["add", "vendor/lib"]).unwrap();
        run_git(&parent, &["commit", "-qm", "add sub"]).unwrap();
        std::fs::write(nested.join("a.txt"), "two\n").unwrap();

        let parent_status = load_git_status(&parent);
        assert!(
            parent_status
                .files
                .iter()
                .any(|(code, path)| path == "vendor/lib" && code.contains('M')),
            "父仓看到的是一条 gitlink：{:?}",
            parent_status.files
        );
        assert!(
            !parent_status
                .files
                .iter()
                .any(|(_, path)| path.starts_with("vendor/lib/")),
            "子仓文件不该出现在父仓列表里：{:?}",
            parent_status.files
        );

        let nested_status = load_git_status(&nested_root);
        assert!(
            nested_status
                .files
                .iter()
                .any(|(code, path)| path == "a.txt" && code.contains('M')),
            "子仓自己报自己的文件：{:?}",
            nested_status.files
        );
    }

    /// 父仓的 diff 只包含父仓自己的改动。
    ///
    /// 旧行为把子仓内部（含**已提交**的内容）摊进父仓的「全部改动」，于是变更列表
    /// 只有一条 gitlink、右边 diff 却铺开几十个文件，两边对不上。指针变化属于父仓，
    /// 子仓的工作区内容属于子仓自己那一组。
    #[test]
    fn parent_diff_shows_the_gitlink_pointer_not_the_submodule_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = init_repo(tmp.path());
        let nested = Path::new(&parent).join("sub");
        let nested_root = init_repo(&nested);
        std::fs::write(nested.join("a.txt"), "one\n").unwrap();
        run_git(&nested_root, &["add", "a.txt"]).unwrap();
        run_git(&nested_root, &["commit", "-qm", "sub"]).unwrap();
        run_git(&parent, &["add", "sub"]).unwrap();
        run_git(&parent, &["commit", "-qm", "add sub"]).unwrap();

        // 子仓提交了新内容，父仓的 gitlink 还指着旧 SHA。
        std::fs::write(nested.join("a.txt"), "two\n").unwrap();
        run_git(&nested_root, &["add", "a.txt"]).unwrap();
        run_git(&nested_root, &["commit", "-qm", "move pointer"]).unwrap();

        let mut args = DiffScope::All.args().to_vec();
        args.pop(); // 去掉结尾的 "--"，这里不限定路径
        let out = run_git(&parent, &args).unwrap();
        let diff = String::from_utf8_lossy(&out.stdout);
        assert!(
            diff.contains("Subproject commit"),
            "父仓要显示 gitlink 指针变化：{diff}"
        );
        assert!(
            !diff.contains("a.txt"),
            "子仓内部文件不属于父仓的 diff：{diff}"
        );
    }

    /// `.gitmodules` 里的 `ignore` 配置必须被尊重。
    ///
    /// 以前 status 强制 `--ignore-submodules=none`，覆盖了用户的明确配置：明明配了
    /// `ignore = all`，父仓还是塞进来一条 gitlink，跟子仓自己那一组重复。
    /// 现在不传参，口径完全交给 git。
    #[test]
    fn submodule_ignore_configuration_is_respected() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = init_repo(&tmp.path().join("upstream"));
        std::fs::write(Path::new(&upstream).join("a.txt"), "one\n").unwrap();
        run_git(&upstream, &["add", "a.txt"]).unwrap();
        run_git(&upstream, &["commit", "-qm", "seed"]).unwrap();

        let parent = init_repo(&tmp.path().join("parent"));
        std::fs::write(Path::new(&parent).join("r.txt"), "r\n").unwrap();
        run_git(&parent, &["add", "r.txt"]).unwrap();
        run_git(&parent, &["commit", "-qm", "init"]).unwrap();
        run_git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &upstream,
                "sub",
            ],
        )
        .unwrap();
        run_git(&parent, &["commit", "-qm", "add sub"]).unwrap();

        // 子仓变脏：默认配置下父仓会报它。
        std::fs::write(Path::new(&parent).join("sub/a.txt"), "dirty\n").unwrap();
        assert!(
            load_git_status(&parent)
                .files
                .iter()
                .any(|(_, path)| path == "sub"),
            "没配 ignore 时跟随 git 默认：父仓报告 submodule 有变化"
        );

        // 配上 ignore = all 之后，父仓就该闭嘴——用户已经明确说了不想看。
        run_git(
            &parent,
            &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
        )
        .unwrap();
        assert!(
            !load_git_status(&parent)
                .files
                .iter()
                .any(|(_, path)| path == "sub"),
            "配了 ignore = all 就不该再报 submodule：{:?}",
            load_git_status(&parent).files
        );
    }

    /// 子仓已提交、工作区干净，只剩父仓的指针落后：这是父仓一条**真实的**待提交改动。
    ///
    /// 提交它才会把"我用子仓的哪个版本"记进父仓；不报的话，子模块升级永远推不上去，
    /// 协作者拉下来仍是旧版本。上游 b38a284 曾选择在这种情况下丢掉这条 gitlink，
    /// 那是旧摊平模型下的权宜——当时不丢就会把子仓已提交的内容摊成上百个文件。
    /// 现在不摊平了，这条记录没有理由再被隐藏；嫌吵可以在 `.gitmodules` 里配
    /// `ignore`，那条配置是被尊重的（见 submodule_ignore_configuration_is_respected）。
    #[test]
    fn submodule_pointer_change_is_a_pending_parent_change() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = init_repo(tmp.path());
        let nested = Path::new(&parent).join("vendor/lib");
        let nested_root = init_repo(&nested);
        std::fs::write(nested.join("a.txt"), "one\n").unwrap();
        run_git(&nested_root, &["add", "a.txt"]).unwrap();
        run_git(&nested_root, &["commit", "-qm", "sub"]).unwrap();
        run_git(&parent, &["add", "vendor/lib"]).unwrap();
        run_git(&parent, &["commit", "-qm", "add sub"]).unwrap();
        std::fs::write(nested.join("a.txt"), "two\n").unwrap();
        run_git(&nested_root, &["add", "a.txt"]).unwrap();
        run_git(&nested_root, &["commit", "-qm", "move pointer"]).unwrap();

        let status = load_git_status(&parent);
        assert!(
            status.files.iter().any(|(_, path)| path == "vendor/lib"),
            "指针落后是父仓要提交的一条 gitlink：{:?}",
            status.files
        );
        assert!(
            !status
                .files
                .iter()
                .any(|(_, path)| path.starts_with("vendor/lib/")),
            "子仓内部文件归子仓自己那一组：{:?}",
            status.files
        );
    }
}
