//! 丢弃改动的「所见即所丢」校验：请求带着用户看到的那份快照（状态码 + 工作区内容
//! 指纹），运行时丢弃前从磁盘重算，对不上就拒绝。场景是 agent 在同一个工作区里
//! 接着写：用户点丢弃时，agent 刚写进去的内容、用户没见过的新文件都不能跟着丢。
//! 这些都跑真 git。

use std::path::Path;

use smelt_git::{discard_all, discard_file, load_git_status, run_git};

/// 造一个仓库：`a.txt` 提交为 `one\n`。返回 (临时目录守卫, 仓库根)。
fn committed_repo() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap().to_string();
    run_git(&root, &["init", "-q"]).unwrap();
    // Windows 的 Git 默认把工作区换行转成 CRLF。这里要比对提交里的原始字节。
    run_git(&root, &["config", "core.autocrlf", "false"]).unwrap();
    run_git(&root, &["config", "user.email", "t@t"]).unwrap();
    run_git(&root, &["config", "user.name", "t"]).unwrap();
    std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();
    run_git(&root, &["add", "a.txt"]).unwrap();
    run_git(&root, &["commit", "-qm", "seed"]).unwrap();
    (tmp, root)
}

fn read(root: &str, path: &str) -> String {
    std::fs::read_to_string(Path::new(root).join(path)).unwrap()
}

#[test]
fn discard_file_refuses_tracked_file_rewritten_after_view() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    let seen = load_git_status(&root).seen_change("a.txt").unwrap();

    // 用户看完列表之后，agent 又往同一个文件里写了东西。
    std::fs::write(Path::new(&root).join("a.txt"), "agent\n").unwrap();
    let err = discard_file(&root, &seen).expect_err("内容变了必须拒绝");
    assert!(err.contains("a.txt") && err.contains("刷新"), "{err}");
    assert_eq!(read(&root, "a.txt"), "agent\n", "被拒绝时一个字节都不能动");

    // 刷新后看到的是新内容，再丢就照常执行。
    let seen = load_git_status(&root).seen_change("a.txt").unwrap();
    discard_file(&root, &seen).unwrap();
    assert_eq!(read(&root, "a.txt"), "one\n");
}

#[test]
fn discard_file_refuses_untracked_file_rewritten_after_view() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("new.txt"), "draft\n").unwrap();
    let seen = load_git_status(&root).seen_change("new.txt").unwrap();
    assert!(seen.untracked());

    std::fs::write(Path::new(&root).join("new.txt"), "draft\nagent\n").unwrap();
    discard_file(&root, &seen).expect_err("未跟踪文件内容变了必须拒绝");
    assert_eq!(read(&root, "new.txt"), "draft\nagent\n");

    let seen = load_git_status(&root).seen_change("new.txt").unwrap();
    discard_file(&root, &seen).unwrap();
    assert!(!Path::new(&root).join("new.txt").exists());
}

/// 状态码变了（比如 agent 把它 add 进了暂存区）也算用户没见过。
#[test]
fn discard_file_refuses_when_status_code_changed_after_view() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    let seen = load_git_status(&root).seen_change("a.txt").unwrap();

    run_git(&root, &["add", "a.txt"]).unwrap();
    discard_file(&root, &seen).expect_err("状态码变了必须拒绝");
    assert_eq!(read(&root, "a.txt"), "user\n");
}

/// 文件已经不在改动列表里（被别人提交或还原了）：没什么可丢，也不该报成功。
#[test]
fn discard_file_refuses_path_that_is_no_longer_changed() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    let seen = load_git_status(&root).seen_change("a.txt").unwrap();

    run_git(&root, &["commit", "-qam", "agent committed"]).unwrap();
    discard_file(&root, &seen).expect_err("快照里的改动已经不存在");
    assert_eq!(read(&root, "a.txt"), "user\n");
}

#[test]
fn discard_all_refuses_when_new_untracked_file_appeared_after_view() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    let seen = load_git_status(&root).seen_changes();

    // 用户从没见过的新文件：`git clean -fd` 会把它连根删掉。
    std::fs::write(Path::new(&root).join("agent.txt"), "fresh\n").unwrap();
    let err = discard_all(&root, &seen).expect_err("多出没见过的文件必须拒绝");
    assert!(err.contains("agent.txt") && err.contains("刷新"), "{err}");
    assert_eq!(read(&root, "agent.txt"), "fresh\n");
    assert_eq!(read(&root, "a.txt"), "user\n", "被拒绝时已跟踪文件也不能动");
}

#[test]
fn discard_all_refuses_when_listed_file_rewritten_after_view() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    std::fs::write(Path::new(&root).join("b.txt"), "b\n").unwrap();
    let seen = load_git_status(&root).seen_changes();

    std::fs::write(Path::new(&root).join("a.txt"), "agent\n").unwrap();
    discard_all(&root, &seen).expect_err("列表里的文件内容变了必须拒绝");
    assert_eq!(read(&root, "a.txt"), "agent\n");
    assert_eq!(read(&root, "b.txt"), "b\n");
}

#[test]
fn discard_all_discards_exactly_what_was_seen() {
    let (_tmp, root) = committed_repo();
    std::fs::write(Path::new(&root).join("a.txt"), "user\n").unwrap();
    std::fs::create_dir_all(Path::new(&root).join("dir")).unwrap();
    std::fs::write(Path::new(&root).join("dir/new.txt"), "n\n").unwrap();
    let seen = load_git_status(&root).seen_changes();

    discard_all(&root, &seen).unwrap();
    assert_eq!(read(&root, "a.txt"), "one\n");
    assert!(!Path::new(&root).join("dir/new.txt").exists());
    assert!(load_git_status(&root).files.is_empty());
}

/// 工作区一侧已删除的文件没有内容可指纹；它被重新写出来就是用户没见过的内容。
#[test]
fn deleted_file_recreated_after_view_is_refused() {
    let (_tmp, root) = committed_repo();
    std::fs::remove_file(Path::new(&root).join("a.txt")).unwrap();
    let seen = load_git_status(&root).seen_change("a.txt").unwrap();
    assert_eq!(seen.worktree_hash, None);

    std::fs::write(Path::new(&root).join("a.txt"), "agent\n").unwrap();
    discard_file(&root, &seen).expect_err("重新写出来的内容用户没见过");
    assert_eq!(read(&root, "a.txt"), "agent\n");
}

/// 路径按字面匹配：`[id].tsx` 当 glob 会把 `i.tsx` 的改动一起还原掉，那条用户根本没点。
#[test]
fn discard_file_treats_path_literally_not_as_glob() {
    let (_tmp, root) = committed_repo();
    for name in ["[id].tsx", "i.tsx"] {
        std::fs::write(Path::new(&root).join(name), "seed\n").unwrap();
    }
    run_git(&root, &["add", "-A"]).unwrap();
    run_git(&root, &["commit", "-qm", "pages"]).unwrap();
    for name in ["[id].tsx", "i.tsx"] {
        std::fs::write(Path::new(&root).join(name), "edit\n").unwrap();
    }

    let seen = load_git_status(&root).seen_change("[id].tsx").unwrap();
    discard_file(&root, &seen).unwrap();
    assert_eq!(read(&root, "[id].tsx"), "seed\n");
    assert_eq!(
        read(&root, "i.tsx"),
        "edit\n",
        "没点的文件不能被 glob 连带还原"
    );
}
