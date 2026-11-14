//! Safe, local multi-repository worktree operations.

use std::{
    collections::HashSet,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use smelt_core::fs::{FileSystem as _, LocalFs};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeError {
    message: String,
    rollback_errors: Vec<String>,
}

impl WorktreeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rollback_errors: Vec::new(),
        }
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
            write!(f, "\n回滚未完成：{error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WorktreeError {}

struct PreparedRepository {
    name: String,
    root: PathBuf,
    worktree: PathBuf,
    base_ref: String,
    branch_created: bool,
}

pub fn create_workset(request: CreateWorkset) -> Result<Workset, WorktreeError> {
    let task = validate_segment(&request.task, "工作树名称")?;
    let branch = request.branch;
    if branch.is_empty() || branch.trim() != branch {
        return Err(WorktreeError::new("分支名不能为空或包含首尾空格"));
    }

    // Keep Windows verbatim prefixes out of paths passed to Git and compared with its output.
    let project_root = LocalFs
        .canonicalize(&request.project_root)
        .map_err(|error| {
            WorktreeError::new(format!(
                "无法解析项目目录 {}：{error}",
                request.project_root.display()
            ))
        })?;
    crate::project_repository::exclude_managed_directory(&project_root, "worktrees")
        .map_err(WorktreeError::new)?;

    let worktrees_root = project_root.join("worktrees");
    let task_root = worktrees_root.join(&task);
    if task_root.exists() && !task_root.is_dir() {
        return Err(WorktreeError::new(format!(
            "工作树目标不是目录：{}",
            task_root.display()
        )));
    }

    let mut names = HashSet::new();
    let mut roots = HashSet::new();
    let mut prepared = Vec::with_capacity(request.repositories.len());
    if request.repositories.is_empty() {
        return Err(WorktreeError::new("至少选择一个参与仓库"));
    }

    for repository in &request.repositories {
        validate_segment(&repository.name, "仓库名称")?;
        if !names.insert(repository.name.clone()) {
            return Err(WorktreeError::new(format!(
                "仓库名称重复：{}",
                repository.name
            )));
        }
        let root = LocalFs.canonicalize(&repository.root).map_err(|error| {
            WorktreeError::new(format!(
                "无法解析仓库 {}（{}）：{error}",
                repository.name,
                repository.root.display()
            ))
        })?;
        if !roots.insert(root.clone()) {
            return Err(WorktreeError::new(format!(
                "同一个仓库被重复选择：{}",
                root.display()
            )));
        }
        let top = git_output(&root, &["rev-parse", "--show-toplevel"]).map_err(|error| {
            WorktreeError::new(format!(
                "仓库 {} 无法读取 Git 根目录：{error}",
                repository.name
            ))
        })?;
        let git_root = LocalFs
            .canonicalize(Path::new(top.stdout_str().trim()))
            .map_err(|error| {
                WorktreeError::new(format!(
                    "仓库 {} 返回了无效的 Git 根目录：{error}",
                    repository.name
                ))
            })?;
        if git_root != root {
            return Err(WorktreeError::new(format!(
                "{} 不是仓库根目录（Git 根目录是 {}）",
                repository.root.display(),
                git_root.display()
            )));
        }

        let worktree = task_root.join(&repository.name);
        if (worktree.starts_with(&root) && root != project_root)
            || root.starts_with(&worktrees_root)
        {
            return Err(WorktreeError::new(format!(
                "工作树目录与仓库嵌套：{}",
                worktree.display()
            )));
        }
        if worktree.exists() {
            return Err(WorktreeError::new(format!(
                "工作树目标已存在：{}",
                worktree.display()
            )));
        }
        if !git_succeeds(&root, &["check-ref-format", "--branch", &branch])? {
            return Err(WorktreeError::new(format!("无效的 Git 分支名：{branch}")));
        }

        let base_ref = fetch_default_base(&root, &repository.name)?;
        let branch_created = !local_branch_exists(&root, &branch)?;
        if let Some(path) = checked_out_at(&root, &branch)? {
            return Err(WorktreeError::new(format!(
                "仓库 {} 的分支 {branch} 已在 {} 检出",
                repository.name,
                path.display()
            )));
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
            "无法创建工作树目录 {}：{error}",
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
                    "创建仓库 {} 的 worktree 失败：{}",
                    repository.name,
                    git_error(&output, "git worktree add 失败")
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
                    "创建仓库 {} 的 worktree 失败：{error}",
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

pub fn finish_workset(workset: &Workset) -> Result<FinishReport, WorktreeError> {
    let mut already_removed = HashSet::new();
    for repository in &workset.repositories {
        let registered_path = checked_out_at(&repository.root, &workset.branch)?;
        let exists = match registered_path {
            Some(path) if path == repository.worktree && repository.worktree.exists() => true,
            Some(path) if path == repository.worktree => {
                return Err(WorktreeError::new(format!(
                    "仓库 {} 的 worktree 目录不存在但 Git 仍登记着它：{}",
                    repository.name,
                    path.display()
                )));
            }
            Some(path) => {
                return Err(WorktreeError::new(format!(
                    "仓库 {} 的分支 {} 已在其他目录检出：{}",
                    repository.name,
                    workset.branch,
                    path.display()
                )));
            }
            None if repository.worktree.exists() => {
                return Err(WorktreeError::new(format!(
                    "仓库 {} 的工作目录存在但未登记为 worktree：{}",
                    repository.name,
                    repository.worktree.display()
                )));
            }
            None => {
                already_removed.insert(repository.worktree.clone());
                false
            }
        };
        if !exists {
            if repository.branch_created && local_branch_exists(&repository.root, &workset.branch)?
            {
                // A previous attempt may have removed the worktree but failed before deleting
                // its branch. The branch is checked below and removed after every repo passes.
            } else {
                continue;
            }
        } else {
            let status = git_output(
                &repository.worktree,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )
            .map_err(|error| {
                WorktreeError::new(format!("读取仓库 {} 状态失败：{error}", repository.name))
            })?;
            if !status.success() {
                return Err(WorktreeError::new(format!(
                    "读取仓库 {} 状态失败：{}",
                    repository.name,
                    git_error(&status, "git status 失败")
                )));
            }
            if !status.stdout.is_empty() {
                return Err(WorktreeError::new(format!(
                    "仓库 {} 有未提交改动或未跟踪文件，已阻止关闭工作树",
                    repository.name
                )));
            }
        }

        if local_branch_exists(&repository.root, &workset.branch)? {
            let merged = run_git(
                &repository.root.to_string_lossy(),
                &[
                    "merge-base",
                    "--is-ancestor",
                    &workset.branch,
                    &repository.base_ref,
                ],
            )
            .map_err(|error| {
                WorktreeError::new(format!(
                    "检查仓库 {} 的分支合并状态失败：{error}",
                    repository.name
                ))
            })?;
            if !merged.success() {
                if merged.code == Some(1) {
                    return Err(WorktreeError::new(format!(
                        "仓库 {} 的分支 {} 尚未合并，已阻止关闭工作树",
                        repository.name, workset.branch
                    )));
                }
                return Err(WorktreeError::new(format!(
                    "检查仓库 {} 的分支合并状态失败：{}",
                    repository.name,
                    git_error(&merged, "git merge-base 失败")
                )));
            }
        } else if repository.branch_created {
            return Err(WorktreeError::new(format!(
                "仓库 {} 的工作树分支 {} 不存在",
                repository.name, workset.branch
            )));
        }
    }

    for repository in &workset.repositories {
        if !already_removed.contains(&repository.worktree) {
            let path = repository.worktree.to_string_lossy().into_owned();
            let output =
                git_output(&repository.root, &["worktree", "remove", &path]).map_err(|error| {
                    WorktreeError::new(format!(
                        "移除仓库 {} 的 worktree 失败（其余操作可能尚未完成）：{error}",
                        repository.name
                    ))
                })?;
            if !output.success() {
                return Err(WorktreeError::new(format!(
                    "移除仓库 {} 的 worktree 失败（其余操作可能尚未完成）：{}",
                    repository.name,
                    git_error(&output, "git worktree remove 失败")
                )));
            }
        }
        if repository.branch_created && local_branch_exists(&repository.root, &workset.branch)? {
            let output = git_output(&repository.root, &["branch", "-D", &workset.branch]).map_err(
                |error| {
                    WorktreeError::new(format!(
                        "worktree 已移除，但仓库 {} 的分支删除失败：{error}",
                        repository.name
                    ))
                },
            )?;
            if !output.success() {
                return Err(WorktreeError::new(format!(
                    "worktree 已移除，但仓库 {} 的分支删除失败：{}",
                    repository.name,
                    git_error(&output, "git branch -D 失败")
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
                "工作树已关闭，但无法读取工作树目录 {}：{error}",
                task_root.display()
            )));
        }
    };
    if leftover_entries.is_empty() && task_root.exists() {
        fs::remove_dir(&task_root).map_err(|error| {
            WorktreeError::new(format!(
                "工作树已关闭，但无法移除空工作树目录 {}：{error}",
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

pub fn abort_workset_creation(workset: &Workset) -> Result<(), WorktreeError> {
    let errors = rollback_worktrees(&workset.repositories, &workset.branch);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WorktreeError::new("工作树创建失败，自动回滚未完成").with_rollback(errors))
    }
}

pub fn rename_workset(workset: &Workset, new_task: &str) -> Result<Workset, WorktreeError> {
    let task = validate_segment(new_task, "工作树名称")?;
    if task == workset.task {
        return Ok(workset.clone());
    }
    let old_task_root = workset.project_root.join("worktrees").join(&workset.task);
    let new_task_root = workset.project_root.join("worktrees").join(&task);
    if new_task_root.exists() {
        return Err(WorktreeError::new(format!(
            "工作树目标已存在：{}",
            new_task_root.display()
        )));
    }
    for repository in &workset.repositories {
        let expected_path = old_task_root.join(&repository.name);
        if repository.worktree != expected_path {
            return Err(WorktreeError::new(format!(
                "仓库 {} 的 worktree 路径与工作树目录不匹配",
                repository.name
            )));
        }
        let checked_out = checked_out_at(&repository.root, &workset.branch)?;
        if checked_out.as_deref() != Some(repository.worktree.as_path()) {
            return Err(WorktreeError::new(format!(
                "仓库 {} 的工作树分支未在预期目录检出",
                repository.name
            )));
        }
        let status = git_output(
            &repository.worktree,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .map_err(|error| {
            WorktreeError::new(format!("读取仓库 {} 状态失败：{error}", repository.name))
        })?;
        if !status.success() {
            return Err(WorktreeError::new(format!(
                "读取仓库 {} 状态失败：{}",
                repository.name,
                git_error(&status, "git status 失败")
            )));
        }
        if !status.stdout.is_empty() {
            return Err(WorktreeError::new(format!(
                "仓库 {} 有未提交改动，已阻止重命名",
                repository.name
            )));
        }
        if !local_branch_exists(&repository.root, &workset.branch)? {
            return Err(WorktreeError::new(format!(
                "仓库 {} 的工作树分支 {} 不存在",
                repository.name, workset.branch
            )));
        }
    }
    fs::create_dir_all(&new_task_root).map_err(|error| {
        WorktreeError::new(format!(
            "无法创建工作树目录 {}：{error}",
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
                    "读取仓库 {} 当前分支失败：{error}",
                    repository.name
                ))
                .with_rollback(errors)
            })?;
        if !old_branch.success() || old_branch.stdout_str().trim() != workset.branch {
            let errors = rollback_renames(&progress, &new_task_root);
            return Err(WorktreeError::new(format!(
                "仓库 {} 当前分支已变化，已取消重命名",
                repository.name
            ))
            .with_rollback(errors));
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
                    "移动仓库 {} 的 worktree 失败：{}",
                    repository.name,
                    git_error(&output, "git worktree move 失败")
                ))
                .with_rollback(errors));
            }
            Err(error) => {
                let errors = rollback_renames(&progress, &new_task_root);
                return Err(WorktreeError::new(format!(
                    "移动仓库 {} 的 worktree 失败：{error}",
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
                "工作树已重命名，但无法移除旧目录 {}：{error}",
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
                    "{} worktree 回滚: {}",
                    repository.name,
                    git_error(&output, "git worktree move 失败")
                )),
                Err(error) => errors.push(format!("{} worktree 回滚: {error}", repository.name)),
            }
        }
    }
    match fs::remove_dir(new_task_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!(
            "无法移除工作树目录 {}：{error}",
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
            "{label}必须是单个安全的目录名：{value:?}"
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
        .map_err(|error| WorktreeError::new(format!("运行 git {:?} 失败：{error}", args)))?;
    Ok(output.success())
}

fn local_branch_exists(root: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let ref_name = format!("refs/heads/{branch}");
    let output = git_output(root, &["show-ref", "--verify", "--quiet", &ref_name])
        .map_err(|error| WorktreeError::new(format!("检查分支 {branch} 失败：{error}")))?;
    match output.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorktreeError::new(format!(
            "检查分支 {branch} 失败：{}",
            git_error(&output, "git show-ref 失败")
        ))),
    }
}

fn checked_out_at(root: &Path, branch: &str) -> Result<Option<PathBuf>, WorktreeError> {
    let output = git_output(root, &["worktree", "list", "--porcelain", "-z"])
        .map_err(|error| WorktreeError::new(format!("读取 worktree 列表失败：{error}")))?;
    if !output.success() {
        return Err(WorktreeError::new(format!(
            "读取 worktree 列表失败：{}",
            git_error(&output, "git worktree list 失败")
        )));
    }
    let expected = format!("branch refs/heads/{branch}");
    let mut path = None;
    let output_text = std::str::from_utf8(&output.stdout)
        .map_err(|error| WorktreeError::new(format!("读取 worktree 路径编码失败：{error}")))?;
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

fn fetch_default_base(root: &Path, name: &str) -> Result<String, WorktreeError> {
    let remotes = git_output(root, &["remote"])
        .map_err(|error| WorktreeError::new(format!("仓库 {name} 无法读取远端：{error}")))?;
    if !remotes.success() {
        return Err(WorktreeError::new(format!(
            "仓库 {name} 无法读取远端：{}",
            git_error(&remotes, "git remote 失败")
        )));
    }
    let mut remotes = remotes
        .stdout_str()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if remotes.is_empty() {
        return Err(WorktreeError::new(format!(
            "仓库 {name} 没有配置 Git 远端，无法确定上游默认分支"
        )));
    }
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
    let candidates = preferred
        .into_iter()
        .chain(remotes.iter().cloned())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut selected = None;
    for remote in candidates {
        if !seen.insert(remote.clone()) {
            continue;
        }
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
        WorktreeError::new(format!(
            "仓库 {name} 没有可用的上游默认分支（检查远端 HEAD 或当前分支 upstream）"
        ))
    })?;
    let fetched = git_output(root, &["fetch", &remote])
        .map_err(|error| WorktreeError::new(format!("仓库 {name} fetch {remote} 失败：{error}")))?;
    if !fetched.success() {
        return Err(WorktreeError::new(format!(
            "仓库 {name} fetch {remote} 失败：{}",
            git_error(&fetched, "git fetch 失败")
        )));
    }
    let resolved = git_output(root, &["rev-parse", "--verify", &base_ref])
        .map_err(|error| WorktreeError::new(format!("仓库 {name} 基线不可用：{error}")))?;
    if !resolved.success() {
        return Err(WorktreeError::new(format!(
            "仓库 {name} 的上游默认分支 {base_ref} 不存在：{}",
            git_error(&resolved, "git rev-parse 失败")
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
                        "{}: 分支 {branch} 在其他位置检出：{}",
                        repository.name,
                        path.display()
                    ));
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                errors.push(format!("{} worktree 检查: {error}", repository.name));
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
                    git_error(&output, "无法移除本次创建的 worktree")
                )),
                Err(error) => errors.push(format!("{}: {error}", repository.name)),
            }
        }
        if repository.branch_created && !worktree_remains {
            match local_branch_exists(&repository.root, branch) {
                Ok(false) => {}
                Err(error) => errors.push(format!("{} 分支清理: {error}", repository.name)),
                Ok(true) => match git_output(&repository.root, &["branch", "-D", branch]) {
                    Ok(output) if output.success() => {}
                    Ok(output) => errors.push(format!(
                        "{} 分支清理: {}",
                        repository.name,
                        git_error(&output, "无法清理本次创建的分支")
                    )),
                    Err(error) => errors.push(format!("{} 分支清理: {error}", repository.name)),
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
                                "无法移除空工作树目录 {}：{error}",
                                task_root.display()
                            ));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => errors.push(format!(
                    "无法检查工作树目录 {}：{error}",
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
