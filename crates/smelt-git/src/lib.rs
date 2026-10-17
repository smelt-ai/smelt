//! Git 领域层：仓库发现、状态解析、diff 与写操作。不含 UI，不含面板状态。
//!
//! 所有命令消费者都有 provider-aware 变体，调用方可以换一个 `Subprocess` 实现
//! 把同一套逻辑跑到别的执行世界（远程 / 沙箱）里；便捷包装让本地调用点保持简短。

pub mod discovery;

use smelt_core::fs::FileSystem as _;
pub use smelt_core::subprocess::{LocalSubprocess, Output, Subprocess};
pub use smelt_runtime_client::protocol::git::{
    BranchList, CommitNode, DiffHunk, DiffKind, DiffLine, DiffScope, GitFileDiffResult,
    GitPatchAction, GitStatusData, LOG_FORMAT, LogScope, ParsedDiff, RepoInfo, WorktreeEntry,
    WorktreeListData, main_repo_root_from_common_dir, parse_diff, parse_diff_with_file_headers,
    parse_log, parse_name_status,
};
use std::path::{Path, PathBuf};

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

/// 解析 `git status --porcelain=v1 -b -z` 的输出。
///
/// 必须用 `-z`：不加时 git 会把中文、空格、引号等路径转义成 `"\344\270\255.txt"`
/// 这种 C 字符串，重命名还会写成 `old -> new`，按原样当路径就成了乱码。`-z` 下
/// 条目以 NUL 分隔、路径原样输出；重命名/复制写成 `XY 新路径\0旧路径\0`。
pub fn parse_git_status(text: &str) -> GitStatusData {
    let mut status = GitStatusData {
        ok: true,
        ..Default::default()
    };
    let mut entries = text.split('\0');
    while let Some(entry) = entries.next() {
        if let Some(branch) = entry.strip_prefix("## ") {
            let (name, upstream, ahead, behind) = parse_branch_status_line(branch);
            status.branch = name;
            status.upstream = upstream;
            status.ahead = ahead;
            status.behind = behind;
            continue;
        }
        let (Some(xy), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        if xy.contains(['R', 'C']) {
            // 紧跟的那一段是旧路径，列表只显示新路径。
            entries.next();
        }
        // 未跟踪目录 / 嵌套 git 仓库（常被当成 submodule）porcelain 会写成
        // `?? path/`。文件树按 `/` 切最后一段，尾斜杠会得到空文件名直接丢掉。
        let path = path.trim_end_matches('/');
        if !path.is_empty() {
            status.files.push((xy.to_string(), path.to_string()));
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
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "-b",
            "-z",
        ],
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

/// 推送当前分支。
///
/// 有上游就只跑 `git push`，失败原样返回 git 的诊断——不能再「失败就换个远端重试」：
/// 网络断开、被拒绝（non-fast-forward）、没权限都不是「缺上游」，换远端重试只会
/// 用一条无关的报错（比如仓库根本没有 origin）把真正的原因盖掉。
/// 没上游时才 `push -u <远端> <分支>` 建立跟踪，远端怎么选见 [`default_push_remote`]。
pub fn push_current_with(
    subprocess: &dyn Subprocess,
    root: &str,
    branch: &str,
) -> Result<(), String> {
    let env = [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")];
    if branch.is_empty() || has_upstream(subprocess, root) {
        return checked(
            subprocess.run("git", &["-C", root, "push"], None, &env),
            "git push 失败",
        );
    }
    let remote = default_push_remote(subprocess, root, branch)?;
    checked(
        subprocess.run(
            "git",
            &["-C", root, "push", "-u", &remote, branch],
            None,
            &env,
        ),
        "git push 失败",
    )
}

fn has_upstream(subprocess: &dyn Subprocess, root: &str) -> bool {
    run_git_with(
        subprocess,
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .is_ok_and(|out| out.success())
}

/// 分支还没上游时推到哪个远端，口径跟 git 自己一致：
/// `remote.pushDefault` > 唯一的远端 > 名为 `origin` 的远端。
/// 多个远端又没有 origin 时（比如同时挂着 gh / nio），猜哪个都可能推错地方，
/// 直接报错让用户自己指定，而不是瞎推。
fn default_push_remote(
    subprocess: &dyn Subprocess,
    root: &str,
    branch: &str,
) -> Result<String, String> {
    if let Ok(out) = run_git_with(subprocess, root, &["config", "--get", "remote.pushDefault"]) {
        let name = out.stdout_str().trim().to_string();
        if out.success() && !name.is_empty() {
            return Ok(name);
        }
    }
    let out = run_git_with(subprocess, root, &["remote"]).map_err(|error| error.to_string())?;
    if !out.success() {
        return Err(git_err(&out, "读取远端列表失败"));
    }
    let remotes: Vec<String> = out
        .stdout_str()
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    match remotes.as_slice() {
        [] => Err("仓库没有配置任何远端，无法推送".to_string()),
        [only] => Ok(only.clone()),
        _ if remotes.iter().any(|name| name == "origin") => Ok("origin".to_string()),
        _ => Err(format!(
            "分支「{branch}」还没有上游，且有多个远端（{}），请先执行 git push -u <远端> {branch} 指定推送目标",
            remotes.join("、")
        )),
    }
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

pub fn apply_patch(root: &str, args: &[&str], patch: &str) -> Result<(), String> {
    checked(run_git_stdin(root, args, patch), "git apply 失败")
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

/// `git add -- <paths>`。`paths` 相对 `root` 这个仓库；调用方拿到文件时就知道它属于
/// 哪个仓库，这里不再按路径反推——反推正是暂存落到子仓、提交却回到父仓的根源。
/// 批量一条命令：几十个文件逐个跑就是几十个进程。
pub fn stage_paths(root: &str, paths: &[String]) -> Result<(), String> {
    index_paths(root, "add", paths, "git add 失败")
}

/// `git reset -- <paths>`。
pub fn unstage_paths(root: &str, paths: &[String]) -> Result<(), String> {
    index_paths(root, "reset", paths, "git reset 失败")
}

fn index_paths(root: &str, verb: &str, paths: &[String], fallback: &str) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![verb, "--"];
    args.extend(paths.iter().map(String::as_str));
    checked(run_git(root, &args), fallback)
}

/// 丢弃一个文件的全部改动。已跟踪走 `git restore --staged --worktree`（暂存区和
/// 工作区一起还原，省得用户为「已 add 过」的文件再点一次）；未跟踪的 git 管不着，
/// 直接删盘。
pub fn discard_file(root: &str, path: &str, untracked: bool) -> Result<(), String> {
    if untracked {
        let full = Path::new(root).join(path);
        return smelt_core::fs::LocalFs
            .remove_file(&full)
            .map_err(|e| format!("删除 {path} 失败：{e}"));
    }
    checked(
        run_git(root, &["restore", "--staged", "--worktree", "--", path]),
        "git restore 失败",
    )
}

/// 工作区有没有未提交改动。探测失败按「干净」算：这只用来决定提醒文案。
pub fn is_dirty(root: &str) -> bool {
    run_git(root, &["status", "--porcelain"])
        .ok()
        .is_some_and(|o| o.success() && !o.stdout.is_empty())
}

pub fn apply_patch_action(root: &str, action: GitPatchAction, patch: &str) -> Result<(), String> {
    apply_patch(root, action.args(), patch)
}

/// 单个文件的 diff。git 跑不起来时把原因当正文返回，界面照样显示。
pub fn file_diff(root: &str, path: &str, untracked: bool, scope: DiffScope) -> GitFileDiffResult {
    let has_staged = !untracked
        && run_git(root, &["diff", "--staged", "--quiet", "--", path])
            .map(|o| !o.success())
            .unwrap_or(false);
    let text = if untracked {
        match run_git(root, &["diff", "--no-index", "--", "/dev/null", path]) {
            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
            Err(err) => format!("无法执行 git diff：{err}"),
        }
    } else {
        let mut args: Vec<&str> = scope.args().to_vec();
        args.push(path);
        match run_git(root, &args) {
            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
            Err(err) => format!("无法执行 git diff：{err}"),
        }
    };
    let is_gitlink = text.lines().any(|l| l.starts_with("Submodule "));
    let worktree_file = Some(Path::new(root).join(path))
        .filter(|full| smelt_core::fs::is_regular_file(&smelt_core::fs::LocalFs, full));
    GitFileDiffResult {
        text,
        has_staged,
        is_gitlink,
        worktree_file: worktree_file.map(|full| full.to_string_lossy().into_owned()),
    }
}

/// 一个仓库在 `scope` 里的全部改动，`untracked_paths` 逐个拼在后面。
///
/// 不把子仓的文件 diff 拼进父仓：子仓有自己的分组和自己的提交入口。父仓这边只展示
/// gitlink 指针变化（它确实是父仓待提交的改动）。
pub fn all_diff(root: &str, scope: DiffScope, untracked_paths: &[String]) -> String {
    let mut text = run_git(root, scope.args())
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .unwrap_or_else(|err| format!("无法执行 git diff：{err}"));
    for path in untracked_paths {
        let chunk = run_git(root, &["diff", "--no-index", "--", "/dev/null", path])
            .ok()
            .filter(|out| !out.stdout.is_empty())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_else(|| untracked_entry_diff(path));
        if chunk.is_empty() {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&chunk);
    }
    text
}

pub fn log_args(scope: &LogScope, limit: usize) -> Vec<String> {
    let mut a = vec![
        "log".to_string(),
        LOG_FORMAT.to_string(),
        format!("--max-count={limit}"),
    ];
    match scope {
        // 不带 ref = HEAD，也就是当前分支。
        LogScope::Head => {}
        LogScope::All => a.push("--all".to_string()),
        LogScope::Branch(b) => a.push(b.clone()),
    }
    a
}

pub fn log(root: &str, scope: &LogScope, limit: usize) -> Result<Vec<CommitNode>, String> {
    let args = log_args(scope, limit);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_git(root, &argv).map_err(|error| error.to_string())?;
    if !out.success() {
        return Err(git_err(&out, "git log 失败"));
    }
    Ok(parse_log(&String::from_utf8_lossy(&out.stdout)))
}

/// 一个提交改了哪些文件。只要清单，diff 内容等点了具体文件再拉——一个大提交的
/// 完整 diff 可能上万行。
pub fn commit_files(root: &str, hash: &str) -> Result<Vec<(String, String)>, String> {
    let out = run_git(
        root,
        &[
            "show",
            "--name-status",
            "--pretty=format:",
            "--no-color",
            hash,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(parse_name_status(&String::from_utf8_lossy(&out.stdout)))
}

/// 单个文件在某个提交里的改动。`--format=` 去掉提交头，只留 diff 本体。
pub fn commit_file_diff(root: &str, hash: &str, path: &str) -> String {
    match run_git(root, &["show", "--format=", "--no-color", hash, "--", path]) {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            // `git show` 失败时正文在 stderr，stdout 是空的。空成功才是无差异。
            if stdout.trim().is_empty() && !out.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                let err = err.trim();
                if err.is_empty() {
                    stdout
                } else {
                    format!("无法执行 git show：{err}")
                }
            } else {
                stdout
            }
        }
        Err(err) => format!("无法执行 git show：{err}"),
    }
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
    use smelt_runtime_client::protocol::git::build_git_tree;
    use std::collections::HashSet;

    /// pre-commit hook 失败时，必须把 git 的诊断原样回传，而不是吞成一句
    /// 「git commit 失败」——用户要靠这段文字才知道是 hook 里的 npm 没找到。
    #[test]
    fn failed_commit_surfaces_the_hook_diagnostics() {
        let stderr = b".husky/pre-commit: line 4: npm: command not found\n\
husky - pre-commit hook exited with code 127 (error)"
            .to_vec();
        let sp = MockSubprocess::new().when(
            "git",
            &[
                "-C",
                "/repo",
                "-c",
                "core.quotePath=false",
                "commit",
                "-m",
                "msg",
            ],
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
            &[
                "-C",
                "/repo",
                "-c",
                "core.quotePath=false",
                "commit",
                "-m",
                "msg",
            ],
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
            "## main...origin/main [ahead 2, behind 1]\0 M src/lib.rs\0?? notes.md\0",
        );
        assert_eq!(status.branch, "main");
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(status.files.len(), 2);
    }

    /// 输出取自真实 `git status --porcelain=v1 -b -z`：中文、空格、引号原样保留，
    /// 重命名只留新路径，不能把旧路径当成另一条改动。
    #[test]
    fn status_parser_keeps_non_ascii_and_renamed_paths_verbatim() {
        let status = parse_git_status("## main\0 M 中文 名.txt\0R  新.txt\0old.txt\0?? q\"x.txt\0");
        assert_eq!(
            status.files,
            vec![
                (" M".into(), "中文 名.txt".into()),
                ("R ".into(), "新.txt".into()),
                ("??".into(), "q\"x.txt".into()),
            ]
        );
    }

    #[test]
    fn status_parser_keeps_submodule_and_nested_repo_paths() {
        let status =
            parse_git_status("## main\0 M vendor/lib\0 m vendor/dirty\0?? skills/fx-deploy/\0");
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
    fn provider_aware_status_uses_subprocess_seam() {
        let status_args = [
            "-C",
            "/repo",
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "-b",
            "-z",
        ];
        let subprocess = MockSubprocess::new().when(
            "git",
            &status_args,
            Output {
                code: Some(0),
                stdout: b"## main\0 M src/lib.rs\0".to_vec(),
                stderr: Vec::new(),
            },
        );
        let status = load_git_status_with(&subprocess, "/repo");
        assert!(status.ok);
        assert_eq!(status.branch, "main");
        assert_eq!(status.files, vec![(" M".into(), "src/lib.rs".into())]);
    }

    fn init_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let root = dir.to_str().unwrap().to_string();
        run_git(&root, &["init", "-q"]).unwrap();
        run_git(&root, &["config", "user.email", "t@t"]).unwrap();
        run_git(&root, &["config", "user.name", "t"]).unwrap();
        root
    }

    /// 建一个带一次提交、当前在 `feature` 分支上的仓库，并按 `remotes` 挂上本地 bare 远端。
    fn repo_with_remotes(tmp: &Path, remotes: &[&str]) -> String {
        let root = init_repo(&tmp.join("work"));
        run_git(&root, &["checkout", "-q", "-b", "feature"]).unwrap();
        std::fs::write(Path::new(&root).join("a.txt"), "one\n").unwrap();
        run_git(&root, &["add", "a.txt"]).unwrap();
        run_git(&root, &["commit", "-qm", "seed"]).unwrap();
        for name in remotes {
            let bare = tmp.join(format!("{name}.git"));
            let bare = bare.to_str().unwrap();
            run_git(&root, &["init", "-q", "--bare", bare]).unwrap();
            run_git(&root, &["remote", "add", name, bare]).unwrap();
        }
        root
    }

    fn remote_has_branch(tmp: &Path, remote: &str, branch: &str) -> bool {
        let bare = tmp.join(format!("{remote}.git"));
        run_git(
            bare.to_str().unwrap(),
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .unwrap()
        .success()
    }

    /// 已有上游时 push 失败，必须原样报出这次失败的原因。
    ///
    /// 旧实现失败后无条件回退 `push -u origin`：仓库只有 nio / gh 两个远端时，
    /// 用户看到的是 "'origin' does not appear to be a git repository"，真正的原因被盖掉了。
    #[test]
    fn push_failure_with_upstream_reports_the_real_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repo_with_remotes(tmp.path(), &["nio", "gh"]);
        run_git(&root, &["push", "-q", "-u", "nio", "feature"]).unwrap();
        // 让上游远端变得不可达：模拟网络 / 权限一类与「缺上游」无关的失败。
        let gone = tmp.path().join("gone.git");
        run_git(&root, &["remote", "set-url", "nio", gone.to_str().unwrap()]).unwrap();
        std::fs::write(Path::new(&root).join("a.txt"), "two\n").unwrap();
        run_git(&root, &["commit", "-qam", "next"]).unwrap();

        let err = push_current(&root, "feature").expect_err("远端不可达必须报错");
        assert!(!err.contains("origin"), "不该去推不存在的 origin：{err}");
        assert!(err.contains("gone.git"), "应报出真实失败的远端：{err}");
        assert!(
            !remote_has_branch(tmp.path(), "gh", "feature"),
            "失败后不能擅自改推到别的远端"
        );
    }

    /// 新分支没上游、仓库只有一个远端（叫什么都行）：推到它并建立跟踪。
    #[test]
    fn first_push_uses_the_only_remote_even_if_not_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repo_with_remotes(tmp.path(), &["nio"]);

        push_current(&root, "feature").expect("唯一远端应能直接推送");
        assert!(remote_has_branch(tmp.path(), "nio", "feature"));
        assert_eq!(
            load_git_status(&root).upstream.as_deref(),
            Some("nio/feature")
        );
    }

    /// 没上游、多个远端且没有 origin：不猜，报错让用户指定。
    #[test]
    fn first_push_with_ambiguous_remotes_asks_instead_of_guessing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repo_with_remotes(tmp.path(), &["gh", "nio"]);

        let err = push_current(&root, "feature").expect_err("多远端不能瞎猜");
        assert!(
            err.contains("gh") && err.contains("nio"),
            "应列出可选远端：{err}"
        );
        assert!(!remote_has_branch(tmp.path(), "gh", "feature"));
        assert!(!remote_has_branch(tmp.path(), "nio", "feature"));
    }

    /// 配了 `remote.pushDefault` 就跟 git 一样听它的。
    #[test]
    fn first_push_honors_remote_push_default() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repo_with_remotes(tmp.path(), &["gh", "nio"]);
        run_git(&root, &["config", "remote.pushDefault", "nio"]).unwrap();

        push_current(&root, "feature").expect("有 pushDefault 应能推送");
        assert!(remote_has_branch(tmp.path(), "nio", "feature"));
        assert!(!remote_has_branch(tmp.path(), "gh", "feature"));
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
