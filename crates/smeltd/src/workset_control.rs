use std::{collections::HashSet, path::PathBuf};

use smelt_core::{
    workspace_catalog::{self, DirectoryChange, DirectoryError},
    workspace_directory::{
        WorkspaceRevision, WorkspaceWorkset, WorkspaceWorksetCreateParams,
        WorkspaceWorksetFinishParams, WorkspaceWorksetFinishResult, WorkspaceWorksetRenameParams,
    },
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use crate::EventHubHandle;

pub(crate) fn repository_action(
    store: &smelt_store::Store,
    params: smelt_core::workspace_directory::WorkspaceProjectRepositoryActionParams,
    event_hub: &EventHubHandle,
) -> Result<smelt_core::workspace_directory::WorkspaceDirectory, ControlError> {
    use smelt_core::workspace_directory::{
        WorkspaceProjectRepository, WorkspaceProjectRepositoryAction as Action,
    };
    use smelt_git::project_repository as git;

    let mut cloned = if let Action::Clone { url, name } = &params.action {
        let directory = workspace_catalog::directory_on(store).map_err(directory_error)?;
        if directory
            .repositories
            .iter()
            .any(|repo| repo.project_root == params.project_root && repo.name == *name)
        {
            return Err(repository_error("仓库名已存在，请修改目录名"));
        }
        Some(
            git::clone_repository(std::path::Path::new(&params.project_root), url, name)
                .map_err(repository_error)?,
        )
    } else {
        None
    };
    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    if matches!(&params.action, Action::Discover)
        && store
            .project_repositories_initialized(&params.project_root)
            .map_err(repository_error)?
    {
        return Ok(directory);
    }
    let project = std::fs::canonicalize(&params.project_root).map_err(repository_error)?;
    let project_root = params.project_root;
    let mut repositories = directory
        .repositories
        .into_iter()
        .filter(|repo| repo.project_root == project_root)
        .collect::<Vec<_>>();
    let mut added = None;
    let mut removed_link = None;
    match params.action {
        Action::Discover => {
            let mut candidates = Vec::new();
            if project.join(".git").exists() {
                candidates.push(project.clone());
            }
            match std::fs::read_dir(project.join("repos")) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(repository_error)?;
                        if entry.path().join(".git").exists() {
                            candidates.push(entry.path());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(repository_error(error)),
            }
            for candidate in candidates {
                let root = git::repository_root(&candidate).map_err(repository_error)?;
                let root = path_text(&root)?;
                if repositories.iter().any(|repo| repo.root == root) {
                    continue;
                }
                let name = candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| repository_error("仓库名称不是 UTF-8"))?
                    .to_string();
                git::validate_name(&name).map_err(repository_error)?;
                if repositories.iter().any(|repo| repo.name == name) {
                    return Err(repository_error(format!(
                        "发现同名仓库 {name}，请在管理项目仓库中选择并改名"
                    )));
                }
                repositories.push(WorkspaceProjectRepository {
                    project_root: project_root.clone(),
                    name,
                    root,
                    position: repositories.len() as u32,
                    managed_link: None,
                });
            }
        }
        Action::Bind { source, name } => {
            let root =
                git::repository_root(std::path::Path::new(&source)).map_err(repository_error)?;
            reject_duplicate(&repositories, &name, &path_text(&root)?)?;
            let bound = git::bind(&project, &root, &name).map_err(repository_error)?;
            repositories.push(WorkspaceProjectRepository {
                project_root: project_root.clone(),
                name,
                root: path_text(&bound.root)?,
                position: repositories.len() as u32,
                managed_link: bound.managed_link.as_deref().map(path_text).transpose()?,
            });
            added = Some(bound);
        }
        Action::Clone { name, .. } => {
            if repositories.iter().any(|repo| repo.name == name) {
                return Err(repository_error(
                    "克隆完成，但仓库名被其它客户端占用；目录已保留，请改名后重新绑定",
                ));
            }
            let bound = cloned
                .take()
                .ok_or_else(|| repository_error("克隆结果缺失"))?;
            repositories.push(WorkspaceProjectRepository {
                project_root: project_root.clone(),
                name,
                root: path_text(&bound.root)?,
                position: repositories.len() as u32,
                managed_link: None,
            });
            added = Some(bound);
        }
        Action::Remove { name } => {
            let Some(index) = repositories.iter().position(|repo| repo.name == name) else {
                return Err(repository_error("仓库已不存在"));
            };
            let repo = &repositories[index];
            let referenced = directory
                .worksets
                .iter()
                .filter(|workset| {
                    workset.project_root == project_root
                        && workset
                            .repositories
                            .iter()
                            .any(|r| r.name == repo.name || r.root == repo.root)
                })
                .map(|workset| workset.task.as_str())
                .collect::<Vec<_>>();
            if !referenced.is_empty() {
                return Err(repository_error(format!(
                    "仓库仍被工作树引用：{}",
                    referenced.join("、")
                )));
            }
            if let Some(link) = &repo.managed_link {
                let existed = std::fs::symlink_metadata(link).is_ok();
                git::remove_link(
                    &project,
                    std::path::Path::new(link),
                    std::path::Path::new(&repo.root),
                )
                .map_err(repository_error)?;
                if existed {
                    removed_link = Some(repo.clone());
                }
            }
            repositories.remove(index);
        }
    }
    match workspace_catalog::set_project_repositories_on_locked(store, &project_root, &repositories)
    {
        Ok(change) => {
            publish_change(&change, event_hub);
            Ok(change.directory)
        }
        Err(error) => {
            let mut message = format!("保存项目仓库失败：{error}");
            if let Some(bound) = added {
                if let Some(link) = bound.managed_link {
                    if let Err(error) = git::remove_link(&project, &link, &bound.root) {
                        message.push_str(&format!("；软链接回滚失败：{error}"));
                    }
                } else {
                    message.push_str(&format!(
                        "；仓库文件保留在 {}，可选择该目录重新绑定",
                        bound.root.display()
                    ));
                }
            }
            if let Some(repo) = removed_link {
                if let Err(error) =
                    git::bind(&project, std::path::Path::new(&repo.root), &repo.name)
                {
                    message.push_str(&format!("；恢复软链接失败：{error}"));
                }
            }
            Err(repository_error(message))
        }
    }
}

fn path_text(path: &std::path::Path) -> Result<String, ControlError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| repository_error("路径不是 UTF-8"))
}

fn repository_error(error: impl std::fmt::Display) -> ControlError {
    ControlError::new(ControlErrorCode::OperationFailed, error.to_string())
}

fn reject_duplicate(
    repositories: &[smelt_core::workspace_directory::WorkspaceProjectRepository],
    name: &str,
    root: &str,
) -> Result<(), ControlError> {
    for repo in repositories {
        let same_root = match std::fs::canonicalize(&repo.root) {
            Ok(existing) => existing == std::path::Path::new(root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => repo.root == root,
            Err(error) => {
                return Err(repository_error(format!(
                    "无法检查已有仓库 {}：{error}",
                    repo.root
                )));
            }
        };
        if repo.name == name || same_root {
            return Err(repository_error("该仓库或目录名已添加，请勿重复绑定"));
        }
    }
    Ok(())
}

pub(crate) fn create(
    store: &smelt_store::Store,
    params: WorkspaceWorksetCreateParams,
    event_hub: &EventHubHandle,
) -> Result<WorkspaceRevision, ControlError> {
    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    let mut selected_names = HashSet::new();
    let repositories = params
        .repository_names
        .iter()
        .map(|name| name.trim())
        .map(|name| {
            if name.is_empty() || !selected_names.insert(name.to_string()) {
                return Err(ControlError::new(
                    ControlErrorCode::InvalidParams,
                    format!("仓库名称为空或重复：{name}"),
                ));
            }
            directory
                .repositories
                .iter()
                .find(|repository| {
                    repository.project_root == params.project_root && repository.name == name
                })
                .map(|repository| smelt_git::worktree::Repository {
                    name: repository.name.clone(),
                    root: PathBuf::from(&repository.root),
                })
                .ok_or_else(|| {
                    ControlError::new(
                        ControlErrorCode::InvalidParams,
                        format!("项目没有配置仓库：{name}"),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let branch = params.branch_name();
    let workset = smelt_git::worktree::create_workset(smelt_git::worktree::CreateWorkset {
        project_root: PathBuf::from(&params.project_root),
        task: params.task,
        branch,
        repositories,
    })
    .map_err(worktree_error)?;
    let record = model_workset(&workset, uuid::Uuid::new_v4().simple().to_string(), 0);
    match workspace_catalog::create_workset_on_locked(store, &record) {
        Ok(change) => {
            publish_change(&change, event_hub);
            Ok(change.directory.version())
        }
        Err(error) => {
            let rollback = smelt_git::worktree::abort_workset_creation(&workset);
            let mut message = format!("工作树目录保存失败：{error}");
            if let Err(rollback) = rollback {
                message.push_str(&format!("\n{rollback}"));
            }
            Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                message,
            ))
        }
    }
}

pub(crate) fn rename(
    store: &smelt_store::Store,
    params: WorkspaceWorksetRenameParams,
    event_hub: &EventHubHandle,
) -> Result<WorkspaceRevision, ControlError> {
    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    let workset = directory
        .worksets
        .iter()
        .find(|workset| workset.id == params.workset_id)
        .ok_or_else(|| ControlError::new(ControlErrorCode::NotFound, "工作树不存在"))?;
    if directory
        .conversations
        .iter()
        .any(|conversation| conversation.workset_id.as_deref() == Some(workset.id.as_str()))
    {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            "工作树仍绑定会话；先关闭并删除这些会话再重命名",
        ));
    }
    let original = git_workset(workset);
    let renamed =
        smelt_git::worktree::rename_workset(&original, &params.task).map_err(worktree_error)?;
    let updated = model_workset(&renamed, workset.id.clone(), workset.position);
    match workspace_catalog::rename_workset_on_locked(store, &updated) {
        Ok(change) => {
            publish_change(&change, event_hub);
            Ok(change.directory.version())
        }
        Err(error) => {
            let rollback = smelt_git::worktree::rename_workset(&renamed, &original.task);
            let mut message = format!("工作树元数据更新失败：{error}");
            if let Err(rollback) = rollback {
                message.push_str(&format!("\nGit 回滚未完成：{rollback}"));
            }
            Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                message,
            ))
        }
    }
}

pub(crate) fn finish(
    store: &smelt_store::Store,
    params: WorkspaceWorksetFinishParams,
    event_hub: &EventHubHandle,
) -> Result<WorkspaceWorksetFinishResult, ControlError> {
    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    let workset = directory
        .worksets
        .iter()
        .find(|workset| workset.id == params.workset_id)
        .ok_or_else(|| ControlError::new(ControlErrorCode::NotFound, "工作树不存在"))?;
    if directory.conversations.iter().any(|conversation| {
        conversation.workset_id.as_deref() == Some(workset.id.as_str())
            && conversation.archived_at.is_none()
    }) {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            "工作树仍绑定未归档会话；请先关闭或归档这些会话",
        ));
    }
    let report =
        smelt_git::worktree::finish_workset(&git_workset(workset)).map_err(worktree_error)?;
    let change =
        workspace_catalog::delete_workset_on_locked(store, &workset.id).map_err(|error| {
            ControlError::new(
                ControlErrorCode::OperationFailed,
                format!("Git worktree 已关闭，但删除工作树记录失败：{error}"),
            )
        })?;
    publish_change(&change, event_hub);
    Ok(WorkspaceWorksetFinishResult {
        revision: change.directory.version(),
        retained_branches: report.retained_branches,
        leftover_entries: report
            .leftover_entries
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    })
}

fn model_workset(
    workset: &smelt_git::worktree::Workset,
    id: String,
    position: u32,
) -> WorkspaceWorkset {
    WorkspaceWorkset {
        id,
        project_root: workset.project_root.to_string_lossy().into_owned(),
        task: workset.task.clone(),
        branch: workset.branch.clone(),
        position,
        repositories: workset
            .repositories
            .iter()
            .enumerate()
            .map(|(position, repository)| {
                smelt_core::workspace_directory::WorkspaceWorksetRepository {
                    name: repository.name.clone(),
                    root: repository.root.to_string_lossy().into_owned(),
                    worktree: repository.worktree.to_string_lossy().into_owned(),
                    base_ref: repository.base_ref.clone(),
                    branch_created: repository.branch_created,
                    position: position as u32,
                }
            })
            .collect(),
    }
}

fn git_workset(workset: &WorkspaceWorkset) -> smelt_git::worktree::Workset {
    smelt_git::worktree::Workset {
        project_root: PathBuf::from(&workset.project_root),
        task: workset.task.clone(),
        branch: workset.branch.clone(),
        repositories: workset
            .repositories
            .iter()
            .map(|repository| smelt_git::worktree::WorktreeRepository {
                name: repository.name.clone(),
                root: PathBuf::from(&repository.root),
                worktree: PathBuf::from(&repository.worktree),
                base_ref: repository.base_ref.clone(),
                branch_created: repository.branch_created,
            })
            .collect(),
    }
}

fn publish_change(change: &DirectoryChange, event_hub: &EventHubHandle) {
    if change.changed
        && let Err(error) = event_hub.publish_workspace(&change.directory)
    {
        super::dlog(&format!("推送工作区目录失败：{error}"));
    }
}

fn directory_error(error: DirectoryError) -> ControlError {
    let code = match &error {
        DirectoryError::NotFound(_) => ControlErrorCode::NotFound,
        DirectoryError::Invalid(_) => ControlErrorCode::InvalidParams,
        DirectoryError::Store(_) => ControlErrorCode::OperationFailed,
    };
    ControlError::new(code, error.to_string())
}

fn worktree_error(error: smelt_git::worktree::WorktreeError) -> ControlError {
    ControlError::new(ControlErrorCode::OperationFailed, error.to_string())
}
