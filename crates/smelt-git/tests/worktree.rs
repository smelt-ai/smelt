use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use smelt_git::worktree::{
    CreateWorkset, Repository, create_workset, finish_workset, rename_workset,
};

fn git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn repo(project: &Path, name: &str) -> Repository {
    let root = project.join("repos").join(name);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--initial-branch=main"]);
    git(&root, &["config", "user.name", "Worktree Test"]);
    git(&root, &["config", "user.email", "worktree@example.invalid"]);
    fs::write(root.join("seed.txt"), "seed\n").unwrap();
    git(&root, &["add", "seed.txt"]);
    git(&root, &["commit", "-m", "seed"]);

    let remote = project.join("remotes").join(format!("{name}.git"));
    fs::create_dir_all(remote.parent().unwrap()).unwrap();
    git(project, &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&root, &["push", "-u", "origin", "main"]);
    git(&root, &["remote", "set-head", "origin", "main"]);
    Repository {
        name: name.to_string(),
        root,
    }
}

fn request(project: &Path, repositories: Vec<Repository>) -> CreateWorkset {
    CreateWorkset {
        project_root: project.to_path_buf(),
        task: "workspace-session-binding".to_string(),
        branch: "feat/workspace-session-binding".to_string(),
        repositories,
    }
}

#[test]
fn creates_and_finishes_chinese_task_with_custom_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let mut request = request(project, vec![repo(project, "api"), repo(project, "web")]);
    request.task = "搜索 功能".into();
    request.branch = "fix/search-box".into();

    let workset = create_workset(request).unwrap();
    assert_eq!(workset.task, "搜索 功能");
    assert_eq!(workset.branch, "fix/search-box");
    for repository in &workset.repositories {
        assert!(
            repository
                .worktree
                .ends_with(format!("搜索 功能/{}", repository.name))
        );
        assert_eq!(
            String::from_utf8_lossy(
                &git(&repository.worktree, &["branch", "--show-current"]).stdout
            ),
            "fix/search-box\n"
        );
    }
    finish_workset(&workset).unwrap();
    assert!(!project.join("worktrees/搜索 功能").exists());
}

#[test]
fn supports_chinese_branch_and_renaming_to_a_chinese_task() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let mut request = request(project, vec![repo(project, "api")]);
    request.task = "搜索".into();
    request.branch = "feat/搜索".into();

    let workset = create_workset(request).unwrap();
    let renamed = rename_workset(&workset, "搜索 改进").unwrap();
    assert_eq!(renamed.branch, "feat/搜索");
    assert!(renamed.repositories[0].worktree.exists());
    finish_workset(&renamed).unwrap();
}

#[test]
fn rejects_invalid_custom_branch_without_creating_a_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let mut request = request(project, vec![repo(project, "api")]);
    request.task = "任务".into();
    request.branch = "invalid branch".into();

    assert!(create_workset(request).is_err());
    assert!(!project.join("worktrees/任务").exists());
}

#[test]
fn creates_each_selected_repo_on_the_shared_prefixed_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let repositories = vec![repo(project, "api"), repo(project, "web")];

    let workset = create_workset(request(project, repositories)).unwrap();

    assert_eq!(workset.branch, "feat/workspace-session-binding");
    for repository in &workset.repositories {
        assert_eq!(
            String::from_utf8_lossy(
                &git(&repository.worktree, &["branch", "--show-current"]).stdout
            ),
            "feat/workspace-session-binding\n"
        );
        assert_eq!(
            fs::canonicalize(&repository.worktree).unwrap(),
            fs::canonicalize(project)
                .unwrap()
                .join("worktrees/workspace-session-binding")
                .join(&repository.name)
        );
    }
}

#[test]
fn canonicalized_paths_support_creation_rename_and_finish() {
    let temp = tempfile::Builder::new()
        .prefix("smelt worktree ")
        .tempdir()
        .unwrap();
    let mut repository = repo(temp.path(), "api");
    repository.root = fs::canonicalize(&repository.root).unwrap();
    let project = fs::canonicalize(temp.path()).unwrap();

    let workset = create_workset(request(&project, vec![repository])).unwrap();
    let renamed = rename_workset(&workset, "renamed task").unwrap();
    assert!(renamed.repositories[0].worktree.join("seed.txt").exists());

    finish_workset(&renamed).unwrap();
    assert!(!project.join("worktrees/renamed task").exists());
}

#[test]
fn creates_workset_when_project_itself_is_a_git_repository() {
    let temp = tempfile::tempdir().unwrap();
    let source_project = temp.path();
    let repository = repo(source_project, "api");
    let project = repository.root.clone();
    let workset = create_workset(request(&project, vec![repository.clone()])).unwrap();
    assert!(workset.repositories[0].worktree.join("seed.txt").exists());
    assert!(
        String::from_utf8_lossy(&git(&project, &["status", "--porcelain"]).stdout)
            .trim()
            .is_empty()
    );
}

#[test]
fn preflights_all_destinations_before_creating_any_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    let web = repo(project, "web");
    fs::create_dir_all(project.join("worktrees/workspace-session-binding/web")).unwrap();

    assert!(create_workset(request(project, vec![api.clone(), web.clone()])).is_err());
    assert!(
        !project
            .join("worktrees/workspace-session-binding/api")
            .exists()
    );
    assert!(
        !String::from_utf8_lossy(&git(&api.root, &["branch", "--list"]).stdout)
            .contains("feat/workspace-session-binding")
    );
}

#[test]
fn finish_refuses_the_whole_workset_if_any_repo_is_dirty() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(
        project,
        vec![repo(project, "api"), repo(project, "web")],
    ))
    .unwrap();
    let changed = &workset.repositories[1].worktree;
    fs::write(changed.join("new.txt"), "uncommitted\n").unwrap();

    assert!(finish_workset(&workset).is_err());
    assert!(
        workset
            .repositories
            .iter()
            .all(|repository| repository.worktree.exists())
    );
}

#[test]
fn finish_refuses_the_whole_workset_if_any_created_branch_is_unmerged() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(
        project,
        vec![repo(project, "api"), repo(project, "web")],
    ))
    .unwrap();
    let changed = &workset.repositories[1].worktree;
    fs::write(changed.join("feature.txt"), "feature\n").unwrap();
    git(changed, &["add", "feature.txt"]);
    git(changed, &["commit", "-m", "feature"]);

    assert!(finish_workset(&workset).is_err());
    assert!(
        workset
            .repositories
            .iter()
            .all(|repository| repository.worktree.exists())
    );
}

#[test]
fn finish_preserves_a_reused_branch_but_removes_workset_created_branches() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    let web = repo(project, "web");
    git(&api.root, &["branch", "feat/workspace-session-binding"]);

    let workset = create_workset(request(project, vec![api.clone(), web.clone()])).unwrap();
    finish_workset(&workset).unwrap();

    assert!(api.root.join(".git").exists());
    assert!(
        String::from_utf8_lossy(&git(&api.root, &["branch", "--list"]).stdout)
            .contains("feat/workspace-session-binding")
    );
    assert!(
        !String::from_utf8_lossy(&git(&web.root, &["branch", "--list"]).stdout)
            .contains("feat/workspace-session-binding")
    );
    assert!(!project.join("worktrees/workspace-session-binding").exists());
}

#[test]
fn rename_moves_worktree_directories_and_preserves_shared_branches() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(
        project,
        vec![repo(project, "api"), repo(project, "web")],
    ))
    .unwrap();

    let renamed = rename_workset(&workset, "new-name").unwrap();

    assert_eq!(renamed.task, "new-name");
    assert_eq!(renamed.branch, "feat/workspace-session-binding");
    for repository in &renamed.repositories {
        assert!(
            !project
                .join("worktrees/workspace-session-binding")
                .join(&repository.name)
                .exists()
        );
        assert!(repository.worktree.exists());
        assert_eq!(
            String::from_utf8_lossy(
                &git(&repository.worktree, &["branch", "--show-current"]).stdout
            ),
            "feat/workspace-session-binding\n"
        );
    }
}

#[test]
fn rename_refuses_if_any_repo_is_dirty_before_changing_anything() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(
        project,
        vec![repo(project, "api"), repo(project, "web")],
    ))
    .unwrap();
    fs::write(
        workset.repositories[1].worktree.join("dirty.txt"),
        "uncommitted\n",
    )
    .unwrap();

    assert!(rename_workset(&workset, "new-name").is_err());
    for repository in &workset.repositories {
        assert!(repository.worktree.exists());
        assert_eq!(
            String::from_utf8_lossy(
                &git(&repository.worktree, &["branch", "--show-current"]).stdout
            ),
            "feat/workspace-session-binding\n"
        );
    }
}

#[test]
fn rename_preserves_custom_branch_even_when_the_new_task_has_a_matching_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    let web = repo(project, "web");
    git(&web.root, &["branch", "feat/new-name"]);
    let mut request = request(project, vec![api, web]);
    request.task = "旧任务".into();
    request.branch = "fix/independent".into();
    let workset = create_workset(request).unwrap();

    let renamed = rename_workset(&workset, "new-name").unwrap();
    assert_eq!(renamed.branch, "fix/independent");
    for repository in &renamed.repositories {
        assert!(repository.worktree.exists());
        assert_eq!(
            String::from_utf8_lossy(
                &git(&repository.worktree, &["branch", "--show-current"]).stdout
            ),
            "fix/independent\n"
        );
    }
    finish_workset(&renamed).unwrap();
}
