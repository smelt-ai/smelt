//! 扫描并清理 `~/.smelt` 下的历史残留文件。
//!
//! 几处已知的残留来源（都是 schema/实现换过之后，老文件没人再读写了）：
//! - `tasks/prompts/*.txt`：任务删除时如果没同步删 prompt 文件（已在
//!   [`crate::tasks::TaskStore::remove`] 里修了），会留下孤儿；老版本的这个 bug
//!   已经攒了一批，这里补一次性清理入口。
//! - `tasks/*.json`：早期按项目路径分文件存任务（文件名形如
//!   `-Users-xxx-dev-proj.json`），现在只读写单一的 `~/.smelt/tasks.json`，
//!   这些按项目分的文件已经没有任何代码路径会碰。
//! - `worktrees/`：更早版本 worktree 检出目录固定放这里，现在检出路径由用户在
//!   新建 worktree 时自己选，这个目录树整体已经没有代码会再写入。
//!
//! 不碰的东西：`mermaid_cache/`（还在用的活缓存）、`projects/*/instincts.md`
//! （对应项目还在 `~/dev` 下就不是残留）、任意 git 仓库自己的 worktree（那是
//! `git worktree remove`/`prune` 的事，不归这里管，避免这个功能碰用户仓库）。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::tasks::TaskStore;

fn smelt_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt"))
}

#[derive(Default, Clone)]
pub struct CleanupScan {
    pub orphan_prompts: Vec<PathBuf>,
    pub legacy_task_files: Vec<PathBuf>,
    pub legacy_worktree_dirs: Vec<PathBuf>,
}

impl CleanupScan {
    pub fn is_empty(&self) -> bool {
        self.orphan_prompts.is_empty()
            && self.legacy_task_files.is_empty()
            && self.legacy_worktree_dirs.is_empty()
    }

    pub fn total_items(&self) -> usize {
        self.orphan_prompts.len() + self.legacy_task_files.len() + self.legacy_worktree_dirs.len()
    }
}

/// 只扫描、不落地任何改动，供设置页展示。
pub fn scan() -> CleanupScan {
    let mut out = CleanupScan::default();
    let Some(home) = smelt_home() else {
        return out;
    };

    let known_ids: HashSet<String> = TaskStore::load().tasks.into_iter().map(|t| t.id).collect();

    // `~/.smelt/tasks/` 目录本身（不是 `~/.smelt/tasks.json`）：
    // 直接子级的 *.json 都是老 schema 遗留。
    let tasks_dir = home.join("tasks");
    if let Ok(entries) = fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                out.legacy_task_files.push(path);
            }
        }
    }

    let prompts_dir = tasks_dir.join("prompts");
    if let Ok(entries) = fs::read_dir(&prompts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !known_ids.contains(id) {
                out.orphan_prompts.push(path);
            }
        }
    }

    let worktrees_dir = home.join("worktrees");
    if worktrees_dir.is_dir() {
        out.legacy_worktree_dirs.push(worktrees_dir);
    }

    out
}

/// 执行清理：删掉 `scan()` 报出的文件/目录，返回成功删除的条目数。
pub fn clean(scan: &CleanupScan) -> usize {
    let mut removed = 0;
    for path in scan
        .orphan_prompts
        .iter()
        .chain(scan.legacy_task_files.iter())
    {
        if fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    for dir in &scan.legacy_worktree_dirs {
        if fs::remove_dir_all(dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_report_counts_match_total_items() {
        let s = CleanupScan {
            orphan_prompts: vec![PathBuf::from("a.txt")],
            legacy_task_files: vec![PathBuf::from("b.json"), PathBuf::from("c.json")],
            legacy_worktree_dirs: vec![],
        };
        assert_eq!(s.total_items(), 3);
        assert!(!s.is_empty());
    }

    #[test]
    fn empty_scan_is_empty() {
        assert!(CleanupScan::default().is_empty());
    }
}
