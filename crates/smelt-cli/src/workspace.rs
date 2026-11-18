//! `smelt project …` 和 `smelt worktree …`：侧栏里的项目和任务工作树。
//!
//! 项目只是工作区目录里的一个路径，开、关都不碰磁盘上的文件。工作树是在项目的仓库上为一个
//! 任务开的一组 git worktree（`<项目>/worktrees/<任务>`），收工前先 `inspect` 看各仓库干不
//! 干净、分支合没合，再 `finish`。

use crate::{absolute, control, control_within, flag_value, id_arg, respond, usage};
use rust_i18n::t;
use smelt_model::control_api::{
    WorkspaceDirectoryGet, WorkspaceProjectClose, WorkspaceProjectOpen,
    WorkspaceProjectRepositoryAction, WorkspaceWorksetCreate, WorkspaceWorksetFinish,
    WorkspaceWorksetInspect,
};
use smelt_model::workspace_directory::{
    WorkspaceDirectory, WorkspaceProjectParams,
    WorkspaceProjectRepositoryAction as RepositoryAction, WorkspaceProjectRepositoryActionParams,
    WorkspaceWorksetCreateParams, WorkspaceWorksetFinishParams, WorkspaceWorksetInspectParams,
};
use smelt_runtime_client::protocol::control::EmptyParams;
use std::path::Path;
use std::time::Duration;

/// 建工作树、收工都要跑 git（fetch、建 worktree、初始化脚本），和桌面一样给足时间。
const GIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(crate) fn run_project(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(
            directory().map(|directory| serde_json::json!({ "projects": directory.projects })),
        ),
        Some(verb @ ("open" | "close")) => {
            let Some(path) = id_arg(rest) else {
                return usage(&format!("smelt project {verb} <path>"));
            };
            respond(project(verb, path))
        }
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt project list|open|close"),
    }
}

pub(crate) fn run_worktree(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => match parse_project_filter(rest) {
            Ok(project) => respond(list(project.as_deref())),
            Err(message) => usage(&message),
        },
        Some("create") => {
            let Some(project) = id_arg(rest) else {
                return usage(&t!("worktree.create_usage"));
            };
            match parse_create(&rest[1..]) {
                Ok(flags) => respond(create(project, flags)),
                Err(message) => usage(&message),
            }
        }
        Some("inspect") => match id_arg(rest) {
            Some(id) => respond(control_within::<WorkspaceWorksetInspect>(
                WorkspaceWorksetInspectParams {
                    workset_id: id.to_string(),
                },
                GIT_TIMEOUT,
            )),
            None => usage("smelt worktree inspect <id>"),
        },
        Some("finish") => {
            let Some(id) = id_arg(rest) else {
                return usage("smelt worktree finish <id> [--allow-unconfirmed-merge]");
            };
            let allow_unconfirmed_merge = match &rest[1..] {
                [] => false,
                [flag] if flag == "--allow-unconfirmed-merge" => true,
                [other, ..] => return usage(&t!("unknown_argument", arg = other)),
            };
            respond(control_within::<WorkspaceWorksetFinish>(
                WorkspaceWorksetFinishParams {
                    workset_id: id.to_string(),
                    allow_unconfirmed_merge,
                },
                GIT_TIMEOUT,
            ))
        }
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt worktree list|create|inspect|finish"),
    }
}

fn directory() -> Result<WorkspaceDirectory, String> {
    control::<WorkspaceDirectoryGet>(EmptyParams::default())
}

fn project(verb: &str, path: &str) -> Result<serde_json::Value, String> {
    let root = absolute(path)?;
    let root = if verb == "open" {
        if !Path::new(&root).is_dir() {
            return Err(t!("project.not_a_directory", path = root).into_owned());
        }
        control::<WorkspaceProjectOpen>(WorkspaceProjectParams { root: root.clone() })?;
        root
    } else {
        let root = known_project(&directory()?.projects, &root)?;
        control::<WorkspaceProjectClose>(WorkspaceProjectParams { root: root.clone() })?;
        root
    };
    Ok(serde_json::json!({ "root": root }))
}

/// 侧栏里的那个项目，按目录里的写法返回。
fn known_project(projects: &[String], root: &str) -> Result<String, String> {
    let trimmed = |path: &str| path.trim_end_matches('/').to_string();
    projects
        .iter()
        .find(|project| trimmed(project) == trimmed(root))
        .cloned()
        .ok_or_else(|| t!("project.not_open", path = root).into_owned())
}

fn parse_project_filter(args: &[String]) -> Result<Option<String>, String> {
    match args {
        [] => Ok(None),
        [flag, project] if flag == "--project" => absolute(project).map(Some),
        [flag] if flag == "--project" => Err(flag_value(args, 0).unwrap_err()),
        [other, ..] => Err(t!("unknown_argument", arg = other).into_owned()),
    }
}

fn list(project: Option<&str>) -> Result<serde_json::Value, String> {
    let directory = directory()?;
    let worktrees: Vec<_> = directory
        .worksets
        .iter()
        .filter(|workset| project.is_none_or(|root| workset.project_root == root))
        .map(|workset| {
            serde_json::json!({
                "id": workset.id,
                "project": workset.project_root,
                "task": workset.task,
                "branch": workset.branch,
                "path": workset.task_root(smelt_model::runtime_path::PathStyle::LOCAL),
                "repositories": workset
                    .repositories
                    .iter()
                    .map(|repository| &repository.name)
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(serde_json::json!({ "worktrees": worktrees }))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CreateFlags {
    task: Option<String>,
    branch: Option<String>,
    prefix: Option<String>,
    repositories: Vec<String>,
}

fn parse_create(args: &[String]) -> Result<CreateFlags, String> {
    let mut flags = CreateFlags::default();
    let mut index = 0;
    while index < args.len() {
        let value = || flag_value(args, index);
        match args[index].as_str() {
            "--task" => flags.task = Some(value()?),
            "--branch" => flags.branch = Some(value()?),
            "--prefix" => flags.prefix = Some(value()?),
            "--repo" => flags.repositories.push(value()?),
            other => return Err(t!("unknown_argument", arg = other).into_owned()),
        }
        index += 2;
    }
    if flags
        .task
        .as_deref()
        .is_none_or(|task| task.trim().is_empty())
    {
        return Err(t!("worktree.create_usage").into_owned());
    }
    Ok(flags)
}

fn create(project: &str, flags: CreateFlags) -> Result<serde_json::Value, String> {
    let project_root = known_project(&directory()?.projects, &absolute(project)?)?;
    // 和桌面的新建框一样先认一遍仓库：项目根和 `repos/` 下的 git 仓库，之前没绑的这次绑上。
    let directory = control_within::<WorkspaceProjectRepositoryAction>(
        WorkspaceProjectRepositoryActionParams {
            project_root: project_root.clone(),
            action: RepositoryAction::Discover,
        },
        GIT_TIMEOUT,
    )?;
    let available: Vec<String> = directory
        .repositories
        .iter()
        .filter(|repository| repository.project_root == project_root)
        .map(|repository| repository.name.clone())
        .collect();
    let repository_names = pick_repositories(&available, flags.repositories)?;
    let mut params = WorkspaceWorksetCreateParams {
        project_root,
        task: flags.task.unwrap_or_default(),
        branch_prefix: "feat".into(),
        branch: flags.branch,
        repository_names,
    };
    if let Some(prefix) = flags.prefix {
        params.branch_prefix = prefix;
    }
    let (project_root, task) = (params.project_root.clone(), params.task.clone());
    let created = control_within::<WorkspaceWorksetCreate>(params, GIT_TIMEOUT)?;
    // 回执只有目录版本，新树的 id 和路径从目录里找：同一个项目里任务名不重。
    let worktrees = list(Some(&project_root))?;
    let worktree = worktrees["worktrees"]
        .as_array()
        .and_then(|list| list.iter().find(|workset| workset["task"] == task.as_str()))
        .cloned();
    Ok(serde_json::json!({ "worktree": worktree, "setup": created.setup }))
}

/// 没点名就用项目的全部仓库；点名的必须是项目里有的。
fn pick_repositories(available: &[String], wanted: Vec<String>) -> Result<Vec<String>, String> {
    if available.is_empty() {
        return Err(t!("worktree.no_repositories").into_owned());
    }
    if wanted.is_empty() {
        return Ok(available.to_vec());
    }
    if let Some(missing) = wanted.iter().find(|name| !available.contains(name)) {
        return Err(t!(
            "worktree.no_such_repository",
            name = missing,
            available = available.join(", ")
        )
        .into_owned());
    }
    Ok(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn create_needs_a_task_and_collects_repositories() {
        let flags = parse_create(&args(&[
            "--task", "login", "--repo", "app", "--repo", "api", "--prefix", "fix",
        ]))
        .unwrap();
        assert_eq!(
            flags,
            CreateFlags {
                task: Some("login".into()),
                branch: None,
                prefix: Some("fix".into()),
                repositories: vec!["app".into(), "api".into()],
            }
        );
        assert!(parse_create(&args(&["--repo", "app"])).is_err());
        assert!(parse_create(&args(&["--task", " "])).is_err());
        assert!(parse_create(&args(&["--task"])).is_err());
        assert!(parse_create(&args(&["--task", "x", "--bogus", "y"])).is_err());
    }

    #[test]
    fn repositories_default_to_all_and_must_exist() {
        let available = vec!["app".to_string(), "api".to_string()];
        assert_eq!(
            pick_repositories(&available, Vec::new()).unwrap(),
            available
        );
        assert_eq!(
            pick_repositories(&available, vec!["api".into()]).unwrap(),
            ["api"]
        );
        assert!(pick_repositories(&available, vec!["web".into()]).is_err());
        assert!(pick_repositories(&[], Vec::new()).is_err());
    }

    #[test]
    fn a_known_project_ignores_a_trailing_slash() {
        let projects = vec!["/work/app".to_string()];
        assert_eq!(known_project(&projects, "/work/app/").unwrap(), "/work/app");
        assert!(known_project(&projects, "/work/other").is_err());
    }
}
