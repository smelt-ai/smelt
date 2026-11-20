//! Safe, local multi-repository worktree operations.

use std::{
    collections::HashSet,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use smelt_core::fs::{FileSystem as _, LocalFs};
use smelt_runtime_client::protocol::reason::ControlReason;

use crate::{GitOutput, run_git};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateWorkset {
    pub project_root: PathBuf,
    pub task: String,
    pub branch: String,
    /// 新分支从哪个分支拉出来。不填用远端默认分支；填了先找远端上的同名分支，再找本地分支。
    pub base: Option<String>,
    pub repositories: Vec<Repository>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRepository {
    pub name: String,
    pub root: PathBuf,
    pub worktree: PathBuf,
    pub base_ref: String,
    pub branch_created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workset {
    pub project_root: PathBuf,
    pub task: String,
    pub branch: String,
    pub repositories: Vec<WorktreeRepository>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishReport {
    pub retained_branches: Vec<String>,
    pub leftover_entries: Vec<PathBuf>,
}

/// 工作树没建成、关不了或改不了名。用户自己能处理的几种带 `reason`，客户端按自己的语言说；
/// 其余是 git 或系统的原话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeError {
    message: String,
    reason: Option<ControlReason>,
    rollback_errors: Vec<String>,
}

impl WorktreeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: None,
            rollback_errors: Vec::new(),
        }
    }

    fn because(reason: ControlReason) -> Self {
        Self::new(reason.text().unwrap_or_default()).with_reason(reason)
    }

    fn with_reason(mut self, reason: ControlReason) -> Self {
        self.reason = Some(reason);
        self
    }

    /// 带种类时客户端按它说；回滚没做完的细节只在 `Display` 里。
    pub fn reason(&self) -> Option<&ControlReason> {
        self.reason.as_ref()
    }

    fn with_rollback(mut self, errors: Vec<String>) -> Self {
        self.rollback_errors = errors;
        self
    }
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        for error in &self.rollback_errors {
            write!(f, "\nRollback didn't finish: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WorktreeError {}

impl From<crate::GitError> for WorktreeError {
    fn from(error: crate::GitError) -> Self {
        let converted = Self::new(error.message());
        match error.reason() {
            Some(reason) => converted.with_reason(reason.clone()),
            None => converted,
        }
    }
}

struct PreparedRepository {
    name: String,
    root: PathBuf,
    worktree: PathBuf,
    base_ref: String,
    branch_created: bool,
}

pub fn create_workset(request: CreateWorkset) -> Result<Workset, WorktreeError> {
    let task = validate_segment(&request.task, "The worktree name")?;
    let branch = request.branch;
    if branch.is_empty() || branch.trim() != branch {
        return Err(WorktreeError::new(
            "The branch name can't be empty or have leading or trailing spaces",
        ));
    }
    let base = request
        .base
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty());

    // Keep Windows verbatim prefixes out of paths passed to Git and compared with its output.
    let project_root = LocalFs
        .canonicalize(&request.project_root)
        .map_err(|error| {
            WorktreeError::new(format!(
                "couldn't resolve the project directory {}: {error}",
                request.project_root.display()
            ))
        })?;
    crate::project_repository::exclude_managed_directory(&project_root, "worktrees")?;

    let worktrees_root = project_root.join("worktrees");
    let task_root = worktrees_root.join(&task);
    if task_root.exists() && !task_root.is_dir() {
        return Err(WorktreeError::new(format!(
            "the worktree target isn't a directory: {}",
            task_root.display()
        )));
    }

    let mut names = HashSet::new();
    let mut roots = HashSet::new();
    let mut prepared = Vec::with_capacity(request.repositories.len());
    if request.repositories.is_empty() {
        return Err(WorktreeError::because(ControlReason::WorktreeNoRepository));
    }

    for repository in &request.repositories {
        validate_segment(&repository.name, "The repository name")?;
        if !names.insert(repository.name.clone()) {
            return Err(WorktreeError::new(format!(
                "Duplicate repository name: {}",
                repository.name
            )));
        }
        let root = LocalFs.canonicalize(&repository.root).map_err(|error| {
            WorktreeError::new(format!(
                "couldn't resolve repository {} ({}): {error}",
                repository.name,
                repository.root.display()
            ))
        })?;
        if !roots.insert(root.clone()) {
            return Err(WorktreeError::new(format!(
                "The same repository was selected twice: {}",
                root.display()
            )));
        }
        let top = git_output(&root, &["rev-parse", "--show-toplevel"]).map_err(|error| {
            WorktreeError::new(format!(
                "repository {} couldn't read its Git root: {error}",
                repository.name
            ))
        })?;
        let git_root = LocalFs
            .canonicalize(Path::new(top.stdout_str().trim()))
            .map_err(|error| {
                WorktreeError::new(format!(
                    "repository {} returned an invalid Git root: {error}",
                    repository.name
                ))
            })?;
        if git_root != root {
            return Err(WorktreeError::new(format!(
                "{} isn't a repository root (the Git root is {})",
                repository.root.display(),
                git_root.display()
            )));
        }

        let worktree = task_root.join(&repository.name);
        if (worktree.starts_with(&root) && root != project_root)
            || root.starts_with(&worktrees_root)
        {
            return Err(WorktreeError::new(format!(
                "The worktree directory is nested with a repository: {}",
                worktree.display()
            )));
        }
        if worktree.exists() {
            return Err(WorktreeError::because(
                ControlReason::WorktreeTargetExists {
                    path: worktree.display().to_string(),
                },
            ));
        }
        if !git_succeeds(&root, &["check-ref-format", "--branch", &branch])? {
            return Err(WorktreeError::new(format!(
                "Invalid Git branch name: {branch}"
            )));
        }

        let base_ref = match base {
            Some(base) => fetch_named_base(&root, &repository.name, base)?,
            None => fetch_default_base(&root, &repository.name)?,
        };
        let branch_created = !local_branch_exists(&root, &branch)?;
        if let Some(path) = checked_out_at(&root, &branch)? {
            return Err(WorktreeError::because(
                ControlReason::WorktreeBranchCheckedOut {
                    repository: repository.name.clone(),
                    branch: branch.clone(),
                    path: path.display().to_string(),
                },
            ));
        }
        prepared.push(PreparedRepository {
            name: repository.name.clone(),
            root,
            worktree,
            base_ref,
            branch_created,
        });
    }

    fs::create_dir_all(&task_root).map_err(|error| {
        WorktreeError::new(format!(
            "couldn't create the worktree directory {}: {error}",
            task_root.display()
        ))
    })?;
    let mut created = Vec::with_capacity(prepared.len());
    for repository in prepared {
        let root_text = repository.worktree.to_string_lossy().into_owned();
        let result = if repository.branch_created {
            run_git(
                &repository.root.to_string_lossy(),
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    &root_text,
                    &repository.base_ref,
                ],
            )
        } else {
            run_git(
                &repository.root.to_string_lossy(),
                &["worktree", "add", &root_text, &branch],
            )
        };
        let output = match result {
            Ok(output) if output.success() => output,
            Ok(output) => {
                let mut rollback = created.clone();
                rollback.push(WorktreeRepository {
                    name: repository.name.clone(),
                    root: repository.root.clone(),
                    worktree: repository.worktree.clone(),
                    base_ref: repository.base_ref.clone(),
                    branch_created: repository.branch_created,
                });
                let errors = rollback_worktrees(&rollback, &branch);
                return Err(WorktreeError::new(format!(
                    "couldn't create the worktree for repository {}: {}",
                    repository.name,
                    git_error(&output, "git worktree add failed")
                ))
                .with_rollback(errors));
            }
            Err(error) => {
                let mut rollback = created.clone();
                rollback.push(WorktreeRepository {
                    name: repository.name.clone(),
                    root: repository.root.clone(),
                    worktree: repository.worktree.clone(),
                    base_ref: repository.base_ref.clone(),
                    branch_created: repository.branch_created,
                });
                let errors = rollback_worktrees(&rollback, &branch);
                return Err(WorktreeError::new(format!(
                    "couldn't create the worktree for repository {}: {error}",
                    repository.name
                ))
                .with_rollback(errors));
            }
        };
        let _ = output;
        created.push(WorktreeRepository {
            name: repository.name,
            root: repository.root,
            worktree: repository.worktree,
            base_ref: repository.base_ref,
            branch_created: repository.branch_created,
        });
    }

    Ok(Workset {
        project_root,
        task,
        branch,
        repositories: created,
    })
}

/// 分支上的改动进没进基线。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeState {
    Merged,
    /// 确实还有改动没进基线。
    Unmerged,
    /// 判断不了：把分支合进基线有冲突。常见于 squash 合并之后基线又改过同一处。
    Unconfirmed,
}

/// 工作树里一个仓库现在的样子：关闭前要看的几项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryState {
    pub name: String,
    /// worktree 目录还在（上次关闭做了一半时可能已经移除）。
    pub present: bool,
    /// 有未提交的改动或未跟踪的文件。
    pub dirty: bool,
    /// 本地还有工作树分支时它的合并情况；分支已经不在了是 `None`。
    pub merge: Option<MergeState>,
}

/// 只读检查工作树的各个仓库：目录在不在、干不干净、分支合没合进基线。目录和 Git 登记对
/// 不上这类关不了也不该硬关的情况直接报错。基线用本地已有的远端引用，要最新的先调
/// [`refresh_workset_bases`]。
pub fn inspect_workset(workset: &Workset) -> Result<Vec<RepositoryState>, WorktreeError> {
    let mut states = Vec::with_capacity(workset.repositories.len());
    for repository in &workset.repositories {
        let registered_path = checked_out_at(&repository.root, &workset.branch)?;
        let present = match registered_path {
            Some(path) if path == repository.worktree && repository.worktree.exists() => true,
            Some(path) if path == repository.worktree => {
                return Err(WorktreeError::new(format!(
                    "repository {}'s worktree directory doesn't exist, but Git still lists it: {}",
                    repository.name,
                    path.display()
                )));
            }
            Some(path) => {
                return Err(WorktreeError::new(format!(
                    "repository {}'s branch {} is checked out elsewhere: {}",
                    repository.name,
                    workset.branch,
                    path.display()
                )));
            }
            None if repository.worktree.exists() => {
                return Err(WorktreeError::new(format!(
                    "repository {}'s working directory exists but isn't registered as a worktree: {}",
                    repository.name,
                    repository.worktree.display()
                )));
            }
            None => false,
        };
        let dirty = present && {
            let status = git_output(
                &repository.worktree,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )
            .map_err(|error| {
                WorktreeError::new(format!(
                    "couldn't read the status of repository {}: {error}",
                    repository.name
                ))
            })?;
            if !status.success() {
                return Err(WorktreeError::new(format!(
                    "couldn't read the status of repository {}: {}",
                    repository.name,
                    git_error(&status, "git status failed")
                )));
            }
            !status.stdout.is_empty()
        };
        // 目录已经移除、分支也不是这次建的（上次关闭只做了一半，或者复用的分支已被删掉）：
        // 这个仓库没什么要查的了。
        let branch_exists = local_branch_exists(&repository.root, &workset.branch)?;
        let merge = if branch_exists && (present || repository.branch_created) {
            Some(
                merged_into_base(&repository.root, &workset.branch, &repository.base_ref).map_err(
                    |error| {
                        WorktreeError::new(format!(
                            "couldn't check whether repository {}'s branch is merged: {error}",
                            repository.name
                        ))
                    },
                )?,
            )
        } else if !branch_exists && repository.branch_created && present {
            return Err(WorktreeError::new(format!(
                "repository {}'s worktree branch {} doesn't exist",
                repository.name, workset.branch
            )));
        } else {
            None
        };
        states.push(RepositoryState {
            name: repository.name.clone(),
            present,
            dirty,
            merge,
        });
    }
    Ok(states)
}

/// 关闭工作树：每个仓库都干净、分支都合进了基线才动手，先移除全部 worktree，再删这次建的
/// 分支。判断不了是否合并（[`MergeState::Unconfirmed`]）的，`allow_unconfirmed_merge` 为
/// 真时也关；有未提交改动、确实没合并的一律拦下。
pub fn finish_workset(
    workset: &Workset,
    allow_unconfirmed_merge: bool,
) -> Result<FinishReport, WorktreeError> {
    let states = inspect_workset(workset)?;
    for (repository, state) in workset.repositories.iter().zip(&states) {
        if state.dirty {
            return Err(WorktreeError::because(
                ControlReason::WorktreeRepositoryDirty {
                    repository: state.name.clone(),
                },
            ));
        }
        let base = repository
            .base_ref
            .strip_prefix("refs/remotes/")
            .or_else(|| repository.base_ref.strip_prefix("refs/heads/"))
            .unwrap_or(&repository.base_ref);
        match state.merge {
            Some(MergeState::Unmerged) => {
                return Err(WorktreeError::because(
                    ControlReason::WorktreeBranchUnmerged {
                        repository: state.name.clone(),
                        branch: workset.branch.clone(),
                        base: base.to_string(),
                    },
                ));
            }
            Some(MergeState::Unconfirmed) if !allow_unconfirmed_merge => {
                return Err(WorktreeError::because(
                    ControlReason::WorktreeMergeUnconfirmed {
                        repository: state.name.clone(),
                        branch: workset.branch.clone(),
                        base: base.to_string(),
                    },
                ));
            }
            _ => {}
        }
    }
    let already_removed: HashSet<PathBuf> = workset
        .repositories
        .iter()
        .zip(&states)
        .filter(|(_, state)| !state.present)
        .map(|(repository, _)| repository.worktree.clone())
        .collect();

    for repository in &workset.repositories {
        if !already_removed.contains(&repository.worktree) {
            let path = repository.worktree.to_string_lossy().into_owned();
            let output =
                git_output(&repository.root, &["worktree", "remove", &path]).map_err(|error| {
                    WorktreeError::new(format!(
                        "couldn't remove repository {}'s worktree (other steps may not have finished): {error}",
                        repository.name
                    ))
                })?;
            if !output.success() {
                return Err(WorktreeError::new(format!(
                    "couldn't remove repository {}'s worktree (other steps may not have finished): {}",
                    repository.name,
                    git_error(&output, "git worktree remove failed")
                )));
            }
        }
        if repository.branch_created && local_branch_exists(&repository.root, &workset.branch)? {
            let output = git_output(&repository.root, &["branch", "-D", &workset.branch]).map_err(
                |error| {
                    WorktreeError::new(format!(
                        "the worktree was removed, but deleting repository {}'s branch failed: {error}",
                        repository.name
                    ))
                },
            )?;
            if !output.success() {
                return Err(WorktreeError::new(format!(
                    "the worktree was removed, but deleting repository {}'s branch failed: {}",
                    repository.name,
                    git_error(&output, "git branch -D failed")
                )));
            }
        }
    }

    let task_root = workset.project_root.join("worktrees").join(&workset.task);
    let leftover_entries = match fs::read_dir(&task_root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(WorktreeError::new(format!(
                "the worktree was closed, but its directory {} couldn't be read: {error}",
                task_root.display()
            )));
        }
    };
    if leftover_entries.is_empty() && task_root.exists() {
        fs::remove_dir(&task_root).map_err(|error| {
            WorktreeError::new(format!(
                "the worktree was closed, but its empty directory {} couldn't be removed: {error}",
                task_root.display()
            ))
        })?;
    }

    Ok(FinishReport {
        retained_branches: workset
            .repositories
            .iter()
            .filter(|repository| !repository.branch_created)
            .map(|repository| format!("{}: {}", repository.name, workset.branch))
            .collect(),
        leftover_entries,
    })
}

/// 关闭前把各仓库基线所在的远端 fetch 一次，合并判定才看得到远端刚合进去的改动。
/// 走网络，调用方不要拿着目录锁调。fetch 失败不中断关闭：返回每条失败说明，判定照用
/// 本地已有的远端引用。
pub fn refresh_workset_bases(workset: &Workset) -> Vec<String> {
    let mut failures = Vec::new();
    let mut fetched = HashSet::new();
    for repository in &workset.repositories {
        let Some(remote) = base_remote(&repository.root, &repository.base_ref) else {
            continue;
        };
        if !fetched.insert((repository.root.clone(), remote.clone())) {
            continue;
        }
        let failure = match git_output(&repository.root, &["fetch", &remote]) {
            Ok(output) if output.success() => continue,
            Ok(output) => git_error(&output, "git fetch failed"),
            Err(error) => error,
        };
        failures.push(format!(
            "repository {} couldn't fetch {remote}: {failure}",
            repository.name
        ));
    }
    failures
}

/// `refs/remotes/<远端>/<分支>` 里的远端名。远端名可以带 `/`，取能对上的最长那个。
fn base_remote(root: &Path, base_ref: &str) -> Option<String> {
    let rest = base_ref.strip_prefix("refs/remotes/")?;
    let remotes = git_output(root, &["remote"])
        .ok()
        .filter(GitOutput::success)?;
    remotes
        .stdout_str()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty() && rest.starts_with(&format!("{remote}/")))
        .max_by_key(|remote| remote.len())
        .map(str::to_string)
}

/// 分支上的改动是否都已经进了基线。普通合并后分支是基线的祖先；squash / rebase 合并
/// 生成的是新提交，分支不再是祖先，这时看把分支合进基线会不会改动什么：合并结果的树
/// 等于基线的树，就说明改动都已在基线里。合并有冲突（比如合进去之后基线又改过同一处）
/// 判断不了，交给人决定。
fn merged_into_base(root: &Path, branch: &str, base: &str) -> Result<MergeState, String> {
    let ancestor = git_output(root, &["merge-base", "--is-ancestor", branch, base])?;
    match ancestor.code {
        Some(0) => return Ok(MergeState::Merged),
        Some(1) => {}
        _ => return Err(git_error(&ancestor, "git merge-base failed")),
    }
    let merged = git_output(
        root,
        &["merge-tree", "--write-tree", "--no-messages", base, branch],
    )?;
    match merged.code {
        Some(0) => {}
        Some(1) => return Ok(MergeState::Unconfirmed),
        _ => return Err(git_error(&merged, "git merge-tree failed")),
    }
    let base_tree = git_output(root, &["rev-parse", &format!("{base}^{{tree}}")])?;
    if !base_tree.success() {
        return Err(git_error(&base_tree, "git rev-parse failed"));
    }
    let merged_tree = merged.stdout_str();
    Ok(
        if merged_tree.lines().next().map(str::trim) == Some(base_tree.stdout_str().trim()) {
            MergeState::Merged
        } else {
            MergeState::Unmerged
        },
    )
}

pub fn abort_workset_creation(workset: &Workset) -> Result<(), WorktreeError> {
    let errors = rollback_worktrees(&workset.repositories, &workset.branch);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WorktreeError::new(
            "Creating the worktree failed and the automatic rollback didn't finish",
        )
        .with_rollback(errors))
    }
}

pub fn rename_workset(workset: &Workset, new_task: &str) -> Result<Workset, WorktreeError> {
    let task = validate_segment(new_task, "The worktree name")?;
    if task == workset.task {
        return Ok(workset.clone());
    }
    let old_task_root = workset.project_root.join("worktrees").join(&workset.task);
    let new_task_root = workset.project_root.join("worktrees").join(&task);
    if new_task_root.exists() {
        return Err(WorktreeError::because(
            ControlReason::WorktreeTargetExists {
                path: new_task_root.display().to_string(),
            },
        ));
    }
    for repository in &workset.repositories {
        let expected_path = old_task_root.join(&repository.name);
        if repository.worktree != expected_path {
            return Err(WorktreeError::new(format!(
                "repository {}'s worktree path doesn't match the worktree directory",
                repository.name
            )));
        }
        let checked_out = checked_out_at(&repository.root, &workset.branch)?;
        if checked_out.as_deref() != Some(repository.worktree.as_path()) {
            return Err(WorktreeError::new(format!(
                "repository {}'s worktree branch isn't checked out in the expected directory",
                repository.name
            )));
        }
        let status = git_output(
            &repository.worktree,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .map_err(|error| {
            WorktreeError::new(format!(
                "couldn't read the status of repository {}: {error}",
                repository.name
            ))
        })?;
        if !status.success() {
            return Err(WorktreeError::new(format!(
                "couldn't read the status of repository {}: {}",
                repository.name,
                git_error(&status, "git status failed")
            )));
        }
        if !status.stdout.is_empty() {
            return Err(WorktreeError::because(ControlReason::WorktreeRenameDirty {
                repository: repository.name.clone(),
            }));
        }
        if !local_branch_exists(&repository.root, &workset.branch)? {
            return Err(WorktreeError::new(format!(
                "repository {}'s worktree branch {} doesn't exist",
                repository.name, workset.branch
            )));
        }
    }
    fs::create_dir_all(&new_task_root).map_err(|error| {
        WorktreeError::new(format!(
            "couldn't create the worktree directory {}: {error}",
            new_task_root.display()
        ))
    })?;

    let mut progress = Vec::with_capacity(workset.repositories.len());
    for repository in &workset.repositories {
        let new_path = new_task_root.join(&repository.name);
        let old_branch =
            git_output(&repository.worktree, &["branch", "--show-current"]).map_err(|error| {
                let errors = rollback_renames(&progress, &new_task_root);
                WorktreeError::new(format!(
                    "couldn't read repository {}'s current branch: {error}",
                    repository.name
                ))
                .with_rollback(errors)
            })?;
        if !old_branch.success() || old_branch.stdout_str().trim() != workset.branch {
            let errors = rollback_renames(&progress, &new_task_root);
            return Err(
                WorktreeError::because(ControlReason::WorktreeBranchChanged {
                    repository: repository.name.clone(),
                })
                .with_rollback(errors),
            );
        }
        progress.push((repository.clone(), new_path.clone(), false));

        let old_path = repository.worktree.to_string_lossy().into_owned();
        let new_path_text = new_path.to_string_lossy().into_owned();
        let moved = git_output(
            &repository.root,
            &["worktree", "move", &old_path, &new_path_text],
        );
        match moved {
            Ok(output) if output.success() => {
                if let Some((_, _, was_moved)) = progress.last_mut() {
                    *was_moved = true;
                }
            }
            Ok(output) => {
                let errors = rollback_renames(&progress, &new_task_root);
                return Err(WorktreeError::new(format!(
                    "couldn't move repository {}'s worktree: {}",
                    repository.name,
                    git_error(&output, "git worktree move failed")
                ))
                .with_rollback(errors));
            }
            Err(error) => {
                let errors = rollback_renames(&progress, &new_task_root);
                return Err(WorktreeError::new(format!(
                    "couldn't move repository {}'s worktree: {error}",
                    repository.name
                ))
                .with_rollback(errors));
            }
        }
    }

    if old_task_root.exists() {
        if let Err(error) = fs::remove_dir(&old_task_root) {
            let rollback = rollback_renames(&progress, &new_task_root);
            return Err(WorktreeError::new(format!(
                "the worktree was renamed, but the old directory {} couldn't be removed: {error}",
                old_task_root.display()
            ))
            .with_rollback(rollback));
        }
    }
    let repositories = workset
        .repositories
        .iter()
        .map(|repository| WorktreeRepository {
            worktree: new_task_root.join(&repository.name),
            ..repository.clone()
        })
        .collect();
    Ok(Workset {
        project_root: workset.project_root.clone(),
        task,
        branch: workset.branch.clone(),
        repositories,
    })
}

fn rollback_renames(
    progress: &[(WorktreeRepository, PathBuf, bool)],
    new_task_root: &Path,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (repository, new_path, moved) in progress.iter().rev() {
        let old_path = repository.worktree.to_string_lossy().into_owned();
        if *moved {
            let current_path_text = new_path.to_string_lossy().into_owned();
            match git_output(
                &repository.root,
                &["worktree", "move", &current_path_text, &old_path],
            ) {
                Ok(output) if output.success() => {}
                Ok(output) => errors.push(format!(
                    "{} worktree rollback: {}",
                    repository.name,
                    git_error(&output, "git worktree move failed")
                )),
                Err(error) => {
                    errors.push(format!("{} worktree rollback: {error}", repository.name))
                }
            }
        }
    }
    match fs::remove_dir(new_task_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!(
            "couldn't remove the worktree directory {}: {error}",
            new_task_root.display()
        )),
    }
    errors
}

pub(crate) fn validate_segment(value: &str, label: &str) -> Result<String, WorktreeError> {
    let path = Path::new(value);
    let mut components = path.components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !value.starts_with('.')
        && !value.contains(['/', '\\', '\0'])
        && value.trim() == value;
    if !valid {
        return Err(WorktreeError::new(format!(
            "{label} must be a single safe directory name: {value:?}"
        )));
    }
    Ok(value.to_string())
}

fn git_output(root: &Path, args: &[&str]) -> Result<GitOutput, String> {
    let root = root.to_string_lossy();
    let output = run_git(&root, args).map_err(|error| error.to_string())?;
    Ok(output)
}

fn git_succeeds(root: &Path, args: &[&str]) -> Result<bool, WorktreeError> {
    let output = git_output(root, args)
        .map_err(|error| WorktreeError::new(format!("couldn't run git {:?}: {error}", args)))?;
    Ok(output.success())
}

fn local_branch_exists(root: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let ref_name = format!("refs/heads/{branch}");
    let output = git_output(root, &["show-ref", "--verify", "--quiet", &ref_name])
        .map_err(|error| WorktreeError::new(format!("couldn't check branch {branch}: {error}")))?;
    match output.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorktreeError::new(format!(
            "couldn't check branch {branch}: {}",
            git_error(&output, "git show-ref failed")
        ))),
    }
}

fn checked_out_at(root: &Path, branch: &str) -> Result<Option<PathBuf>, WorktreeError> {
    let output = git_output(root, &["worktree", "list", "--porcelain", "-z"])
        .map_err(|error| WorktreeError::new(format!("couldn't read the worktree list: {error}")))?;
    if !output.success() {
        return Err(WorktreeError::new(format!(
            "couldn't read the worktree list: {}",
            git_error(&output, "git worktree list failed")
        )));
    }
    let expected = format!("branch refs/heads/{branch}");
    let mut path = None;
    let output_text = std::str::from_utf8(&output.stdout).map_err(|error| {
        WorktreeError::new(format!("couldn't decode the worktree path: {error}"))
    })?;
    for line in output_text.split('\0') {
        if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if line == expected {
            return Ok(path);
        } else if line.is_empty() {
            path = None;
        }
    }
    Ok(None)
}

/// 仓库的远端，当前分支上游所在的远端排最前，其余按名字从长到短（远端名可以带 `/`）。
/// 第二项是当前分支的上游（`<远端>/<分支>`）。
fn ordered_remotes(
    root: &Path,
    name: &str,
) -> Result<(Vec<String>, Option<String>), WorktreeError> {
    let remotes = git_output(root, &["remote"]).map_err(|error| {
        WorktreeError::new(format!(
            "repository {name} couldn't read its remotes: {error}"
        ))
    })?;
    if !remotes.success() {
        return Err(WorktreeError::new(format!(
            "repository {name} couldn't read its remotes: {}",
            git_error(&remotes, "git remote failed")
        )));
    }
    let mut remotes = remotes
        .stdout_str()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    remotes.sort_by_key(|remote| std::cmp::Reverse(remote.len()));

    let upstream = git_output(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok()
    .filter(GitOutput::success)
    .map(|output| output.stdout_str().trim().to_string());

    let preferred = upstream.as_ref().and_then(|upstream| {
        remotes
            .iter()
            .find(|remote| upstream.starts_with(&format!("{remote}/")))
            .cloned()
    });
    let mut seen = HashSet::new();
    let ordered = preferred
        .into_iter()
        .chain(remotes)
        .filter(|remote| seen.insert(remote.clone()))
        .collect();
    Ok((ordered, upstream))
}

fn fetch_remote(root: &Path, name: &str, remote: &str) -> Result<(), WorktreeError> {
    let fetched = git_output(root, &["fetch", remote]).map_err(|error| {
        WorktreeError::new(format!(
            "repository {name} couldn't fetch {remote}: {error}"
        ))
    })?;
    if !fetched.success() {
        return Err(WorktreeError::new(format!(
            "repository {name} couldn't fetch {remote}: {}",
            git_error(&fetched, "git fetch failed")
        )));
    }
    Ok(())
}

fn ref_exists(root: &Path, reference: &str) -> Result<bool, WorktreeError> {
    let commit = format!("{reference}^{{commit}}");
    git_succeeds(root, &["rev-parse", "--verify", "--quiet", &commit])
}

/// 指定的基线分支：优先用远端上的同名分支（fetch 后拿到最新的），都没有再用本地分支。
/// `<远端>/<分支>` 写法也认。本地已有跟踪分支的远端先 fetch，其余按远端顺序；
/// 某个远端 fetch 失败只跳过，免得连不上的远端挡住别处能用的分支。
fn fetch_named_base(root: &Path, name: &str, base: &str) -> Result<String, WorktreeError> {
    if !git_succeeds(root, &["check-ref-format", "--branch", base])? {
        return Err(WorktreeError::new(format!(
            "Invalid Git branch name: {base}"
        )));
    }
    let (remotes, _) = ordered_remotes(root, name)?;
    // 按远端名从长到短匹配，带 `/` 的远端名才不会被短名截掉。
    let mut by_length = remotes.clone();
    by_length.sort_by_key(|remote| std::cmp::Reverse(remote.len()));
    let qualified = by_length.iter().find_map(|remote| {
        base.strip_prefix(remote.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|branch| !branch.is_empty())
            .map(|branch| format!("refs/remotes/{remote}/{branch}"))
            .map(|reference| (remote.clone(), reference))
    });
    let candidates = qualified.into_iter().chain(
        remotes
            .iter()
            .map(|remote| (remote.clone(), format!("refs/remotes/{remote}/{base}"))),
    );
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for candidate in candidates {
        if ref_exists(root, &candidate.1)? {
            known.push(candidate);
        } else {
            unknown.push(candidate);
        }
    }

    let mut fetched = HashSet::new();
    let mut failed = HashSet::new();
    let mut failures = Vec::new();
    for (remote, reference) in known.into_iter().chain(unknown) {
        if failed.contains(&remote) {
            continue;
        }
        if fetched.insert(remote.clone())
            && let Err(error) = fetch_remote(root, name, &remote)
        {
            failures.push(error.message);
            failed.insert(remote);
            continue;
        }
        if ref_exists(root, &reference)? {
            return Ok(reference);
        }
    }
    let local_ref = format!("refs/heads/{base}");
    if ref_exists(root, &local_ref)? {
        return Ok(local_ref);
    }
    if !failures.is_empty() {
        return Err(WorktreeError::new(format!(
            "repository {name} has no branch {base}; some remotes couldn't be fetched: {}",
            failures.join("; ")
        )));
    }
    Err(WorktreeError::because(
        ControlReason::WorktreeBaseNotFound {
            repository: name.to_string(),
            base: base.to_string(),
        },
    ))
}

fn fetch_default_base(root: &Path, name: &str) -> Result<String, WorktreeError> {
    let (remotes, upstream) = ordered_remotes(root, name)?;
    if remotes.is_empty() {
        return Err(WorktreeError::because(ControlReason::WorktreeNoRemote {
            repository: name.to_string(),
        }));
    }
    let mut selected = None;
    for remote in remotes {
        let head_ref = format!("refs/remotes/{remote}/HEAD");
        let head = git_output(root, &["symbolic-ref", "--quiet", &head_ref])
            .ok()
            .filter(GitOutput::success)
            .map(|output| output.stdout_str().trim().to_string());
        if let Some(head) = head {
            selected = Some((remote, head));
            break;
        }
        if let Some(upstream) = &upstream
            && let Some(branch) = upstream.strip_prefix(&format!("{remote}/"))
        {
            selected = Some((remote.clone(), format!("refs/remotes/{remote}/{branch}")));
            break;
        }
    }
    let (remote, base_ref) = selected.ok_or_else(|| {
        WorktreeError::because(ControlReason::WorktreeNoUpstream {
            repository: name.to_string(),
        })
    })?;
    fetch_remote(root, name, &remote)?;
    let resolved = git_output(root, &["rev-parse", "--verify", &base_ref]).map_err(|error| {
        WorktreeError::new(format!(
            "repository {name}'s baseline isn't available: {error}"
        ))
    })?;
    if !resolved.success() {
        return Err(WorktreeError::new(format!(
            "repository {name}'s upstream default branch {base_ref} doesn't exist: {}",
            git_error(&resolved, "git rev-parse failed")
        )));
    }
    Ok(base_ref)
}

fn rollback_worktrees(repositories: &[WorktreeRepository], branch: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for repository in repositories.iter().rev() {
        let mut worktree_remains = match checked_out_at(&repository.root, branch) {
            Ok(Some(path)) => {
                if path != repository.worktree {
                    errors.push(format!(
                        "{}: branch {branch} is checked out elsewhere: {}",
                        repository.name,
                        path.display()
                    ));
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                errors.push(format!("{} worktree check: {error}", repository.name));
                true
            }
        };
        if worktree_remains {
            let path = repository.worktree.to_string_lossy().into_owned();
            match git_output(&repository.root, &["worktree", "remove", &path]) {
                Ok(output) if output.success() => worktree_remains = false,
                Ok(output) => errors.push(format!(
                    "{}: {}",
                    repository.name,
                    git_error(&output, "couldn't remove the worktree created this time")
                )),
                Err(error) => errors.push(format!("{}: {error}", repository.name)),
            }
        }
        if repository.branch_created && !worktree_remains {
            match local_branch_exists(&repository.root, branch) {
                Ok(false) => {}
                Err(error) => errors.push(format!("{} branch cleanup: {error}", repository.name)),
                Ok(true) => match git_output(&repository.root, &["branch", "-D", branch]) {
                    Ok(output) if output.success() => {}
                    Ok(output) => errors.push(format!(
                        "{} branch cleanup: {}",
                        repository.name,
                        git_error(&output, "couldn't clean up the branch created this time")
                    )),
                    Err(error) => {
                        errors.push(format!("{} branch cleanup: {error}", repository.name))
                    }
                },
            }
        }
    }
    if let Some(first) = repositories.first() {
        let task_root = first.worktree.parent();
        if let Some(task_root) = task_root {
            match fs::read_dir(task_root) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        if let Err(error) = fs::remove_dir(task_root) {
                            errors.push(format!(
                                "couldn't remove the empty worktree directory {}: {error}",
                                task_root.display()
                            ));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => errors.push(format!(
                    "couldn't check the worktree directory {}: {error}",
                    task_root.display()
                )),
            }
        }
    }
    errors
}

fn git_error(output: &GitOutput, fallback: &str) -> String {
    let stderr = output.stderr_str().trim().to_string();
    if stderr.is_empty() {
        fallback.to_string()
    } else {
        stderr
    }
}
