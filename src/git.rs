//! git：读取本地 git 仓库里「你自己」的提交行为，提炼 commit 风格 / 分支 / 工作流习惯。
//! 用 Spotlight 反推活跃仓库（零配置），只读你本人的 commit 元数据，纯本地。
//!
//! 这是「数据层·Git」数据源；commit/remote 还为后续「画像层·关系图谱」和
//! 「Project 作用域」（按 git remote 区分项目级 instinct）铺路。

use anyhow::Result;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 反推活跃仓库时只看最近这么多秒内改动过的文件（30 天）。
const SINCE_SECS: i64 = 30 * 24 * 3600;
/// 最多纳入的仓库数。
const MAX_REPOS: usize = 15;
/// 每个仓库取的提交条数。
const COMMITS_PER_REPO: usize = 25;

/// 生成 git 行为报告，作为 digest 数据源；无活跃仓库时返回 None。
pub fn recent_activity_report() -> Result<Option<String>> {
    let repos = per_repo();
    if repos.is_empty() {
        return Ok(None);
    }
    let blocks: Vec<String> = repos.iter().map(format_repo).collect();
    let mut out = String::from(
        "下面是我最近在各 git 仓库的提交行为（只含我本人的 commit 与分支），反映我的编码工作流与提交习惯。\n",
    );
    out.push_str(&blocks.join("\n"));
    Ok(Some(out))
}

/// `smelt git` 子命令：预览分身从 git 提取到的提交行为。
pub fn run() -> Result<()> {
    match recent_activity_report()? {
        Some(r) => print!("{r}"),
        None => println!("未发现活跃的 git 仓库（近 30 天无 git 仓库改动）。"),
    }
    Ok(())
}

/// 用 mdfind 找最近改动的文件，反推其所属 git 仓库（去重）。
fn discover_repos() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let query = format!("kMDItemFSContentChangeDate >= $time.now(-{SINCE_SECS})");
    let Ok(out) = Command::new("mdfind")
        .arg("-onlyin")
        .arg(&home)
        .arg("-0")
        .arg(&query)
        .output()
    else {
        return Vec::new();
    };

    // 先把文件的父目录去重，再向上找 .git，避免对几万个文件逐个 stat。
    let mut dirs_seen: BTreeSet<PathBuf> = BTreeSet::new();
    for raw in out.stdout.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = Path::new(std::ffi::OsStr::from_bytes(raw));
        if let Some(parent) = path.parent() {
            dirs_seen.insert(parent.to_path_buf());
        }
    }

    let mut repos: BTreeSet<PathBuf> = BTreeSet::new();
    for d in dirs_seen {
        if let Some(root) = find_git_root(&d) {
            repos.insert(root);
        }
    }
    repos.into_iter().collect()
}

/// 从某路径向上查找含 .git 的目录。
fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(c) = cur {
        if c.join(".git").exists() {
            return Some(c.to_path_buf());
        }
        cur = c.parent();
    }
    None
}

/// 单个仓库的提交行为（自己的 commit + 当前分支 + remote 标识）。
pub struct RepoActivity {
    pub name: String,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub commits: Vec<String>,
}

/// 列出活跃仓库的提交行为，供数据源报告与项目级提炼共用。
pub fn per_repo() -> Vec<RepoActivity> {
    let repos = discover_repos();
    let email = git_global_email();
    repos
        .iter()
        .take(MAX_REPOS)
        .filter_map(|repo| {
            let name = repo.file_name()?.to_string_lossy().into_owned();
            let commits = repo_commits(repo, email.as_deref());
            if commits.is_empty() {
                return None;
            }
            Some(RepoActivity {
                name,
                remote: git_str(repo, &["remote", "get-url", "origin"]),
                branch: git_str(repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
                commits,
            })
        })
        .collect()
}

/// 取某仓库里「自己」的最近 commit subject（不含 merge）。
fn repo_commits(repo: &Path, email: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-C".into(),
        repo.to_string_lossy().into_owned(),
        "log".into(),
        "--no-merges".into(),
        format!("-{COMMITS_PER_REPO}"),
        "--pretty=format:%s".into(),
    ];
    if let Some(e) = email {
        args.push(format!("--author={e}"));
    }
    let Ok(log) = Command::new("git").args(&args).output() else {
        return Vec::new();
    };
    if !log.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&log.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 仓库的「值得注意」信号：未推送提交数、最后提交相对时间，供主动简报用。
pub struct RepoSignal {
    pub name: String,
    pub branch: Option<String>,
    pub unpushed: usize,
    pub last_commit: Option<String>,
}

/// 探测各活跃仓库的状态信号（未推送 / 最后提交多久前）。
pub fn repo_signals() -> Vec<RepoSignal> {
    discover_repos()
        .iter()
        .take(MAX_REPOS)
        .filter_map(|repo| {
            let name = repo.file_name()?.to_string_lossy().into_owned();
            let last_commit = git_str(repo, &["log", "-1", "--format=%cr"]);
            // 未推送提交数（无 upstream 时该命令失败，记 0）。
            let unpushed = git_str(repo, &["rev-list", "--count", "@{u}..HEAD"])
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let branch = git_str(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
            Some(RepoSignal {
                name,
                branch,
                unpushed,
                last_commit,
            })
        })
        .collect()
}

/// 把一个仓库的活动格式化成报告块。
fn format_repo(r: &RepoActivity) -> String {
    let mut header = format!("[{}]", r.name);
    if let Some(rem) = &r.remote {
        header.push_str(&format!(" ({rem})"));
    }
    if let Some(b) = &r.branch {
        header.push_str(&format!(" 当前分支: {b}"));
    }
    let mut out = header;
    out.push('\n');
    for c in &r.commits {
        out.push_str(&format!("  - {c}\n"));
    }
    out
}

/// 在指定仓库执行 git 子命令，返回 trim 后的 stdout。
fn git_str(repo: &Path, sub: &[&str]) -> Option<String> {
    let repo_s = repo.to_str()?;
    let mut full = vec!["-C", repo_s];
    full.extend_from_slice(sub);
    let out = Command::new("git").args(&full).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 读取 git 全局配置的 user.email，用于只过滤自己的提交。
fn git_global_email() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_git_root_walks_up() {
        let base = std::env::temp_dir().join(format!("smelt-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::create_dir_all(base.join("src/deep")).unwrap();

        // 从深层子目录应能向上找到仓库根。
        let found = find_git_root(&base.join("src/deep")).expect("应找到 .git 根");
        assert_eq!(found, base);

        // 无 .git 的目录返回 None。
        let other = std::env::temp_dir().join(format!("smelt-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&other);
        std::fs::create_dir_all(&other).unwrap();
        assert!(find_git_root(&other).is_none());

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&other);
    }
}
