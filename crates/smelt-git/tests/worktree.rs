use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use smelt_git::worktree::{
    CreateWorkset, MergeState, Repository, create_workset, finish_workset, inspect_workset,
    refresh_workset_bases, rename_workset,
};
use smelt_runtime_client::protocol::reason::ControlReason;

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
        base: None,
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
    finish_workset(&workset, false).unwrap();
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
    finish_workset(&renamed, false).unwrap();
}

#[test]
fn creates_from_a_named_remote_base_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    git(&api.root, &["checkout", "-b", "iter/big"]);
    fs::write(api.root.join("big.txt"), "big\n").unwrap();
    git(&api.root, &["add", "big.txt"]);
    git(&api.root, &["commit", "-m", "big"]);
    git(&api.root, &["push", "origin", "iter/big"]);
    // 本地分支落后于远端：应从远端拉出。
    git(&api.root, &["reset", "--hard", "HEAD~1"]);
    git(&api.root, &["checkout", "main"]);

    let mut request = request(project, vec![api]);
    request.base = Some("iter/big".into());
    let workset = create_workset(request).unwrap();
    let repository = &workset.repositories[0];
    assert_eq!(repository.base_ref, "refs/remotes/origin/iter/big");
    assert!(repository.worktree.join("big.txt").exists());
}

#[test]
fn creates_from_a_local_only_base_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    git(&api.root, &["checkout", "-b", "iter/local"]);
    fs::write(api.root.join("local.txt"), "local\n").unwrap();
    git(&api.root, &["add", "local.txt"]);
    git(&api.root, &["commit", "-m", "local"]);
    git(&api.root, &["checkout", "main"]);

    let mut request = request(project, vec![api]);
    request.base = Some("iter/local".into());
    let workset = create_workset(request).unwrap();
    let repository = &workset.repositories[0];
    assert_eq!(repository.base_ref, "refs/heads/iter/local");
    assert!(repository.worktree.join("local.txt").exists());
    // 分支没改动，视为已合进基线，能直接关。
    finish_workset(&workset, false).unwrap();
}

#[test]
fn accepts_a_remote_qualified_base_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    git(&api.root, &["push", "origin", "main:iter/big"]);

    let mut request = request(project, vec![api]);
    request.base = Some("origin/iter/big".into());
    let workset = create_workset(request).unwrap();
    assert_eq!(
        workset.repositories[0].base_ref,
        "refs/remotes/origin/iter/big"
    );
}

#[test]
fn an_unreachable_remote_does_not_block_a_local_base_branch() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    git(&api.root, &["branch", "iter/local"]);
    fs::remove_dir_all(project.join("remotes/api.git")).unwrap();

    let mut base_request = request(project, vec![api.clone()]);
    base_request.base = Some("iter/local".into());
    let workset = create_workset(base_request).unwrap();
    assert_eq!(workset.repositories[0].base_ref, "refs/heads/iter/local");

    // 分支哪儿都没有时，报错里带上 fetch 失败的原因。
    let mut request = request(project, vec![api]);
    request.task = "other".into();
    request.branch = "feat/other".into();
    request.base = Some("iter/missing".into());
    let error = create_workset(request).unwrap_err().to_string();
    assert!(error.contains("couldn't fetch origin"), "{error}");
}

#[test]
fn rejects_a_missing_base_branch_without_creating_a_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let mut request = request(project, vec![repo(project, "api")]);
    request.base = Some("iter/missing".into());

    let error = create_workset(request).unwrap_err();
    assert_eq!(
        error.reason(),
        Some(&ControlReason::WorktreeBaseNotFound {
            repository: "api".into(),
            base: "iter/missing".into(),
        })
    );
    assert!(!project.join("worktrees/workspace-session-binding").exists());
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

    finish_workset(&renamed, false).unwrap();
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

    assert!(finish_workset(&workset, false).is_err());
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

    assert!(finish_workset(&workset, false).is_err());
    assert!(
        workset
            .repositories
            .iter()
            .all(|repository| repository.worktree.exists())
    );
}

/// 在另一个克隆里把工作树分支合进远端 main 并推上去，本地的 `origin/main` 不动，
/// 和在 GitHub 上合并 PR 一样。
fn merge_on_remote(project: &Path, name: &str, branch: &str, how: &str) {
    let remote = project.join("remotes").join(format!("{name}.git"));
    let clone = project.join("clones").join(format!("{name}-{how}"));
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    git(
        project,
        &[
            "clone",
            "--branch",
            "main",
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.name", "Remote Merger"]);
    git(&clone, &["config", "user.email", "merger@example.invalid"]);
    // 合并前 main 上先有一个别人的提交，rebase 合并才会生成新的提交。
    fs::write(clone.join("other.txt"), "other\n").unwrap();
    git(&clone, &["add", "other.txt"]);
    git(&clone, &["commit", "-m", "other work"]);
    let source = format!("origin/{branch}");
    match how {
        "merge" => {
            git(&clone, &["merge", "--no-ff", "-m", "merge", &source]);
        }
        "squash" => {
            git(&clone, &["merge", "--squash", &source]);
            git(&clone, &["commit", "-m", "squashed"]);
        }
        "rebase" => {
            git(&clone, &["cherry-pick", &format!("main..{source}")]);
        }
        _ => unreachable!(),
    }
    git(&clone, &["push", "origin", "main"]);
}

fn commit_and_push(worktree: &Path, branch: &str) {
    fs::write(worktree.join("feature.txt"), "feature\n").unwrap();
    git(worktree, &["add", "feature.txt"]);
    git(worktree, &["commit", "-m", "feature"]);
    fs::write(worktree.join("feature.txt"), "feature v2\n").unwrap();
    git(worktree, &["commit", "-am", "feature v2"]);
    git(worktree, &["push", "origin", branch]);
}

#[test]
fn finish_accepts_branches_merged_on_the_remote_by_merge_squash_or_rebase() {
    for how in ["merge", "squash", "rebase"] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let workset = create_workset(request(project, vec![repo(project, "api")])).unwrap();
        commit_and_push(&workset.repositories[0].worktree, &workset.branch);
        merge_on_remote(project, "api", &workset.branch, how);

        // 本地的远端分支还是旧的：不刷新就判成没合并。
        let stale = finish_workset(&workset, false).unwrap_err();
        assert_eq!(
            stale.reason(),
            Some(&ControlReason::WorktreeBranchUnmerged {
                repository: "api".into(),
                branch: workset.branch.clone(),
                base: "origin/main".into(),
            }),
            "{how}: {stale}"
        );

        assert!(refresh_workset_bases(&workset).is_empty(), "{how}");
        finish_workset(&workset, false).unwrap_or_else(|error| panic!("{how}: {error}"));
        assert!(!workset.repositories[0].worktree.exists(), "{how}");
        assert!(
            !String::from_utf8_lossy(
                &git(&workset.repositories[0].root, &["branch", "--list"]).stdout
            )
            .contains(&workset.branch),
            "{how}"
        );
    }
}

#[test]
fn finish_refuses_a_branch_only_partly_merged_on_the_remote() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(project, vec![repo(project, "api")])).unwrap();
    let worktree = &workset.repositories[0].worktree;
    commit_and_push(worktree, &workset.branch);
    merge_on_remote(project, "api", &workset.branch, "squash");
    fs::write(worktree.join("late.txt"), "late\n").unwrap();
    git(worktree, &["add", "late.txt"]);
    git(worktree, &["commit", "-m", "late change"]);

    assert!(refresh_workset_bases(&workset).is_empty());
    assert!(finish_workset(&workset, false).is_err());
    assert!(worktree.exists());
}

/// squash 合并之后远端又改了同一处：合进基线有冲突，判断不了是否已合并。默认拦下，
/// 用户确认后可以照关；有未提交改动的照样拦。
#[test]
fn unconfirmed_merges_block_finishing_unless_allowed() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(project, vec![repo(project, "api")])).unwrap();
    let worktree = &workset.repositories[0].worktree;
    commit_and_push(worktree, &workset.branch);
    merge_on_remote(project, "api", &workset.branch, "squash");
    let clone = project.join("clones/api-squash");
    fs::write(clone.join("feature.txt"), "feature v3\n").unwrap();
    git(&clone, &["commit", "-am", "feature v3"]);
    git(&clone, &["push", "origin", "main"]);
    assert!(refresh_workset_bases(&workset).is_empty());

    let states = inspect_workset(&workset).unwrap();
    assert_eq!(states[0].merge, Some(MergeState::Unconfirmed));
    assert!(!states[0].dirty);
    let error = finish_workset(&workset, false).unwrap_err();
    assert!(
        matches!(
            error.reason(),
            Some(ControlReason::WorktreeMergeUnconfirmed { .. })
        ),
        "{error}"
    );

    fs::write(worktree.join("scratch.txt"), "x").unwrap();
    assert!(inspect_workset(&workset).unwrap()[0].dirty);
    assert!(finish_workset(&workset, true).is_err());
    fs::remove_file(worktree.join("scratch.txt")).unwrap();

    finish_workset(&workset, true).unwrap();
    assert!(!worktree.exists());
}

#[test]
fn inspect_reports_unmerged_and_merged_branches() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(
        project,
        vec![repo(project, "api"), repo(project, "web")],
    ))
    .unwrap();
    let web = &workset.repositories[1].worktree;
    fs::write(web.join("feature.txt"), "feature\n").unwrap();
    git(web, &["add", "feature.txt"]);
    git(web, &["commit", "-m", "feature"]);

    let states = inspect_workset(&workset).unwrap();
    let merges: Vec<_> = states.iter().map(|state| state.merge).collect();
    assert_eq!(
        merges,
        [Some(MergeState::Merged), Some(MergeState::Unmerged)]
    );
    assert!(
        finish_workset(&workset, true).is_err(),
        "确实没合并的不能硬关"
    );
}

#[test]
fn refresh_reports_unreachable_remotes_without_failing() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let workset = create_workset(request(project, vec![repo(project, "api")])).unwrap();
    fs::remove_dir_all(project.join("remotes/api.git")).unwrap();

    let failures = refresh_workset_bases(&workset);
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("repository api couldn't fetch origin"),
        "{failures:?}"
    );
    finish_workset(&workset, false).unwrap();
}

#[test]
fn finish_preserves_a_reused_branch_but_removes_workset_created_branches() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path();
    let api = repo(project, "api");
    let web = repo(project, "web");
    git(&api.root, &["branch", "feat/workspace-session-binding"]);

    let workset = create_workset(request(project, vec![api.clone(), web.clone()])).unwrap();
    finish_workset(&workset, false).unwrap();

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
    finish_workset(&renamed, false).unwrap();
}
