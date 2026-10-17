//! git 命令层回归：diff 解析出的单块 patch 必须真能被 `git apply` 吃下，worktree
//! 的建、列、删按预期工作。这些都跑真 git，原先在 GUI 的 git 面板测试里。

use std::path::Path;

use smelt_git::{
    DiffScope, create_worktree, file_diff, parse_diff, parse_worktree_list, run_git, run_git_stdin,
};
use smelt_runtime_client::protocol::git::hunk_patch;

/// 在临时目录里造一个仓库：写 `content`、提交，再覆写成 `modified`（不提交）。
/// 返回仓库根路径。用 pid + 标签避免并行测试互相踩。
fn repo_with_change(tag: &str, content: &str, modified: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("smelt-git-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let r = root.to_str().unwrap();
    run_git(r, &["init", "-q"]).unwrap();
    run_git(r, &["config", "user.email", "t@t"]).unwrap();
    run_git(r, &["config", "user.name", "t"]).unwrap();
    std::fs::write(root.join("f.txt"), content).unwrap();
    run_git(r, &["add", "-A"]).unwrap();
    run_git(r, &["commit", "-qm", "init"]).unwrap();
    std::fs::write(root.join("f.txt"), modified).unwrap();
    root
}

#[test]
fn file_diff_offers_the_worktree_file_only_while_it_is_a_regular_file() {
    let root = repo_with_change("full-file-availability", "before\n", "after\n");
    let r = root.to_str().unwrap();
    let diff = file_diff(r, "f.txt", false, DiffScope::All);
    assert_eq!(
        diff.worktree_file.as_deref(),
        Some(root.join("f.txt").to_str().unwrap())
    );
    assert!(diff.text.contains("+after"));

    std::fs::remove_file(root.join("f.txt")).unwrap();
    let diff = file_diff(r, "f.txt", false, DiffScope::All);
    assert_eq!(
        diff.worktree_file, None,
        "已删除的文件不能再提供「打开完整文件」"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 隐藏元信息行之后，单块 patch 仍要能被 git apply 接受（回归）。
#[test]
fn hunk_patch_still_applies_after_hiding_header_lines() {
    let root = repo_with_change("hide-hdr", "alpha\nbravo\n", "alpha\nCHANGED\n");
    let r = root.to_str().unwrap();
    let out = run_git(r, &["diff", "HEAD", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));
    let patch = hunk_patch(&parsed.header, &parsed.hunks[0]);
    let applied = run_git_stdin(r, &["apply", "--cached", "-"], &patch).unwrap();
    assert!(
        applied.success(),
        "隐藏元信息后 patch 反而不合法了：{}\n--- patch ---\n{patch}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 隔得够远的两处改动 → git 一定分成两个 hunk，且每段的 range 覆盖自己的行。
#[test]
fn parses_multiple_hunks_with_correct_ranges() {
    let orig: String = (1..=60).map(|i| format!("line{i}\n")).collect();
    let mut lines: Vec<String> = orig.lines().map(|l| l.to_string()).collect();
    lines[2] = "CHANGED-TOP".into();
    lines[55] = "CHANGED-BOTTOM".into();
    let modified: String = lines.iter().map(|l| format!("{l}\n")).collect();

    let root = repo_with_change("multi", &orig, &modified);
    let out = run_git(root.to_str().unwrap(), &["diff", "HEAD", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));

    assert_eq!(parsed.hunks.len(), 2, "相距 50 行的两处改动应分成两个 hunk");
    // range 不能留占位值，且必须首尾相接不重叠
    for h in &parsed.hunks {
        assert!(h.range.end != usize::MAX, "range.end 占位值没回填");
        assert!(h.range.start < h.range.end, "range 为空: {:?}", h.range);
    }
    assert!(
        parsed.hunks[0].range.end <= parsed.hunks[1].range.start,
        "两段 range 重叠"
    );
    // 每段原文里只该有自己那处改动
    assert!(parsed.hunks[0].raw.contains("CHANGED-TOP"));
    assert!(!parsed.hunks[0].raw.contains("CHANGED-BOTTOM"));
    assert!(parsed.hunks[1].raw.contains("CHANGED-BOTTOM"));
    assert!(!parsed.hunks[1].raw.contains("CHANGED-TOP"));
    assert!(
        parsed.header.starts_with("diff --git"),
        "header 应从 diff --git 起头"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 光比字符串不算数——生成的 patch 必须真能被 `git apply` 吃下。
/// 只暂存第一个 hunk，索引里就该只有它那处改动。
#[test]
fn single_hunk_patch_applies_to_index() {
    let orig: String = (1..=60).map(|i| format!("line{i}\n")).collect();
    let mut lines: Vec<String> = orig.lines().map(|l| l.to_string()).collect();
    lines[2] = "CHANGED-TOP".into();
    lines[55] = "CHANGED-BOTTOM".into();
    let modified: String = lines.iter().map(|l| format!("{l}\n")).collect();

    let root = repo_with_change("apply", &orig, &modified);
    let r = root.to_str().unwrap();
    let out = run_git(r, &["diff", "HEAD", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));

    let patch = hunk_patch(&parsed.header, &parsed.hunks[0]);
    let applied = run_git_stdin(r, &["apply", "--cached", "-"], &patch).unwrap();
    assert!(
        applied.success(),
        "单块 patch 被 git apply 拒绝：{}\n--- patch ---\n{patch}",
        String::from_utf8_lossy(&applied.stderr)
    );

    // 索引里只有第一处改动，第二处仍留在工作区未暂存
    let staged = run_git(r, &["diff", "--cached"]).unwrap();
    let staged = String::from_utf8_lossy(&staged.stdout);
    assert!(staged.contains("CHANGED-TOP"), "第一块没进索引");
    assert!(!staged.contains("CHANGED-BOTTOM"), "第二块不该被一起暂存");
    let _ = std::fs::remove_dir_all(&root);
}

/// 末行无换行符时 git 会吐 `\ No newline at end of file`。这行不进 DiffLine，
/// 若 patch 从渲染结果反拼就会丢，apply 直接报 corrupt——必须留在 raw 里。
#[test]
fn patch_keeps_no_newline_marker() {
    let root = repo_with_change("nonewline", "alpha\n", "beta");
    let r = root.to_str().unwrap();
    let out = run_git(r, &["diff", "HEAD", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));

    let patch = hunk_patch(&parsed.header, &parsed.hunks[0]);
    assert!(
        patch.contains("\\ No newline at end of file"),
        "patch 丢了无换行标记:\n{patch}"
    );
    let applied = run_git_stdin(r, &["apply", "--cached", "-"], &patch).unwrap();
    assert!(
        applied.success(),
        "无换行结尾的 patch 被拒绝：{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 「已暂存」视图下取消暂存一块：patch 来自 `git diff --staged`，
/// `apply --cached --reverse` 应把它退出索引，而工作区文件不受影响。
#[test]
fn unstage_hunk_patch_removes_it_from_index_only() {
    let root = repo_with_change("unstage", "alpha\nbravo\n", "alpha\nCHANGED\n");
    let r = root.to_str().unwrap();
    run_git(r, &["add", "-A"]).unwrap();

    // 已暂存视图的 diff，正是索引相对 HEAD 的差异
    let out = run_git(r, &["diff", "--staged", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(parsed.hunks.len(), 1);

    let patch = hunk_patch(&parsed.header, &parsed.hunks[0]);
    let applied = run_git_stdin(r, &["apply", "--cached", "--reverse", "-"], &patch).unwrap();
    assert!(
        applied.success(),
        "取消暂存的 patch 被拒绝：{}",
        String::from_utf8_lossy(&applied.stderr)
    );

    // 索引已回到 HEAD，但工作区仍是改过的内容
    let staged = run_git(r, &["diff", "--staged"]).unwrap();
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "索引没退干净"
    );
    let text = std::fs::read_to_string(root.join("f.txt")).unwrap();
    assert_eq!(text, "alpha\nCHANGED\n", "取消暂存不该动工作区文件");
    let _ = std::fs::remove_dir_all(&root);
}

/// 丢弃块走 `apply --reverse`（作用于工作区）：改动应从文件里消失。
#[test]
fn reverse_patch_discards_change_in_worktree() {
    let root = repo_with_change("reverse", "alpha\nbravo\n", "alpha\nCHANGED\n");
    let r = root.to_str().unwrap();
    let out = run_git(r, &["diff", "HEAD", "--", "f.txt"]).unwrap();
    let parsed = parse_diff(&String::from_utf8_lossy(&out.stdout));

    let patch = hunk_patch(&parsed.header, &parsed.hunks[0]);
    let applied = run_git_stdin(r, &["apply", "--reverse", "-"], &patch).unwrap();
    assert!(
        applied.success(),
        "reverse apply 失败：{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let text = std::fs::read_to_string(root.join("f.txt")).unwrap();
    assert_eq!(text, "alpha\nbravo\n", "工作区没被还原");
    let _ = std::fs::remove_dir_all(&root);
}

/// porcelain 输出：主工作树 + 普通分支 worktree + detached + prunable + locked。
const PORCELAIN_SAMPLE: &str = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo-wt-a
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feature/x

worktree /repo-wt-detached
HEAD 3333333333333333333333333333333333333333
detached

worktree /repo-wt-locked
HEAD 4444444444444444444444444444444444444444
branch refs/heads/locked-branch
locked reason

worktree /repo-wt-stale
HEAD 5555555555555555555555555555555555555555
branch refs/heads/stale-branch
prunable gitdir file points to non-existent location
";

#[test]
fn worktree_list_parses_branch_detached_locked_prunable() {
    let entries = parse_worktree_list(PORCELAIN_SAMPLE, Some("/repo"));
    assert_eq!(entries.len(), 5);
    assert_eq!(
        entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        vec![
            "/repo",
            "/repo-wt-a",
            "/repo-wt-detached",
            "/repo-wt-locked",
            "/repo-wt-stale",
        ]
    );
    assert!(entries[0].is_main, "show-toplevel 命中的第一条应是主工作树");
    assert!(!entries[1].is_main);
    assert_eq!(entries[1].branch.as_deref(), Some("feature/x"));
    assert_eq!(entries[2].branch, None, "detached HEAD 没有分支名");
    assert!(entries[3].locked);
    assert!(!entries[3].prunable);
    assert!(entries[4].prunable);
    assert!(!entries[4].locked);
}

#[test]
fn worktree_list_marks_main_by_toplevel_not_position() {
    // show-toplevel 没命中任何条目时，主工作树标记应该全 false（不靠位置猜）。
    let entries = parse_worktree_list(PORCELAIN_SAMPLE, Some("/elsewhere"));
    assert!(entries.iter().all(|e| !e.is_main));
    // None（探测失败）同理，不能把第一条误标成主工作树。
    let entries = parse_worktree_list(PORCELAIN_SAMPLE, None);
    assert!(entries.iter().all(|e| !e.is_main));
}

/// 建一个带一次 commit 的干净仓库（worktree 测试用，不带未提交改动）。
fn repo_with_initial_commit(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("smelt-wt-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let r = root.to_str().unwrap();
    run_git(r, &["init", "-q"]).unwrap();
    run_git(r, &["config", "user.email", "t@t"]).unwrap();
    run_git(r, &["config", "user.name", "t"]).unwrap();
    std::fs::write(root.join("f.txt"), "alpha\n").unwrap();
    run_git(r, &["add", "-A"]).unwrap();
    run_git(r, &["commit", "-qm", "init"]).unwrap();
    root
}

/// `create_worktree`：新分支走 `-b` 新建；已存在分支若未被检出则直接检出，
/// 已被其他 worktree 检出则明确报错；空分支名走 detached。
#[test]
fn create_worktree_handles_new_existing_and_detached() {
    let main = repo_with_initial_commit("create-wt");
    let m = main.to_str().unwrap();
    let base = std::env::temp_dir().join(format!("smelt-wt-test-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let (wt1, wt2, wt3) = (base.join("wt-a"), base.join("wt-b"), base.join("wt-c"));

    // 新分支：`-b` 新建并检出
    let created = create_worktree(m, Some("feature/x"), wt1.to_str().unwrap()).unwrap();
    assert_eq!(Path::new(&created), wt1);
    let head = run_git(
        wt1.to_str().unwrap(),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "feature/x",
        "新 worktree 应检出新建的分支"
    );

    // 分支已被 wt1 检出：拒绝重复检出，给明确报错而不是 git 的 fatal
    let err = create_worktree(m, Some("feature/x"), wt2.to_str().unwrap())
        .expect_err("同一分支不能被两个 worktree 检出");
    assert!(
        err.contains("已被其他 worktree 检出"),
        "报错应说明原因：{err}"
    );

    // 删掉 wt1 后分支仍在但未被检出：直接检出成功
    let remove = run_git(m, &["worktree", "remove", "--force", wt1.to_str().unwrap()]).unwrap();
    assert!(remove.success());
    create_worktree(m, Some("feature/x"), wt2.to_str().unwrap()).unwrap();
    let head2 = run_git(
        wt2.to_str().unwrap(),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&head2.stdout).trim(), "feature/x");

    // 空分支名：detached HEAD
    create_worktree(m, None, wt3.to_str().unwrap()).unwrap();
    let head3 = run_git(
        wt3.to_str().unwrap(),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&head3.stdout).trim(), "HEAD");

    for wt in [&wt2, &wt3] {
        let out = run_git(m, &["worktree", "remove", "--force", wt.to_str().unwrap()]).unwrap();
        assert!(out.success(), "清理 worktree 失败");
    }
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&main);
}
