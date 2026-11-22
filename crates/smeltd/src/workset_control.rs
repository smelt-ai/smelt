use std::{collections::HashSet, path::PathBuf};

use smelt_core::{
    workspace_catalog::{self, DirectoryChange, DirectoryError},
    workspace_directory::{
        WorkspaceDirectory, WorkspaceRevision, WorkspaceWorkset, WorkspaceWorksetCreateParams,
        WorkspaceWorksetCreateResult, WorkspaceWorksetFinishParams, WorkspaceWorksetFinishResult,
        WorkspaceWorksetInspectParams, WorkspaceWorksetInspectResult, WorkspaceWorksetMerge,
        WorkspaceWorksetRenameParams, WorkspaceWorksetRepositoryState,
    },
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlReason};

use crate::EventHubHandle;

/// 工作树和项目仓库的增删改排成一队。和目录锁分开：建工作树时的 fetch 只占这把锁，
/// 不挡会话的目录写入。要同时拿两把锁时，一律先拿这把。
fn lock_worksets() -> std::sync::MutexGuard<'static, ()> {
    static WORKSETS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    WORKSETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
            return Err(ControlError::because(
                ControlErrorCode::OperationFailed,
                ControlReason::RepositoryAlreadyAdded,
            ));
        }
        Some(
            git::clone_repository(std::path::Path::new(&params.project_root), url, name)
                .map_err(git_repository_error)?,
        )
    } else {
        None
    };
    let _worksets = lock_worksets();
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
                let root = git::repository_root(&candidate).map_err(git_repository_error)?;
                let root = path_text(&root)?;
                if repositories.iter().any(|repo| repo.root == root) {
                    continue;
                }
                let name = candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| repository_error("The repository name isn't UTF-8"))?
                    .to_string();
                git::validate_name(&name).map_err(repository_error)?;
                if repositories.iter().any(|repo| repo.name == name) {
                    return Err(repository_error(format!(
                        "A repository named {name} already exists; pick it in project repositories and rename it"
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
            let root = git::repository_root(std::path::Path::new(&source))
                .map_err(git_repository_error)?;
            reject_duplicate(&repositories, &name, &path_text(&root)?)?;
            let bound = git::bind(&project, &root, &name).map_err(git_repository_error)?;
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
                    "The clone finished, but another client took the repository name; the folder was kept, so rename it and bind it again",
                ));
            }
            let bound = cloned
                .take()
                .ok_or_else(|| repository_error("The clone result is missing"))?;
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
                return Err(repository_error("The repository no longer exists"));
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
                    "The repository is still used by worktrees: {}",
                    referenced.join(", ")
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
            let mut message = format!("Couldn't save the project repositories: {error}");
            if let Some(bound) = added {
                if let Some(link) = bound.managed_link {
                    if let Err(error) = git::remove_link(&project, &link, &bound.root) {
                        message.push_str(&format!("; couldn't roll back the symlink: {error}"));
                    }
                } else {
                    message.push_str(&format!(
                        "; the repository files are kept at {}, so you can bind that folder again",
                        bound.root.display()
                    ));
                }
            }
            if let Some(repo) = removed_link {
                if let Err(error) =
                    git::bind(&project, std::path::Path::new(&repo.root), &repo.name)
                {
                    message.push_str(&format!("; couldn't restore the symlink: {error}"));
                }
            }
            Err(repository_error(message))
        }
    }
}

fn path_text(path: &std::path::Path) -> Result<String, ControlError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| repository_error("The path isn't UTF-8"))
}

fn repository_error(error: impl std::fmt::Display) -> ControlError {
    ControlError::new(ControlErrorCode::OperationFailed, error.to_string())
}

/// 绑定、克隆仓库没成：带种类的照种类说。
fn git_repository_error(error: smelt_git::GitError) -> ControlError {
    ControlError {
        reason: error.reason().cloned(),
        ..ControlError::new(ControlErrorCode::OperationFailed, error.message())
    }
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
                    "Couldn't check the existing repository {}: {error}",
                    repo.root
                )));
            }
        };
        if repo.name == name || same_root {
            return Err(ControlError::because(
                ControlErrorCode::OperationFailed,
                ControlReason::RepositoryAlreadyAdded,
            ));
        }
    }
    Ok(())
}

pub(crate) fn create(
    store: &smelt_store::Store,
    params: WorkspaceWorksetCreateParams,
    event_hub: &EventHubHandle,
) -> Result<WorkspaceWorksetCreateResult, ControlError> {
    // 建 worktree 要 fetch 各仓库的远端，可能很久。这期间只占着工作树操作锁，不拿目录锁，
    // 会话的新建、改名这些目录写入照常进行；工作树和仓库的增删改由工作树操作锁排成一队，
    // 读到的仓库配置不会在建的过程中被改掉。
    let _worksets = lock_worksets();
    let directory = workspace_catalog::directory_on(store).map_err(directory_error)?;
    let mut selected_names = HashSet::new();
    let repositories = params
        .repository_names
        .iter()
        .map(|name| name.trim())
        .map(|name| {
            if name.is_empty() || !selected_names.insert(name.to_string()) {
                return Err(ControlError::because(
                    ControlErrorCode::InvalidParams,
                    ControlReason::RepositoryNameInvalid {
                        name: name.to_string(),
                    },
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
                    ControlError::because(
                        ControlErrorCode::InvalidParams,
                        ControlReason::RepositoryNotInProject {
                            name: name.to_string(),
                        },
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let branch = params.branch_name();
    let workset = smelt_git::worktree::create_workset(smelt_git::worktree::CreateWorkset {
        project_root: PathBuf::from(&params.project_root),
        task: params.task,
        branch,
        base: params.base.clone(),
        repositories,
    })
    .map_err(worktree_error)?;
    let record = model_workset(&workset, uuid::Uuid::new_v4().simple().to_string(), 0);
    let saved = {
        let _guard = workspace_catalog::lock_catalog();
        workspace_catalog::create_workset_on_locked(store, &record)
    };
    match saved {
        Ok(change) => {
            publish_change(&change, event_hub);
            // 工作树已经可用，再跑各仓库的初始化脚本。失败不回滚，只在回执里报。
            Ok(WorkspaceWorksetCreateResult {
                revision: change.directory.version(),
                setup: crate::worktree_setup::run_setup_scripts(&workset),
            })
        }
        Err(error) => {
            let rollback = smelt_git::worktree::abort_workset_creation(&workset);
            let mut message = format!("Couldn't save the worktree directory: {error}");
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
    let _worksets = lock_worksets();
    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    let workset = directory
        .worksets
        .iter()
        .find(|workset| workset.id == params.workset_id)
        .ok_or_else(|| {
            ControlError::because(ControlErrorCode::NotFound, ControlReason::WorktreeNotFound)
        })?;
    if directory
        .conversations
        .iter()
        .any(|conversation| conversation.workset_id.as_deref() == Some(workset.id.as_str()))
    {
        return Err(ControlError::because(
            ControlErrorCode::FailedPrecondition,
            ControlReason::WorktreeHasSessions,
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
            let mut message = format!("Couldn't update the worktree metadata: {error}");
            if let Err(rollback) = rollback {
                message.push_str(&format!("\nThe Git rollback didn't finish: {rollback}"));
            }
            Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                message,
            ))
        }
    }
}

/// 关闭前的只读检查。fetch 走网络，只占工作树操作锁，不拿目录锁。
pub(crate) fn inspect(
    store: &smelt_store::Store,
    params: WorkspaceWorksetInspectParams,
) -> Result<WorkspaceWorksetInspectResult, ControlError> {
    let _worksets = lock_worksets();
    let directory = workspace_catalog::directory_on(store).map_err(directory_error)?;
    let workset = directory
        .worksets
        .iter()
        .find(|workset| workset.id == params.workset_id)
        .ok_or_else(|| {
            ControlError::because(ControlErrorCode::NotFound, ControlReason::WorktreeNotFound)
        })?;
    let workset = git_workset(workset);
    let refresh_failures = smelt_git::worktree::refresh_workset_bases(&workset);
    let repositories = smelt_git::worktree::inspect_workset(&workset)
        .map_err(worktree_error)?
        .into_iter()
        .map(|state| WorkspaceWorksetRepositoryState {
            name: state.name,
            dirty: state.dirty,
            merge: state.merge.map(|merge| match merge {
                smelt_git::worktree::MergeState::Merged => WorkspaceWorksetMerge::Merged,
                smelt_git::worktree::MergeState::Unmerged => WorkspaceWorksetMerge::Unmerged,
                smelt_git::worktree::MergeState::Unconfirmed => WorkspaceWorksetMerge::Unconfirmed,
            }),
        })
        .collect();
    Ok(WorkspaceWorksetInspectResult {
        repositories,
        refresh_failures,
    })
}

pub(crate) fn finish(
    store: &smelt_store::Store,
    params: WorkspaceWorksetFinishParams,
    event_hub: &EventHubHandle,
) -> Result<WorkspaceWorksetFinishResult, ControlError> {
    // 工作树操作锁在 fetch 前拿：fetch 期间别的工作树操作排队，但不占目录锁。
    let _worksets = lock_worksets();
    // 先在锁外检查会话、fetch 基线：fetch 走网络，拿着目录锁会卡住所有目录写入。
    let directory = workspace_catalog::directory_on(store).map_err(directory_error)?;
    let workset = closable_workset(&directory, &params.workset_id)?;
    let refresh_failures = smelt_git::worktree::refresh_workset_bases(&git_workset(workset));

    let _guard = workspace_catalog::lock_catalog();
    let directory = workspace_catalog::directory_on_locked(store).map_err(directory_error)?;
    let workset = closable_workset(&directory, &params.workset_id)?;
    let report =
        smelt_git::worktree::finish_workset(&git_workset(workset), params.allow_unconfirmed_merge)
            .map_err(|error| {
                let mut error = worktree_error(error);
                if !refresh_failures.is_empty() {
                    error.message = format!(
                        "{}\nCouldn't refresh the remotes, so this used the remote branches already here:\n{}",
                        error.message,
                        refresh_failures.join("\n")
                    );
                }
                error
            })?;
    let change =
        workspace_catalog::delete_workset_on_locked(store, &workset.id).map_err(|error| {
            ControlError::because(
                ControlErrorCode::OperationFailed,
                ControlReason::WorktreeRecordNotDeleted {
                    error: error.to_string(),
                },
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

fn closable_workset<'a>(
    directory: &'a WorkspaceDirectory,
    id: &str,
) -> Result<&'a WorkspaceWorkset, ControlError> {
    let workset = directory
        .worksets
        .iter()
        .find(|workset| workset.id == id)
        .ok_or_else(|| {
            ControlError::because(ControlErrorCode::NotFound, ControlReason::WorktreeNotFound)
        })?;
    if directory.conversations.iter().any(|conversation| {
        conversation.workset_id.as_deref() == Some(id) && conversation.archived_at.is_none()
    }) {
        return Err(ControlError::because(
            ControlErrorCode::FailedPrecondition,
            ControlReason::WorktreeHasOpenSessions,
        ));
    }
    Ok(workset)
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
    // 带种类的照种类说；`message` 仍是整句加上回滚没做完的细节，给老客户端和日志。
    ControlError {
        reason: error.reason().cloned(),
        ..ControlError::new(ControlErrorCode::OperationFailed, error.to_string())
    }
}
