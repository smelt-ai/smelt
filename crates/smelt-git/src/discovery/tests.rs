//! 发现器测试。
//!
//! 全部跑真 `git`：这些用例要验证的正是「git 自己怎么看这些目录」——
//! `.gitignore` 的遮挡、`ignore = all` 的 submodule、未初始化的 gitlink。
//! 用 mock 子进程只会把我们自己的假设测一遍，测不出真实行为。

use super::*;
use smelt_core::fs::LocalFs;
use smelt_core::subprocess::LocalSubprocess;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        // 不让本机 ~/.gitconfig 影响结果（模板、默认分支名、hooks）。
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 建一个有一次提交的仓库。
fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("seed.txt"), "seed\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
}

fn run(root: &Path) -> Discovery {
    discover(
        &LocalFs,
        &LocalSubprocess,
        root,
        &DiscoveryOptions::default(),
    )
}

fn run_with(root: &Path, options: &DiscoveryOptions) -> Discovery {
    discover(&LocalFs, &LocalSubprocess, root, options)
}

fn rels(discovery: &Discovery) -> Vec<(&str, RepoKind)> {
    discovery
        .repos
        .iter()
        .map(|r| (r.rel_path.as_str(), r.kind))
        .collect()
}

/// 被 `.gitignore` 忽略的独立仓库：父仓 `git status` 一个字都不报，
/// 旧的「从父仓 status 展开」模型结构上永远发现不了它。
#[test]
fn finds_repository_hidden_by_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "-qm", "ignore vendor"]);
    init_repo(&root.join("vendor").join("lib"));

    let found = run(root);
    assert!(
        rels(&found).contains(&("vendor/lib", RepoKind::Nested)),
        "被 .gitignore 忽略的仓库必须能发现，实际: {:?}",
        rels(&found)
    );
}

/// 工作区干净的 submodule 也要列出：它是一个可提交单位，
/// 「当前没改动」跟「不存在」是两回事。
#[test]
fn lists_clean_submodule() {
    let tmp = tempfile::tempdir().unwrap();
    let upstream = tmp.path().join("upstream");
    init_repo(&upstream);
    let root = tmp.path().join("parent");
    init_repo(&root);
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "sub",
        ],
    );
    git(&root, &["commit", "-qm", "add sub"]);

    let found = run(&root);
    assert!(
        rels(&found).contains(&("sub", RepoKind::Submodule)),
        "干净的 submodule 也要列出，实际: {:?}",
        rels(&found)
    );
}

/// `.gitmodules` 里声明、但还没 `submodule update` 的条目不是仓库。
///
/// 它们没有 `.git`，跑不了任何 git 命令，既不能提交也不能看 diff。列出来只会
/// 占满 `max_repos` 配额，把真正能操作的仓库挤掉——13 个 submodule 里有 7 个
/// 没检出的项目，正是这么被挤没的。
#[test]
fn skips_uninitialized_submodule() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    std::fs::write(
        root.join(".gitmodules"),
        "[submodule \"ps-basic-api\"]\n\tpath = ps-basic-api\n\turl = https://example.invalid/x.git\n\tignore = all\n",
    )
    .unwrap();

    let found = run(root);
    assert!(
        !rels(&found).iter().any(|(p, _)| *p == "ps-basic-api"),
        "没检出的 submodule 不该出现在仓库列表里，实际: {:?}",
        rels(&found)
    );
}

/// 项目根本身不是仓库、但它的父目录是仓库时，绝不能把父仓库拖进来。
/// 对应 VS Code 的 `openRepositoryInParentFolders`。
#[test]
fn never_opens_repository_above_project_root() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path();
    init_repo(outer);
    let project = outer.join("plain-subdir");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "a\n").unwrap();

    let found = run(&project);
    assert!(
        found.repos.is_empty(),
        "项目根之外的仓库不能被打开，实际: {:?}",
        rels(&found)
    );
}

/// 依赖目录里的仓库不是用户的工作对象。
#[test]
fn skips_ignored_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    init_repo(&root.join("node_modules").join("some-pkg"));
    init_repo(&root.join("real"));

    let found = run(root);
    let rels = rels(&found);
    assert!(
        !rels.iter().any(|(p, _)| p.starts_with("node_modules")),
        "node_modules 里的仓库不该出现，实际: {rels:?}"
    );
    assert!(rels.contains(&("real", RepoKind::Nested)), "实际: {rels:?}");
}

/// 超过上限时截断并置位标记，而不是继续扫下去。
#[test]
fn truncates_beyond_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    for i in 0..4 {
        init_repo(&root.join(format!("r{i}")));
    }

    let options = DiscoveryOptions {
        max_repos: 3,
        ..DiscoveryOptions::default()
    };
    let found = run_with(root, &options);
    assert_eq!(found.repos.len(), 3, "实际: {:?}", rels(&found));
    assert!(found.truncated, "截断后必须置位 truncated");
}

/// 深度默认看两层；调小就该找不到，调大才往下找。
#[test]
fn respects_max_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    init_repo(&root.join("a").join("b").join("deep"));

    let shallow = run_with(
        root,
        &DiscoveryOptions {
            max_depth: 1,
            ..DiscoveryOptions::default()
        },
    );
    assert!(
        !rels(&shallow).iter().any(|(p, _)| *p == "a/b/deep"),
        "深度 1 不该找到第三层，实际: {:?}",
        rels(&shallow)
    );

    let deep = run_with(
        root,
        &DiscoveryOptions {
            max_depth: 3,
            ..DiscoveryOptions::default()
        },
    );
    assert!(
        rels(&deep).contains(&("a/b/deep", RepoKind::Nested)),
        "调大深度后要能找到，实际: {:?}",
        rels(&deep)
    );
}

/// 进了仓库就不再往里面钻：子仓内部的仓库归子仓自己管，
/// 否则一个带 vendor 目录的子仓会把一堆无关仓库押进工作区列表。
#[test]
fn does_not_descend_into_discovered_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    init_repo(&root.join("child"));
    init_repo(&root.join("child").join("grandchild"));

    let found = run_with(
        root,
        &DiscoveryOptions {
            max_depth: 5,
            ..DiscoveryOptions::default()
        },
    );
    let rels = rels(&found);
    assert!(
        rels.contains(&("child", RepoKind::Nested)),
        "实际: {rels:?}"
    );
    assert!(
        !rels.iter().any(|(p, _)| *p == "child/grandchild"),
        "不该钻进已确认的仓库内部，实际: {rels:?}"
    );
}

/// 项目根恒在第一位，其余顺序稳定，不泄漏来源的注册顺序。
#[test]
fn project_root_comes_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    init_repo(&root.join("zzz"));
    init_repo(&root.join("aaa"));

    let found = run(root);
    assert_eq!(
        rels(&found),
        vec![
            ("", RepoKind::Root),
            ("aaa", RepoKind::Nested),
            ("zzz", RepoKind::Nested),
        ]
    );
}

/// 同一个仓库被多条来源报告时只出现一次，且保留更精确的 kind。
#[test]
fn deduplicates_across_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let upstream = tmp.path().join("upstream");
    init_repo(&upstream);
    let root = tmp.path().join("parent");
    init_repo(&root);
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "sub",
        ],
    );
    git(&root, &["commit", "-qm", "add sub"]);

    let found = run(&root);
    let subs: Vec<_> = found.repos.iter().filter(|r| r.rel_path == "sub").collect();
    assert_eq!(subs.len(), 1, "同一仓库只能出现一次: {subs:?}");
    assert_eq!(
        subs[0].kind,
        RepoKind::Submodule,
        "submodule 来源比通用扫描更精确，kind 要保留 Submodule"
    );
}
