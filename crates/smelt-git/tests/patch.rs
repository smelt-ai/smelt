//! git 命令层回归：diff 解析出的单块 patch 必须真能被 `git apply` 吃下；用户在别处建的
//! worktree 目录当普通仓库用。这些都跑真 git，原先在 GUI 的 git 面板测试里。

use smelt_git::{DiffScope, file_diff, load_repo_info, parse_diff, run_git, run_git_stdin};
use smelt_runtime_client::protocol::git::hunk_patch;

/// 在临时目录里造一个仓库：写 `content`、提交，再覆写成 `modified`（不提交）。
/// 返回仓库根路径。用 pid + 标签避免并行测试互相踩。
fn repo_with_change(tag: &str, content: &str, modified: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("smelt-git-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let r = root.to_str().unwrap();
    run_git(r, &["init", "-q"]).unwrap();
    // Windows 的 Git 默认把工作区换行转成 CRLF。这里要比对提交里的原始字节。
    run_git(r, &["config", "core.autocrlf", "false"]).unwrap();
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

/// smelt 不管 worktree：用户用 git 自己建的 worktree 目录跟普通仓库一样，只读出当前分支。
#[test]
fn repo_info_treats_a_worktree_checkout_as_an_ordinary_repo() {
    let root =
        std::env::temp_dir().join(format!("smelt-git-test-{}-repo-info", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    let m = main.to_str().unwrap();
    run_git(m, &["init", "-q"]).unwrap();
    run_git(m, &["config", "user.email", "t@t"]).unwrap();
    run_git(m, &["config", "user.name", "t"]).unwrap();
    std::fs::write(main.join("f.txt"), "alpha\n").unwrap();
    run_git(m, &["add", "-A"]).unwrap();
    run_git(m, &["commit", "-qm", "init"]).unwrap();

    let branch_wt = root.join("wt-branch");
    let detached_wt = root.join("wt-detached");
    let add = run_git(
        m,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/x",
            branch_wt.to_str().unwrap(),
        ],
    )
    .unwrap();
    assert!(add.success(), "{}", String::from_utf8_lossy(&add.stderr));
    let add = run_git(
        m,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached_wt.to_str().unwrap(),
        ],
    )
    .unwrap();
    assert!(add.success(), "{}", String::from_utf8_lossy(&add.stderr));

    assert_eq!(
        load_repo_info(branch_wt.to_str().unwrap()).unwrap().branch,
        "feature/x"
    );
    assert_eq!(
        load_repo_info(detached_wt.to_str().unwrap())
            .unwrap()
            .branch,
        "HEAD"
    );
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(load_repo_info(plain.to_str().unwrap()), None);
    let _ = std::fs::remove_dir_all(&root);
}
