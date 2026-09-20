//! 扫描并清理 `~/.smelt` 下的历史残留文件。
//!
//! 几处已知的残留来源（都是 schema/实现换过之后，老文件没人再读写了）：
//! - `smelt.db`、根目录调试文件和 `*.bak`：instincts 下线后的库、崩溃备份、临时诊断。
//! - `llm.json`、`selection.json`、`config.toml`、`global.md`、`projects/`：已经没有读取路径的旧功能残留。
//! - `migration-backups/`：迁库曾把 JSON 另存一份；成功导入后不再需要。
//! - 空的 `skills/.staging`：功能还在，目录空了就是壳。
//! - `mermaid_cache/`：旧版 mermaid 磁盘缓存，渲染已改为只走内存。
//! - `handoffs/`：已废弃的交接底稿目录。
//! - `automation-runs/`：自动化对话曾旁路成 JSON 文件，现只走 SQLite。
//! - `worktrees/`：更早版本固定检出目录，现在路径由用户自选。
//!
//! 不碰的东西：仍存在的 git worktree
//! （那是 `git worktree remove` 的事，避免误删用户还在看的 Issue 副本）。

use std::fs;
use std::path::{Path, PathBuf};

fn smelt_home() -> Option<PathBuf> {
    smelt_paths::smelt_home()
}

/// 启动时回收已经没有任何读写路径的废弃文件。
///
/// 包含 instincts 旧库、根目录调试/备份文件和已迁走的 JSON。仍存在的 git worktree
/// 不在这里删。
pub fn remove_obsolete_files_at_startup() -> usize {
    let Some(home) = smelt_home() else {
        return 0;
    };
    let mut removed = remove_files(&obsolete_home_files(&home));
    removed += remove_dirs(&obsolete_home_dirs(&home));
    removed
}

fn remove_dirs(paths: &[PathBuf]) -> usize {
    paths
        .iter()
        .filter(|path| fs::remove_dir_all(path).is_ok())
        .count()
}

fn remove_files(paths: &[PathBuf]) -> usize {
    paths
        .iter()
        .filter(|path| fs::remove_file(path).is_ok())
        .count()
}

fn obsolete_home_files(home: &Path) -> Vec<PathBuf> {
    let mut files = vec![
        home.join("smelt.db"),
        home.join("smelt.db-wal"),
        home.join("smelt.db-shm"),
        home.join("gui-debug.txt"),
        home.join("scratch-diag.txt"),
        home.join("selection.log"),
        home.join("selection.json"),
        home.join("llm.json"),
        home.join("workspace.json"),
        home.join("appearance.json"),
        home.join("launch.json"),
        home.join("agent_ui.json"),
        home.join("collab.json"),
        home.join("update-settings.json"),
        home.join("update-state.json"),
        home.join("terminal-theme.json"),
        home.join("quota-cache.json"),
        home.join("worktree-inherit.json"),
        home.join("session_metadata.json"),
        home.join("workspace_menu.json"),
        home.join("dsh-auto-models.json"),
        home.join("pi-auto-models.json"),
        home.join("automations.json"),
        home.join("remote_acp_sessions.json"),
        home.join("remote_terminal_sessions.json"),
        home.join("sessions.json"),
        home.join("config.toml"),
        home.join("global.md"),
        home.join("smelt-bridge.log"),
        home.join("app.log.1"),
        home.join("bin").join("smeltd.install.lock"),
    ];
    if let Ok(entries) = fs::read_dir(home.join("bin")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("smeltd.install.") {
                files.push(path);
            }
        }
    }
    if let Ok(entries) = fs::read_dir(home) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".bak") {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files.retain(|path| path.is_file());
    files
}

fn obsolete_home_dirs(home: &Path) -> Vec<PathBuf> {
    let always = [
        home.join("projects"),
        home.join("migration-backups"),
        home.join("mermaid_cache"),
        home.join("handoffs"),
        home.join("tasks"),
        home.join("automation-runs"),
    ];
    let empty_shells = [home.join("skills").join(".staging")];
    always
        .into_iter()
        .filter(|path| path.is_dir())
        .chain(
            empty_shells
                .into_iter()
                .filter(|path| path.is_dir() && dir_is_effectively_empty(path)),
        )
        .collect()
}

fn dir_is_effectively_empty(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().all(|entry| {
        entry.file_name() == ".DS_Store" && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

#[derive(Default, Clone)]
pub struct CleanupScan {
    pub obsolete_files: Vec<PathBuf>,
    pub legacy_worktree_dirs: Vec<PathBuf>,
    pub obsolete_dirs: Vec<PathBuf>,
}

impl CleanupScan {
    pub fn is_empty(&self) -> bool {
        self.obsolete_files.is_empty()
            && self.legacy_worktree_dirs.is_empty()
            && self.obsolete_dirs.is_empty()
    }

    pub fn total_items(&self) -> usize {
        self.obsolete_files.len() + self.legacy_worktree_dirs.len() + self.obsolete_dirs.len()
    }
}

/// 只扫描、不落地任何改动，供设置页展示。
pub fn scan() -> CleanupScan {
    let mut out = CleanupScan::default();
    let Some(home) = smelt_home() else {
        return out;
    };

    out.obsolete_files = obsolete_home_files(&home);
    out.obsolete_dirs = obsolete_home_dirs(&home);

    let worktrees_dir = home.join("worktrees");
    if worktrees_dir.is_dir() {
        out.legacy_worktree_dirs.push(worktrees_dir);
    }

    out
}

/// 执行清理：删掉 `scan()` 报出的历史文件/目录，返回成功删除的条目数。
pub fn clean(scan: &CleanupScan) -> usize {
    let mut removed = 0;
    for path in &scan.obsolete_files {
        if fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    for dir in scan
        .legacy_worktree_dirs
        .iter()
        .chain(scan.obsolete_dirs.iter())
    {
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
            obsolete_files: vec![PathBuf::from("smelt.db"), PathBuf::from("a.txt")],
            legacy_worktree_dirs: vec![],
            obsolete_dirs: vec![],
        };
        assert_eq!(s.total_items(), 2);
        assert!(!s.is_empty());
    }

    #[test]
    fn empty_scan_is_empty() {
        assert!(CleanupScan::default().is_empty());
    }

    #[test]
    fn obsolete_home_files_include_instincts_db_and_backups() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-obsolete-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("smelt.db"), []).unwrap();
        fs::write(dir.join("gui-debug.txt"), "debug").unwrap();
        fs::write(dir.join("llm.json"), "{}").unwrap();
        fs::write(dir.join("workspace.json.empty-after-crash.bak"), "{}").unwrap();
        fs::write(dir.join("workspace.json"), "{}").unwrap();
        fs::write(dir.join("appearance.json"), "{}").unwrap();
        fs::write(dir.join("launch.json"), "{}").unwrap();
        fs::write(dir.join("agent_ui.json"), "{}").unwrap();
        fs::write(dir.join("smelt.sqlite3"), []).unwrap();
        fs::create_dir_all(dir.join("migration-backups")).unwrap();
        fs::create_dir_all(dir.join("projects")).unwrap();
        fs::create_dir_all(dir.join("mermaid_cache")).unwrap();
        fs::create_dir_all(dir.join("handoffs")).unwrap();
        fs::create_dir_all(dir.join("tasks")).unwrap();
        fs::create_dir_all(dir.join("automation-runs")).unwrap();
        fs::create_dir_all(dir.join("skills").join(".staging")).unwrap();
        fs::create_dir_all(dir.join("skills").join("keep")).unwrap();
        fs::write(dir.join("skills").join("keep").join("SKILL.md"), "keep").unwrap();

        let files = obsolete_home_files(&dir);
        assert!(files.contains(&dir.join("smelt.db")));
        assert!(files.contains(&dir.join("gui-debug.txt")));
        assert!(files.contains(&dir.join("llm.json")));
        assert!(files.contains(&dir.join("workspace.json.empty-after-crash.bak")));
        assert!(files.contains(&dir.join("workspace.json")));
        assert!(files.contains(&dir.join("appearance.json")));
        assert!(files.contains(&dir.join("launch.json")));
        assert!(files.contains(&dir.join("agent_ui.json")));
        assert!(!files.contains(&dir.join("smelt.sqlite3")));
        let dirs = obsolete_home_dirs(&dir);
        assert!(dirs.contains(&dir.join("migration-backups")));
        assert!(dirs.contains(&dir.join("projects")));
        assert!(dirs.contains(&dir.join("mermaid_cache")));
        assert!(dirs.contains(&dir.join("handoffs")));
        assert!(dirs.contains(&dir.join("tasks")));
        assert!(dirs.contains(&dir.join("automation-runs")));
        assert!(dirs.contains(&dir.join("skills").join(".staging")));
        assert!(!dirs.contains(&dir.join("skills").join("keep")));
        let _ = fs::remove_dir_all(dir);
    }
}
