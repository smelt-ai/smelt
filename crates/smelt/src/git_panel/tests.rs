//! Git 面板的纯逻辑与 Git 命令回归测试。

// 不用 `use super::*;`：父模块顶部有 gpui/gpui_component 的 glob 导入，带进测试
// 模块会让 trait 解析图爆炸，`cargo test` 编译期能把 rustc 撑崩。只导入真正
// 用到的名字。
use super::RepoSet;
use super::workspace::apply_expanded_files;
use super::{
    DiffScope, GitDiff, GitStatusData, GitStatusResponseAction, GitWrite, PendingGitIndexOp,
    aggregate_diff_ready_for_file, build_git_tree, clear_confirmed_git_index_ops,
    commit_button_label, diff_feedback_text, diff_parse_failure, diff_scroll_target,
    full_file_path, git_empty_list_message, git_index_op_in_flight, git_status_response_action,
    git_status_retry_delay, hunk_line_anchor, parse_diff, parse_diff_with_file_headers,
    release_git_write, repo_needs_attention, reserve_git_write, resolve_git_write_target,
    should_close_aggregate_diff, split_staged_and_changed,
};
use smelt_runtime_client::protocol::git::{DiffKind, DiscoveredRepo, RepoKind};

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
    let raw = "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let parsed = parse_diff_with_file_headers(raw);
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
        patch: Some(raw.into()),
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

/// 块按钮挂在第一行有新文件行号的位置；纯删除才退回旧文件行号。
#[test]
fn hunk_anchor_prefers_the_modified_side() {
    let changed =
        parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\n");
    assert_eq!(
        hunk_line_anchor(&changed.lines, &changed.hunks[0]),
        Some((true, 1)),
        "新增行的新文件行号应优先于后面的上下文"
    );

    let context_first =
        parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -10,3 +10,3 @@\n ctx\n-old\n+new\n");
    assert_eq!(
        hunk_line_anchor(&context_first.lines, &context_first.hunks[0]),
        Some((true, 10)),
        "块从上下文起时，锚点是该块第一行，不是后面的改动行"
    );

    let deleted = parse_diff("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -3,1 +2,0 @@\n-gone\n");
    assert_eq!(
        hunk_line_anchor(&deleted.lines, &deleted.hunks[0]),
        Some((false, 3)),
        "纯删除没有新文件行号，应落在旧文件第 3 行"
    );
}

/// 写进终端的评论带文件和行号，不附带回车以外的发送动作。
#[test]
fn diff_feedback_names_the_file_and_keeps_the_selection() {
    assert_eq!(
        diff_feedback_text("src/a.rs", 8, 3, "hello", "看这里"),
        "对 src/a.rs 的这几行有反馈：\nL3-L8\nhello\n\n看这里\n"
    );
    assert_eq!(
        diff_feedback_text("src/a.rs", 3, 3, "hello\n", ""),
        "对 src/a.rs 的这几行有反馈：\nL3\nhello\n",
        "已有换行不再补；空评论不另起一段"
    );
}

/// 右侧点的是状态路径。重命名要滚到组件展示的新路径，空格不能按空白切开。
#[test]
fn diff_scroll_target_uses_the_component_path() {
    let patch = "\
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
--- a/old name.rs
+++ b/new name.rs
@@ -1 +1 @@
-a
+b
";
    let files = gpui_component::diff::DiffFile::parse(patch).expect("rename diff");
    assert_eq!(
        diff_scroll_target(&files, "new name.rs"),
        Some("new name.rs")
    );
    assert_eq!(
        diff_scroll_target(&files, "old name.rs"),
        Some("new name.rs"),
        "点旧路径也落到展示用的新路径"
    );
    assert_eq!(diff_scroll_target(&files, "missing.rs"), None);
}

/// 空补丁是「无差异」。不像 diff 的原文原样显示，像 diff 但解析失败才用解析器的说明。
#[test]
fn diff_parse_failure_keeps_non_diff_text() {
    assert_eq!(diff_parse_failure("  \n", "ignored"), "（无差异）");
    assert_eq!(
        diff_parse_failure("fatal: not a git repository", "parser"),
        "fatal: not a git repository"
    );
    let combined = "diff --git a/f b/f\n@@@ -1,1 -1,1 +1,1 @@@\n";
    assert_eq!(
        diff_parse_failure(combined, "combined diff"),
        "combined diff"
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

// ===================== 多仓库工作区 =====================

fn repo(root: &str, rel: &str, kind: RepoKind) -> DiscoveredRepo {
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

#[test]
fn commit_button_names_only_its_own_operation() {
    assert_eq!(commit_button_label(None), "提交并推送");
    assert_eq!(commit_button_label(Some(GitWrite::Commit)), "提交中…");
    assert_eq!(
        commit_button_label(Some(GitWrite::CommitAndPush)),
        "提交并推送中…"
    );
    assert_eq!(commit_button_label(Some(GitWrite::Push)), "推送中…");
    assert_eq!(commit_button_label(Some(GitWrite::Fetch)), "提交并推送");
    assert_eq!(commit_button_label(Some(GitWrite::Checkout)), "提交并推送");
}

/// `index.lock` 属于一个仓库。占用 A 不得挡住 B，释放也只认当初占进去的那个操作。
#[test]
fn a_repository_write_excludes_only_that_repository() {
    use std::collections::HashMap;
    let mut writes = HashMap::new();
    assert!(reserve_git_write(&mut writes, "/a", GitWrite::Commit));
    assert!(!reserve_git_write(&mut writes, "/a", GitWrite::Fetch));
    assert!(reserve_git_write(&mut writes, "/b", GitWrite::Fetch));
    assert!(!release_git_write(&mut writes, "/a", GitWrite::Fetch));
    assert_eq!(writes.get("/a"), Some(&GitWrite::Commit));
    assert!(release_git_write(&mut writes, "/a", GitWrite::Commit));
    assert!(!writes.contains_key("/a"));
    assert_eq!(writes.get("/b"), Some(&GitWrite::Fetch));
}

/// 写操作必须落到用户选中的那个仓库。
///
/// 这是「子仓改动能暂存却永远提交不出去」的根因：暂存按路径路由进了子仓，
/// 提交却恒在项目根执行，父仓 index 是空的，于是报 nothing to commit。
#[test]
fn commit_target_follows_the_selected_repository() {
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

#[gpui::test]
fn aggregate_diff_files_stay_collapsed_until_opened(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::collections::HashSet;

    let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
                 diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let files = gpui_component::diff::DiffFile::parse(patch).unwrap();
    let state = cx.new(|cx| gpui_component::diff::DiffState::new(files, cx));
    let opened = HashSet::from(["b.rs".to_string()]);

    state.update(cx, |state, cx| {
        apply_expanded_files(state, true, &opened, cx)
    });
    state.read_with(cx, |state, _| {
        assert!(state.is_file_collapsed("a.rs"));
        assert!(!state.is_file_collapsed("b.rs"));
    });

    // 单文件 diff 不跟这份集合走，总是展开。
    state.update(cx, |state, cx| {
        apply_expanded_files(state, false, &HashSet::new(), cx)
    });
    state.read_with(cx, |state, _| {
        assert!(!state.is_file_collapsed("a.rs"));
        assert!(!state.is_file_collapsed("b.rs"));
    });
}
