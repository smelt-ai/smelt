//! Git 面板的纯逻辑与 Git 命令回归测试。

// 不用 `use super::*;`：父模块顶部有 gpui/gpui_component 的 glob 导入，带进测试
// 模块会让 trait 解析图爆炸，`cargo test` 编译期能把 rustc 撑崩。只导入真正
// 用到的名字。
use super::RepoSet;
use super::{
    AggregateDiffRow, DiffKind, DiffReviewRow, DiffScope, FILE_LINE_H, GitDiff, GitStatusData,
    GitStatusResponseAction, PendingGitIndexOp, aggregate_all_files_collapsed,
    aggregate_diff_ready_for_file, aggregate_diff_rows, aggregate_file_body_heights,
    aggregate_file_header_paths, aggregate_file_row_index, aggregate_file_scroll_top,
    aggregate_sticky_header, aggregate_sticky_headers, build_git_tree, build_split_rows,
    clear_confirmed_git_index_ops, create_worktree, full_file_path, git_empty_list_message,
    git_index_op_in_flight, git_status_response_action, git_status_retry_delay, hunk_patch,
    insert_comment_composer, is_regular_worktree_file, parse_diff, parse_diff_with_file_headers,
    parse_worktree_list, repo_needs_attention, resolve_git_write_target, run_git, run_git_stdin,
    should_close_aggregate_diff, split_staged_and_changed, sticky_toggle_scroll_top,
    toggle_aggregate_collapse_all,
};
use smelt_git::discovery::DiscoveredRepo;
use std::path::Path;

fn files(paths: &[&str]) -> Vec<(String, String)> {
    paths
        .iter()
        .map(|p| (" M".to_string(), p.to_string()))
        .collect()
}

fn sample_status(
    branch: &str,
    upstream: Option<&str>,
    ahead: u32,
    behind: u32,
    insertions: u32,
    deletions: u32,
) -> GitStatusData {
    GitStatusData {
        ok: true,
        branch: branch.to_string(),
        upstream: upstream.map(str::to_string),
        ahead,
        behind,
        files: Vec::new(),
        stash_count: 0,
        insertions,
        deletions,
    }
}

#[test]
fn aggregate_diff_has_no_full_worktree_file() {
    assert!(full_file_path("/repo", "全部更改", true).is_none());
    assert_eq!(
        full_file_path("/repo", "src/main.rs", false).as_deref(),
        Some("/repo/src/main.rs")
    );
}

/// 只有一个孩子的目录链要压成一行，否则深路径全是空缩进。
#[test]
fn tree_compacts_single_child_dir_chains() {
    let rows = build_git_tree(&files(&["crates/smelt/src/main.rs"]), &Default::default());
    assert_eq!(rows.len(), 2, "应是一行目录 + 一行文件，实际 {rows:?}");
    assert_eq!(rows[0].name, "crates/smelt/src");
    assert_eq!(rows[0].path, "crates/smelt/src");
    assert!(rows[0].status.is_none(), "目录行不该带状态码");
    assert_eq!(rows[1].name, "main.rs");
    assert_eq!(rows[1].path, "crates/smelt/src/main.rs");
    assert_eq!(rows[1].depth, 1);
}

#[test]
fn aggregate_rows_keep_later_files_after_a_collapsed_file() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert("one.rs".to_string());
    let rows = aggregate_diff_rows(&parsed.lines, &collapsed);
    let headers: Vec<&str> = rows
        .iter()
        .filter_map(|row| match row {
            AggregateDiffRow::Header { path, .. } => Some(path.as_str()),
            AggregateDiffRow::Line(_) => None,
        })
        .collect();
    assert_eq!(headers, vec!["one.rs", "two.rs"]);
    assert!(
        rows.iter()
            .any(|row| matches!(row, AggregateDiffRow::Line(_)))
    );
}

#[test]
fn aggregate_collapse_all_toggles_every_file_header() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let paths = aggregate_file_header_paths(&parsed.lines);
    assert_eq!(paths, vec!["one.rs", "two.rs"]);
    let mut collapsed = std::collections::HashSet::new();
    assert!(!aggregate_all_files_collapsed(&paths, &collapsed));
    toggle_aggregate_collapse_all(&paths, &mut collapsed);
    assert!(aggregate_all_files_collapsed(&paths, &collapsed));
    toggle_aggregate_collapse_all(&paths, &mut collapsed);
    assert!(collapsed.is_empty());
}

#[test]
fn aggregate_file_navigation_uses_the_continuous_row_index() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let rows = aggregate_diff_rows(&parsed.lines, &Default::default());
    let row = aggregate_file_row_index(&parsed.lines, "two.rs", &Default::default())
        .expect("later file must be reachable from the aggregate list");
    assert!(matches!(
        rows[row],
        AggregateDiffRow::Header { ref path, .. } if path == "two.rs"
    ));
}

#[test]
fn invalidation_during_an_inflight_status_keeps_the_snapshot_and_refreshes_again() {
    assert_eq!(
        git_status_response_action(8, 7, true),
        GitStatusResponseAction::AcceptStaleAndRefresh,
        "持续有文件事件时也要展示最近一次成功快照，不能一直停在旧的空缓存"
    );
}

#[test]
fn failed_git_status_never_replaces_a_known_snapshot() {
    assert_eq!(
        git_status_response_action(7, 7, false),
        GitStatusResponseAction::PreserveAndRetry,
        "失败结果不是权威的干净状态，必须保留已有快照并重试"
    );
}

#[test]
fn empty_successful_status_is_clean_even_while_refreshing() {
    let clean = sample_status("main", Some("origin/main"), 0, 0, 0, 0);
    assert_eq!(
        git_empty_list_message(Some(&clean), true, 0),
        "工作区干净",
        "已有成功空快照时不能因为 inflight 一直显示正在读取"
    );
    assert!(should_close_aggregate_diff(Some(&clean), true));
}

#[test]
fn failed_empty_status_is_not_treated_as_clean() {
    let failed = GitStatusData::default();
    assert!(!failed.ok);
    assert_eq!(
        git_empty_list_message(Some(&failed), true, 1),
        "Git 状态读取失败，正在重试…"
    );
    assert!(
        !should_close_aggregate_diff(Some(&failed), true),
        "读失败的空快照不能把还在看的全部改动清掉"
    );
}

#[test]
fn missing_status_shows_loading_until_the_first_snapshot() {
    assert_eq!(git_empty_list_message(None, true, 0), "正在读取更改…");
    assert_eq!(git_empty_list_message(None, false, 0), "正在读取更改…");
    assert!(!should_close_aggregate_diff(None, true));
}

#[test]
fn git_status_failure_retries_are_bounded() {
    assert_eq!(
        git_status_retry_delay(1),
        Some(std::time::Duration::from_millis(150))
    );
    assert_eq!(
        git_status_retry_delay(3),
        Some(std::time::Duration::from_millis(600))
    );
    assert_eq!(git_status_retry_delay(4), None);
}

#[test]
fn authoritative_status_only_clears_completed_operations_for_its_root() {
    let mut pending = std::collections::HashMap::from([
        (
            ("/repo".to_string(), "running.rs".to_string()),
            PendingGitIndexOp { completed: false },
        ),
        (
            ("/repo".to_string(), "done.rs".to_string()),
            PendingGitIndexOp { completed: true },
        ),
        (
            ("/other".to_string(), "done.rs".to_string()),
            PendingGitIndexOp { completed: true },
        ),
    ]);

    clear_confirmed_git_index_ops(&mut pending, "/repo");

    assert!(pending.contains_key(&("/repo".into(), "running.rs".into())));
    assert!(!pending.contains_key(&("/repo".into(), "done.rs".into())));
    assert!(pending.contains_key(&("/other".into(), "done.rs".into())));
}

#[test]
fn stale_aggregate_is_not_ready_for_a_missing_navigation_target() {
    let parsed = parse_diff_with_file_headers(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
    );
    let diff = GitDiff {
        root: "/repo".into(),
        path: "全部更改".into(),
        aggregate: true,
        worktree_file: None,
        lines: std::rc::Rc::new(parsed.lines),
        header: String::new(),
        hunks: std::rc::Rc::new(parsed.hunks),
        patchable: false,
        scope: DiffScope::All,
        has_staged: false,
    };

    assert!(
        !aggregate_diff_ready_for_file(&diff, "/repo", DiffScope::All, "two.rs"),
        "文件树比聚合 diff 新时，不能把任意非空旧快照误判为目标已就绪"
    );
    assert!(aggregate_diff_ready_for_file(
        &diff,
        "/repo",
        DiffScope::All,
        "one.rs"
    ));
}

#[test]
fn aggregate_navigation_scroll_top_uses_the_rendered_row_heights() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);

    assert_eq!(
        aggregate_file_scroll_top(&parsed.lines, "two.rs", &Default::default(), false, None,),
        Some(FILE_LINE_H + 8.0 + FILE_LINE_H * 2.0),
        "文件标题比代码行高，不能用行号乘固定行高近似滚动位置"
    );

    assert_eq!(
        aggregate_file_scroll_top(&parsed.lines, "two.rs", &Default::default(), false, Some(2),),
        Some(FILE_LINE_H + 8.0 + FILE_LINE_H * 2.0 + super::COMMENT_COMPOSER_H),
        "目标标题在评论器之后时必须补上评论器的真实高度"
    );

    let collapsed = std::collections::HashSet::from(["one.rs".to_string()]);
    assert_eq!(
        aggregate_file_scroll_top(&parsed.lines, "two.rs", &collapsed, false, None),
        Some(FILE_LINE_H + 8.0),
        "折叠文件的正文不应继续占用导航滚动高度"
    );

    assert_eq!(
        aggregate_file_scroll_top(&parsed.lines, "two.rs", &Default::default(), true, None),
        Some(FILE_LINE_H * 2.0),
        "并排视图每个虚拟行等高，应按并排后的行号定位"
    );
}

#[test]
fn aggregate_sticky_header_tracks_the_scrolled_file() {
    let rows = vec![
        DiffReviewRow::Header {
            path: "one.rs".into(),
            adds: 1,
            dels: 1,
        },
        DiffReviewRow::Line(0),
        DiffReviewRow::Line(1),
        DiffReviewRow::Header {
            path: "two.rs".into(),
            adds: 2,
            dels: 0,
        },
        DiffReviewRow::Line(2),
    ];
    let headers = aggregate_sticky_headers(&rows, &[], &[], false);

    assert_eq!(
        aggregate_sticky_header(&headers, 0.0, None)
            .as_ref()
            .map(|header| header.path.as_str()),
        None
    );
    assert_eq!(
        aggregate_sticky_header(&headers, -(FILE_LINE_H + 8. + FILE_LINE_H * 2. + 0.1), None)
            .as_ref()
            .map(|header| (header.path.as_str(), header.adds, header.dels)),
        Some(("two.rs", 2, 0))
    );

    // 下一个标题进入 sticky 高度范围时，当前标题应连续向上顶出，而不是瞬间换字。
    let before_boundary = aggregate_sticky_header(&headers, -60.0, None)
        .expect("first file remains sticky before the boundary");
    assert_eq!(before_boundary.path, "one.rs");
    assert!(before_boundary.offset_y < 0.0);
    assert!(before_boundary.offset_y > -before_boundary.height);
}

#[test]
fn aggregate_sticky_header_tracks_split_rows() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let split_rows = build_split_rows(&parsed.lines);
    let headers = aggregate_sticky_headers(&[], &split_rows, &parsed.lines, true);

    assert_eq!(
        aggregate_sticky_header(&headers, -(FILE_LINE_H * 2. + 0.1), None)
            .as_ref()
            .map(|header| (header.path.as_str(), header.adds, header.dels)),
        Some(("two.rs", 1, 1))
    );
}

#[test]
fn aggregate_sticky_body_height_ignores_collapsed_rows() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let heights = aggregate_file_body_heights(&parsed.lines);
    assert_eq!(heights, vec![40.0, 40.0]);

    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert("one.rs".to_string());
    let rows = aggregate_diff_rows(&parsed.lines, &collapsed)
        .into_iter()
        .map(|row| match row {
            AggregateDiffRow::Header { path, adds, dels } => {
                DiffReviewRow::Header { path, adds, dels }
            }
            AggregateDiffRow::Line(index) => DiffReviewRow::Line(index),
        })
        .collect::<Vec<_>>();
    let headers = aggregate_sticky_headers(&rows, &[], &parsed.lines, false);
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0].body_height, 40.0);
    // 第一文件虽折叠，第二标题仍紧跟其后；body_height 不能被压成 0。
    assert_eq!(headers[1].top, FILE_LINE_H + 8.0);
}

#[test]
fn sticky_toggle_scroll_top_only_moves_when_collapsing() {
    assert_eq!(
        sticky_toggle_scroll_top(420.0, 100.0, 28.0, 200.0, false),
        220.0
    );
    assert_eq!(
        sticky_toggle_scroll_top(120.0, 100.0, 28.0, 200.0, false),
        120.0
    );
    assert_eq!(
        sticky_toggle_scroll_top(420.0, 100.0, 28.0, 200.0, true),
        620.0
    );
    assert_eq!(
        sticky_toggle_scroll_top(120.0, 100.0, 28.0, 200.0, true),
        120.0
    );
}

/// 分叉处必须停止压缩，各分支自己成行。
#[test]
fn tree_stops_compacting_at_a_fork() {
    let rows = build_git_tree(&files(&["a/b/x.rs", "a/c/y.rs"]), &Default::default());
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "x.rs", "c", "y.rs"], "实际 {names:?}");
    assert_eq!(rows[0].depth, 0);
    assert_eq!(rows[1].depth, 1);
    assert_eq!(rows[2].depth, 2);
}

/// 目录排在文件前面，同层内各自有序。
#[test]
fn tree_lists_dirs_before_files() {
    let rows = build_git_tree(&files(&["zz.txt", "aa/b.rs"]), &Default::default());
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["aa", "b.rs", "zz.txt"],
        "目录应排在文件前，实际 {names:?}"
    );
}

/// 折叠的目录不展开其子树，但目录行自己还在。
#[test]
fn tree_hides_children_of_collapsed_dir() {
    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert("a".to_string());
    let rows = build_git_tree(&files(&["a/b/x.rs", "a/c/y.rs", "top.rs"]), &collapsed);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a", "top.rs"],
        "折叠后不该露出子树，实际 {names:?}"
    );
}

/// 旧缓存或外部调用可能仍提供 git 的未跟踪目录汇总项 `dir/`；不能渲染空文件名。
#[test]
fn tree_does_not_create_blank_file_for_trailing_slash() {
    let rows = build_git_tree(&files(&["new-dir/"]), &Default::default());
    assert_eq!(rows.len(), 1, "尾斜杠不应额外生成空文件行：{rows:?}");
    assert_eq!(rows[0].name, "new-dir");
    assert!(rows[0].status.is_none());
}

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
fn full_file_action_requires_an_existing_regular_worktree_file() {
    let root = repo_with_change("full-file-availability", "before\n", "after\n");
    let file = root.join("f.txt");
    assert!(is_regular_worktree_file(&file));
    assert!(!is_regular_worktree_file(&root));

    std::fs::remove_file(&file).unwrap();
    assert!(!is_regular_worktree_file(&file));
    std::fs::remove_dir_all(root).unwrap();
}

/// `diff --git` / `index` / `---` / `+++` 不进渲染行（噪音），但必须留在
/// header 里，否则拼出来的 patch 不合法、`git apply` 直接拒收。
#[test]
fn strips_mechanical_header_lines_from_view_but_keeps_them_in_patch() {
    let raw = "diff --git a/f.txt b/f.txt\n\
                   index 918fba6..0064e96 100644\n\
                   --- a/f.txt\n\
                   +++ b/f.txt\n\
                   @@ -1,2 +1,2 @@\n\
                   -old\n\
                   +new\n\
                    ctx\n";
    let parsed = parse_diff(raw);

    // 渲染行里不该出现这四类
    let texts: Vec<&str> = parsed.lines.iter().map(|l| l.text.as_str()).collect();
    assert!(
        !texts.iter().any(|t| t.starts_with("diff ")
            || t.starts_with("index ")
            || t.starts_with("--- ")
            || t.starts_with("+++ ")),
        "机械头部不该出现在渲染行里：{texts:?}"
    );
    assert_eq!(texts[0], "old", "hunk 坐标不应占用可视代码行");

    // 但 patch 仍然完整：header 四行俱在
    assert!(parsed.header.contains("diff --git a/f.txt b/f.txt"));
    assert!(parsed.header.contains("index 918fba6..0064e96"));
    assert!(parsed.header.contains("--- a/f.txt"));
    assert!(parsed.header.contains("+++ b/f.txt"));
}

#[test]
fn aggregate_diff_keeps_a_heading_for_each_file() {
    let raw = concat!(
        "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n",
    );
    let parsed = parse_diff_with_file_headers(raw);
    let headings: Vec<&str> = parsed
        .lines
        .iter()
        .filter(|line| line.kind == DiffKind::Meta)
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(headings, vec!["one.rs", "two.rs"]);
}

#[test]
fn comment_composer_opens_only_after_anchor_is_activated() {
    let parsed = parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n");
    let selected = std::collections::HashSet::from([0usize]);
    let base = (0..parsed.lines.len())
        .map(DiffReviewRow::Line)
        .collect::<Vec<_>>();

    let mut dragging = base.clone();
    insert_comment_composer(&mut dragging, &selected, false);
    assert!(
        !dragging
            .iter()
            .any(|row| matches!(row, DiffReviewRow::CommentComposer)),
        "单纯拖选不能改变虚拟列表高度"
    );

    let mut opened = base;
    insert_comment_composer(&mut opened, &selected, true);
    assert!(
        opened
            .iter()
            .any(|row| matches!(row, DiffReviewRow::CommentComposer)),
        "点击评论锚点后才插入评论器"
    );
}

/// hunk 坐标不应渲染成无行号的伪代码行；但必须原样留在 raw 里，否则 patch 报废。
#[test]
fn hunk_row_shows_context_not_coordinates() {
    let raw = "diff --git a/f b/f\n--- a/f\n+++ b/f\n\
                   @@ -49,7 +49,7 @@ pub struct DeleteWorktreeTarget {\n\
                   -old\n+new\n ctx\n\
                   @@ -100,3 +100,3 @@\n\
                   -a\n+b\n c\n";
    let parsed = parse_diff(raw);

    assert_eq!(parsed.lines.len(), 6, "两段 hunk 坐标都不应占用可视行");
    // 坐标仍在 raw 里
    assert!(
        parsed.hunks[0].raw.starts_with("@@ -49,7 +49,7 @@"),
        "raw 丢了坐标"
    );
    assert!(parsed.hunks[1].raw.starts_with("@@ -100,3 +100,3 @@"));
    // 行号解析不受影响：第一块的上下文行应从 49 起
    let first_ctx = parsed
        .lines
        .iter()
        .find(|l| l.kind == DiffKind::Del)
        .unwrap();
    assert_eq!(first_ctx.old_ln, Some(49), "hunk 起始行号解析被带偏了");
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

/// 行号列宽跟着最大行号走：小文件不该按四位数留白，大文件也不能挤成一团。
#[test]
fn gutter_width_tracks_the_largest_line_number() {
    use super::gutter_width;
    let parsed_small =
        parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-a\n+b\n c\n");
    let parsed_big =
        parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1200,2 +1200,2 @@\n-a\n+b\n c\n");
    let small = gutter_width(&parsed_small.lines);
    let big = gutter_width(&parsed_big.lines);
    assert!(small < big, "四位数行号该比个位数宽：{small} vs {big}");
    assert!(small < 30.0, "两位数以内不该占到 30px：{small}");
    assert!(big < 50.0, "四位数也不该超过 50px：{big}");
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

// ===================== 多仓库工作区 =====================

fn repo(root: &str, rel: &str, kind: smelt_git::discovery::RepoKind) -> DiscoveredRepo {
    DiscoveredRepo {
        root: std::path::PathBuf::from(root),
        rel_path: rel.to_string(),
        kind,
    }
}

fn repo_set(repos: Vec<DiscoveredRepo>) -> RepoSet {
    RepoSet {
        repos,
        truncated: false,
    }
}

/// 写操作必须落到用户选中的那个仓库。
///
/// 这是「子仓改动能暂存却永远提交不出去」的根因：暂存按路径路由进了子仓，
/// 提交却恒在项目根执行，父仓 index 是空的，于是报 nothing to commit。
#[test]
fn commit_target_follows_the_selected_repository() {
    use smelt_git::discovery::RepoKind;
    let set = repo_set(vec![
        repo("/p", "", RepoKind::Root),
        repo("/p/sub", "sub", RepoKind::Submodule),
    ]);
    assert_eq!(
        resolve_git_write_target("/p", Some("/p/sub"), Some(&set), None),
        "/p/sub"
    );
    assert_eq!(resolve_git_write_target("/p", None, Some(&set), None), "/p");
}

/// 选中的仓库已经不在发现结果里（删了 / 换了项目）就回退到项目根，
/// 不能拿着一个不存在的路径继续跑 git。
#[test]
fn stale_repository_selection_falls_back_to_the_project_root() {
    use smelt_git::discovery::RepoKind;
    let set = repo_set(vec![repo("/p", "", RepoKind::Root)]);
    assert_eq!(
        resolve_git_write_target("/p", Some("/p/gone"), Some(&set), None),
        "/p"
    );
}

/// 发现还没跑完时不要抢用户的选择：此刻回退到项目根会让提交落错仓库。
#[test]
fn selection_survives_until_discovery_answers() {
    assert_eq!(
        resolve_git_write_target("/p", Some("/p/sub"), None, None),
        "/p/sub"
    );
    assert_eq!(
        resolve_git_write_target("/p", Some("/p/sub"), Some(&repo_set(Vec::new())), None),
        "/p/sub"
    );
}

/// 部分暂存的文件两组都要出现；未跟踪文件只属于 CHANGES。
#[test]
fn partially_staged_file_shows_in_both_groups() {
    let files = vec![
        ("MM".to_string(), "both.rs".to_string()),
        ("M ".to_string(), "staged.rs".to_string()),
        (" M".to_string(), "changed.rs".to_string()),
        ("??".to_string(), "new.rs".to_string()),
    ];
    let (staged, changed) = split_staged_and_changed(&files);
    let names = |v: Vec<(String, String)>| v.into_iter().map(|(_, p)| p).collect::<Vec<_>>();
    assert_eq!(names(staged), vec!["both.rs", "staged.rs"]);
    assert_eq!(names(changed), vec!["both.rs", "changed.rs", "new.rs"]);
}

/// 用户没选过仓库时，默认落在有改动的那个仓库上。
///
/// 项目根常常自己是干净的（改动全在子仓，或 submodule 配了 `ignore = all`）。
/// 死认项目根会让面板判定"没有改动"，右侧 diff 预览整个不出现。
#[test]
fn unselected_target_falls_back_to_the_repository_with_changes() {
    use smelt_git::discovery::RepoKind;
    let set = repo_set(vec![
        repo("/p", "", RepoKind::Root),
        repo("/p/sub", "sub", RepoKind::Submodule),
    ]);
    assert_eq!(
        resolve_git_write_target("/p", None, Some(&set), Some("/p/sub")),
        "/p/sub"
    );
    // 用户已经亲自选了，就不要替他改主意——哪怕他选的仓库现在是干净的。
    assert_eq!(
        resolve_git_write_target("/p", Some("/p"), Some(&set), Some("/p/sub")),
        "/p"
    );
}

/// 暂存按钮只在 git 真正还在跑时才拒绝新操作。
///
/// 以前只要存在 pending 就拦，而 pending 要等权威 status 走到确认分支才清理。
/// status 因为迟到回包、读取失败重试、期间切了项目而没走到那一步时，pending 就
/// 永久留在表里，那个路径的暂存按钮从此静默失效——点下去没有任何反应，也没有
/// 任何错误提示，比报错更难排查。
#[test]
fn only_a_running_index_operation_blocks_the_next_click() {
    let running = PendingGitIndexOp { completed: false };
    let awaiting_status = PendingGitIndexOp { completed: true };
    assert!(git_index_op_in_flight(Some(&running)));
    assert!(!git_index_op_in_flight(Some(&awaiting_status)));
    assert!(!git_index_op_in_flight(None));
}

/// 干净的仓库不等于没事可做。
///
/// 提交完最后一笔改动后，仓库一度整个从变更栏消失——连同它唯一的推送入口。
/// 本地攒着的提交、远端待拉取的提交、栈里压着的 stash，都得让那一行留下来。
#[test]
fn a_clean_repository_still_shows_up_when_it_has_work_left() {
    assert!(repo_needs_attention(true, 0, 0, 0), "有改动");
    assert!(repo_needs_attention(false, 2, 0, 0), "干净但有待推送的提交");
    assert!(repo_needs_attention(false, 0, 3, 0), "干净但远端有新提交");
    assert!(repo_needs_attention(false, 0, 0, 1), "干净但压着 stash");
    assert!(!repo_needs_attention(false, 0, 0, 0), "真的没事可做");
}
