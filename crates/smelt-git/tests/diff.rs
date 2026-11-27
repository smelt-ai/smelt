//! diff 先量后读：改动列表只给事实，超过查看上限的文件不算 diff、不给正文。跑真 git。
//!
//! 回归的是 git 面板整块消失：仓库里一个 414 MB 的未跟踪日志被整个 diff 成文本交给插件，
//! 插件虚拟机内存超限崩掉，同一个虚拟机里的插件全没了。

use std::path::PathBuf;

use smelt_git::{
    AGGREGATE_FILE_LIMIT, DIFF_LIMIT, DiffScope, commit_file_diff, diff_files, file_diff, run_git,
};
use smelt_runtime_client::protocol::git::{
    GitChangeStatus, GitDiffBody, GitDiffEntry, GitDiffText, GitLineCounts,
};

/// 临时仓库，用完删掉。
struct Repo(PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("smelt-git-diff-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        repo
    }

    fn root(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = run_git(self.root(), args).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr_str());
    }

    fn write(&self, path: &str, content: &[u8]) {
        let full = self.0.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    /// 一个刚好超过查看上限的文件。稀疏文件：不真写 8 MB。
    fn write_oversized(&self, path: &str) {
        let file = std::fs::File::create(self.0.join(path)).unwrap();
        file.set_len(DIFF_LIMIT + 1).unwrap();
    }

    fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry<'a>(entries: &'a [GitDiffEntry], path: &str) -> &'a GitDiffEntry {
    entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("列表里没有 {path}：{entries:?}"))
}

fn diff_entries(
    root: &str,
    scope: DiffScope,
    untracked: &[String],
) -> Result<Vec<GitDiffEntry>, String> {
    diff_files(root, scope, untracked).map(|r| r.files)
}

fn text(diff: &GitDiffText) -> &str {
    match diff {
        GitDiffText::Text { text } => text,
        GitDiffText::TooLarge { size } => panic!("不该超上限：{size}"),
    }
}

#[test]
fn oversized_untracked_file_is_listed_without_its_text() {
    let repo = Repo::new("big-untracked");
    repo.write("a.txt", b"a\n");
    repo.commit_all("init");
    repo.write_oversized("run.log");

    let entries = diff_entries(repo.root(), DiffScope::All, &["run.log".into()]).unwrap();
    let log = entry(&entries, "run.log");
    assert_eq!(log.status, GitChangeStatus::Untracked);
    assert_eq!(log.body, GitDiffBody::TooLarge);
    assert_eq!(log.size, DIFF_LIMIT + 1);

    let diff = file_diff(repo.root(), "run.log", None, true, DiffScope::All).unwrap();
    assert_eq!(
        diff.diff,
        GitDiffText::TooLarge {
            size: DIFF_LIMIT + 1
        }
    );
}

#[test]
fn oversized_tracked_change_is_not_diffed() {
    let repo = Repo::new("big-tracked");
    repo.write("data.bin", b"small\n");
    repo.commit_all("init");
    repo.write_oversized("data.bin");

    let entries = diff_entries(repo.root(), DiffScope::All, &[]).unwrap();
    assert_eq!(entry(&entries, "data.bin").body, GitDiffBody::TooLarge);
    let diff = file_diff(repo.root(), "data.bin", None, false, DiffScope::All).unwrap();
    assert_eq!(
        diff.diff,
        GitDiffText::TooLarge {
            size: DIFF_LIMIT + 1
        }
    );

    // 提交进历史以后，看那个提交里的这个文件也一样。
    repo.commit_all("grow");
    assert_eq!(
        commit_file_diff(repo.root(), "HEAD", "data.bin").unwrap(),
        GitDiffText::TooLarge {
            size: DIFF_LIMIT + 1
        }
    );
}

/// 文件本身没超上限，diff 却超了（整个重写）：读 git 输出到上限就停。
#[test]
fn diff_over_the_limit_is_cut_off() {
    let repo = Repo::new("big-patch");
    // 每份约为上限的 3/4，整个重写后 diff 约为上限的 1.5 倍。
    let lines = |tag: &str| -> Vec<u8> {
        let count = (DIFF_LIMIT as usize) * 3 / 4 / 62;
        (0..count)
            .flat_map(|i| format!("{tag}{i:0>60}\n").into_bytes())
            .collect()
    };
    repo.write("f.txt", &lines("a"));
    repo.commit_all("init");
    repo.write("f.txt", &lines("b"));

    let diff = file_diff(repo.root(), "f.txt", None, false, DiffScope::All).unwrap();
    let size = std::fs::metadata(repo.0.join("f.txt")).unwrap().len();
    assert!(size <= DIFF_LIMIT);
    assert!(
        diff.diff == GitDiffText::TooLarge { size },
        "整个重写的 diff 超过上限，不该给正文"
    );
}

#[test]
fn entries_carry_status_size_and_line_counts() {
    let repo = Repo::new("facts");
    repo.write("edit.txt", b"one\ntwo\nthree\n");
    repo.write("gone.txt", b"bye\n");
    repo.write("old name.txt", b"same\ncontent\nhere\n");
    repo.write("pic.bin", b"\0\x01\x02");
    repo.commit_all("init");

    repo.write("edit.txt", b"one\nTWO\nthree\nfour\n");
    std::fs::remove_file(repo.0.join("gone.txt")).unwrap();
    repo.git(&["mv", "old name.txt", "new name.txt"]);
    repo.write("pic.bin", b"\0\x01\x03");
    repo.write("fresh.txt", b"hello\n");
    repo.write("binary.dat", b"x\0y");
    repo.write("nested/inner.txt", b"x\n");
    repo.write("[id].tsx", b"literal\n");

    let untracked = vec![
        "fresh.txt".to_string(),
        "binary.dat".to_string(),
        "nested/".to_string(),
        "[id].tsx".to_string(),
        "vanished.txt".to_string(),
    ];
    let entries = diff_entries(repo.root(), DiffScope::All, &untracked).unwrap();

    let edit = entry(&entries, "edit.txt");
    assert_eq!(edit.status, GitChangeStatus::Modified);
    assert_eq!(edit.size, 19);
    assert_eq!(
        edit.body,
        GitDiffBody::Text {
            lines: Some(GitLineCounts {
                added: 2,
                deleted: 1
            })
        }
    );
    assert_eq!(entry(&entries, "gone.txt").status, GitChangeStatus::Deleted);
    assert_eq!(entry(&entries, "gone.txt").size, 4);
    let renamed = entry(&entries, "new name.txt");
    assert_eq!(renamed.status, GitChangeStatus::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some("old name.txt"));
    assert_eq!(entry(&entries, "pic.bin").body, GitDiffBody::Binary);

    let fresh = entry(&entries, "fresh.txt");
    assert_eq!(fresh.status, GitChangeStatus::Untracked);
    assert_eq!(
        (fresh.size, fresh.body),
        (6, GitDiffBody::Text { lines: None })
    );
    assert_eq!(entry(&entries, "binary.dat").body, GitDiffBody::Binary);
    assert_eq!(entry(&entries, "nested").body, GitDiffBody::Directory);
    assert!(
        !entries.iter().any(|e| e.path == "vanished.txt"),
        "status 之后被删掉的文件不再列出"
    );
    // 未跟踪的排在已跟踪的后面，按客户端给的顺序。
    let order: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        &order[order.len() - 4..],
        ["fresh.txt", "binary.dat", "nested", "[id].tsx"]
    );
}

#[test]
fn file_diff_keeps_a_rename_and_reads_paths_literally() {
    let repo = Repo::new("rename");
    repo.write("old.txt", b"a\nb\nc\nd\n");
    repo.write("i.tsx", b"i\n");
    repo.write("[i].tsx", b"bracket\n");
    repo.commit_all("init");
    repo.git(&["mv", "old.txt", "new.txt"]);
    repo.write("new.txt", b"a\nb\nc\nD\n");
    repo.write("i.tsx", b"I\n");
    repo.write("[i].tsx", b"BRACKET\n");

    let diff = file_diff(
        repo.root(),
        "new.txt",
        Some("old.txt"),
        false,
        DiffScope::All,
    )
    .unwrap();
    let body = text(&diff.diff);
    assert!(body.contains("rename from old.txt"), "{body}");
    assert!(body.contains("+D"), "{body}");

    // `[i].tsx` 当 glob 会连带 `i.tsx`。
    let diff = file_diff(repo.root(), "[i].tsx", None, false, DiffScope::All).unwrap();
    let body = text(&diff.diff);
    assert!(body.contains("+BRACKET"), "{body}");
    assert!(!body.contains("+I"), "{body}");
}

#[test]
fn untracked_text_and_directory() {
    let repo = Repo::new("untracked");
    repo.write("a.txt", b"a\n");
    repo.commit_all("init");
    repo.write("new.txt", b"hello\n");
    repo.write("dir/inner.txt", b"x\n");

    let diff = file_diff(repo.root(), "new.txt", None, true, DiffScope::All).unwrap();
    assert!(text(&diff.diff).contains("+hello"));
    let diff = file_diff(repo.root(), "dir", None, true, DiffScope::All).unwrap();
    assert_eq!(text(&diff.diff), "", "目录没有正文，只显示文件头");
    assert!(file_diff(repo.root(), "missing.txt", None, true, DiffScope::All).is_err());
}

#[test]
fn staged_scope_measures_the_index_side() {
    let repo = Repo::new("staged");
    repo.write("f.txt", b"one\n");
    repo.commit_all("init");
    repo.write_oversized("f.txt");
    repo.git(&["add", "f.txt"]);
    // 工作区改回小文件：暂存区那份才是要比的。
    repo.write("f.txt", b"one\n");

    let staged = diff_entries(repo.root(), DiffScope::Staged, &[]).unwrap();
    assert_eq!(entry(&staged, "f.txt").body, GitDiffBody::TooLarge);
    let unstaged = diff_entries(repo.root(), DiffScope::Unstaged, &[]).unwrap();
    assert_eq!(entry(&unstaged, "f.txt").body, GitDiffBody::TooLarge);
    let all = diff_entries(repo.root(), DiffScope::All, &[]).unwrap();
    assert!(all.is_empty(), "HEAD 和工作区一样：{all:?}");
}

#[test]
fn unmerged_path_is_listed_once() {
    let repo = Repo::new("conflict");
    repo.git(&["checkout", "-qb", "main"]);
    repo.write("c.txt", b"base\n");
    repo.commit_all("base");
    repo.git(&["checkout", "-qb", "other"]);
    repo.write("c.txt", b"other\n");
    repo.commit_all("other");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("c.txt", b"main\n");
    repo.commit_all("main");
    let _ = run_git(repo.root(), &["merge", "-q", "other"]);

    let entries = diff_entries(repo.root(), DiffScope::Unstaged, &[]).unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].status, GitChangeStatus::Unmerged);
    assert!(entries[0].size > 0);
}

/// `count` 行、每行 62 字节的文本，`tag` 开头，整个重写后每行都算改动。
fn rewrite_lines(tag: &str, count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|i| format!("{tag}{i:0>60}\n").into_bytes())
        .collect()
}

#[test]
fn patch_holds_the_files_that_fit_in_list_order() {
    let repo = Repo::new("aggregate");
    repo.write("edit.txt", b"one\ntwo\n");
    repo.write("old.txt", b"a\nb\nc\nd\n");
    repo.write("big.txt", b"small\n");
    repo.commit_all("init");
    repo.write("edit.txt", b"one\nTWO\n");
    repo.git(&["mv", "old.txt", "new.txt"]);
    // 没超查看上限，但单个太大，不进汇总。
    repo.write(
        "big.txt",
        &rewrite_lines("b", AGGREGATE_FILE_LIMIT as usize / 62 + 1),
    );
    repo.write("fresh.txt", b"hello\n");
    repo.write("nested/inner.txt", b"x\n");
    repo.write_oversized("run.log");

    let untracked = ["fresh.txt", "nested/", "run.log"].map(String::from);
    let result = diff_files(repo.root(), DiffScope::All, &untracked).unwrap();
    let shown = |path: &str| entry(&result.files, path).in_patch;
    assert!(shown("edit.txt") && shown("new.txt") && shown("fresh.txt"));
    assert!(!shown("big.txt"), "单个超过汇总上限的留在列表里");
    assert!(
        !shown("nested") && !shown("run.log"),
        "目录和超过查看上限的没有正文"
    );
    let patch = &result.patch;
    assert!(patch.contains("+TWO"), "{patch}");
    assert!(patch.contains("rename from old.txt"), "{patch}");
    assert!(patch.contains("+hello"), "{patch}");
    assert!(!patch.contains("big.txt"), "{patch}");
    // 未跟踪的接在已跟踪的后面。
    assert!(patch.find("+TWO") < patch.find("+hello"));
}

/// 整批超过额度时拆开逐个放：放得下的照样进汇总，后面小的也不被挤掉。
#[test]
fn patch_stops_at_the_limit_and_keeps_later_files_that_fit() {
    let repo = Repo::new("aggregate-limit");
    // 每份约 0.9 MB，重写后每个 diff 约 1.8 MB：五个合起来超过查看上限。
    let count = (AGGREGATE_FILE_LIMIT as usize) * 9 / 10 / 62;
    let names = ["a1.txt", "a2.txt", "a3.txt", "a4.txt", "a5.txt"];
    for name in names {
        repo.write(name, &rewrite_lines("a", count));
    }
    repo.write("z.txt", b"z\n");
    repo.commit_all("init");
    for name in names {
        repo.write(name, &rewrite_lines("b", count));
    }
    repo.write("z.txt", b"Z\n");

    let result = diff_files(repo.root(), DiffScope::All, &[]).unwrap();
    assert!(result.patch.len() as u64 <= DIFF_LIMIT);
    let shown: Vec<&str> = result
        .files
        .iter()
        .filter(|e| e.in_patch)
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(shown, ["a1.txt", "a2.txt", "a3.txt", "a4.txt", "z.txt"]);
    assert!(
        result.patch.ends_with("+Z\n"),
        "放不下的 a5 之后，z 照样进来"
    );
}
