//! scan：用 macOS Spotlight（mdfind）自动发现你全盘最近改动的文件，
//! 零配置即覆盖你的真实活动，无需手动指定目录。只采集「路径 + 扩展名 + mtime」
//! 这类元数据，绝不读取文件内容（隐私优先）。
//!
//! 默认扫描范围 = 整个 home；可选用 SMELT_SCAN_DIRS / config.toml 的 scan_dirs
//! 把范围**缩小**到指定目录。系统噪音（~/Library、构建产物、隐藏目录等）自动忽略。
//! mdfind 不可用时回退到本地遍历。

use crate::db;
use anyhow::Result;
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// mdfind 的时间窗：只看最近这么多秒内改动过的文件（14 天）。
const SINCE_SECS: i64 = 14 * 24 * 3600;
/// 报告中展示的最近文件条数。
const MAX_FILES: usize = 50;
/// 候选文件硬上限，防止在超大结果集上耗时。
const SCAN_CAP: usize = 8000;
/// 回退遍历时的最大递归深度。
const MAX_DEPTH: usize = 8;

/// 构建产物 / 依赖 / 版本控制等噪音目录。
const IGNORE_DIRS: &[&str] = &[
    ".git", "target", "node_modules", "dist", "build", ".next",
    "vendor", ".venv", "venv", "__pycache__", "coverage", ".cache",
];
/// 系统 / 非工作内容目录（home 下默认全扫，但这些是机器噪音而非用户活动）。
const NOISE_DIRS: &[&str] = &["Library", "Applications", ".Trash", "Public"];

/// 一条文件记录：绝对路径、扩展名、修改时间。
struct Entry {
    path: PathBuf,
    ext: String,
    mtime: SystemTime,
}

/// `smelt scan` 子命令：预览分身当前发现的最近改动文件（供用户核对采集范围 / 隐私边界）。
pub fn run() -> Result<()> {
    match recent_files_report()? {
        Some(report) => print!("{report}"),
        None => println!(
            "未发现最近改动的文件。\n\
             （Spotlight 不可用、近 14 天无活动，或 scan_dirs 指向了空目录）"
        ),
    }
    Ok(())
}

/// 对外入口：发现最近改动的文件并生成报告；无任何结果时返回 None（静默跳过）。
pub fn recent_files_report() -> Result<Option<String>> {
    let roots = scan_roots()?;
    let home = dirs::home_dir();
    let mut entries: Vec<Entry> = Vec::new();

    for root in &roots {
        match mdfind_recent(root) {
            // mdfind 可用：全量收集它的全盘发现结果。
            // 注意：mdfind 输出顺序非时间序，必须先收齐所有 mtime，
            // 才能在 render 里正确排序取「最近 N」——不能在此提前截断。
            Some(paths) => {
                for p in paths {
                    push_path(p, &mut entries);
                }
            }
            // mdfind 不可用：回退到本地遍历（walk 内部用 SCAN_CAP 防爆）。
            None => walk(root, 0, &mut entries),
        }
    }

    Ok(render(entries, home.as_deref(), MAX_FILES))
}

/// 解析扫描根目录：env SMELT_SCAN_DIRS 优先，其次 config.toml 的 scan_dirs（均冒号分隔，支持 `~`）。
/// 未配置任何目录时，**默认扫描整个 home**（零配置即全局）。
fn scan_roots() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir();
    let raw = std::env::var("SMELT_SCAN_DIRS")
        .ok()
        .or_else(config_scan_dirs)
        .unwrap_or_default();

    let configured: Vec<PathBuf> = raw
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_tilde(s, home.as_deref()))
        .filter(|p| p.is_dir())
        .collect();

    if !configured.is_empty() {
        return Ok(configured);
    }
    // 零配置默认：整个 home。
    Ok(home.into_iter().collect())
}

/// 从 ~/.smelt/config.toml 读取 `scan_dirs = "..."`（手写解析，与 digest::api_key 同风格）。
fn config_scan_dirs() -> Option<String> {
    let cfg = db::smelt_dir().ok()?.join("config.toml");
    let text = std::fs::read_to_string(cfg).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("scan_dirs") {
            if let Some(eq) = rest.split('=').nth(1) {
                return Some(eq.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// 展开开头的 `~` 为 home 目录。
fn expand_tilde(s: &str, home: Option<&Path>) -> PathBuf {
    match (s.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(s),
    }
}

/// 调用 mdfind 查某根目录下最近改动的文件；命令不可用 / 执行失败时返回 None（触发回退）。
fn mdfind_recent(root: &Path) -> Option<Vec<PathBuf>> {
    let query = format!("kMDItemFSContentChangeDate >= $time.now(-{SINCE_SECS})");
    let out = Command::new("mdfind")
        .arg("-onlyin")
        .arg(root)
        .arg("-0") // NUL 分隔，安全处理含空格/换行的路径
        .arg(&query)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let paths = out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(std::ffi::OsStr::from_bytes(s)))
        .collect();
    Some(paths)
}

/// 把一个路径加入候选：过滤噪音、确认是文件、取扩展名与 mtime。
fn push_path(path: PathBuf, out: &mut Vec<Entry>) {
    if is_noise(&path) {
        return;
    }
    let Some(ext) = ext_of(&path) else { return };
    let Ok(meta) = std::fs::metadata(&path) else { return };
    if !meta.is_file() {
        return;
    }
    let Ok(mtime) = meta.modified() else { return };
    out.push(Entry { path, ext, mtime });
}

/// 回退路径：本地递归遍历（mdfind 不可用时）。
fn walk(dir: &Path, depth: usize, out: &mut Vec<Entry>) {
    if depth > MAX_DEPTH || out.len() >= SCAN_CAP {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if out.len() >= SCAN_CAP {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || IGNORE_DIRS.contains(&name.as_ref())
            || NOISE_DIRS.contains(&name.as_ref())
        {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk(&path, depth + 1, out);
        } else if ft.is_file() {
            push_path(path, out);
        }
    }
}

/// 路径是否落在噪音区（系统目录、构建产物、隐藏目录）。
fn is_noise(path: &Path) -> bool {
    for comp in path.components() {
        if let Component::Normal(os) = comp {
            let s = os.to_string_lossy();
            if s.starts_with('.')
                || IGNORE_DIRS.contains(&s.as_ref())
                || NOISE_DIRS.contains(&s.as_ref())
            {
                return true;
            }
        }
    }
    false
}

/// 提取小写扩展名；无扩展名跳过。
fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| !e.is_empty())
}

/// 把绝对路径渲染成简短标签：home 前缀替换为 `~`。
fn to_label(path: &Path, home: Option<&Path>) -> String {
    if let Some(h) = home {
        if let Ok(rel) = path.strip_prefix(h) {
            return format!("~/{}", rel.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

/// 渲染报告：按修改时间降序，输出语言/类型分布 + 最近文件列表。
fn render(mut entries: Vec<Entry>, home: Option<&Path>, limit: usize) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| b.mtime.cmp(&a.mtime));

    // 扩展名分布（反映整体技术栈 / 活动类型）。
    let mut dist: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries {
        *dist.entry(e.ext.clone()).or_insert(0) += 1;
    }
    let mut dist_vec: Vec<(String, usize)> = dist.into_iter().collect();
    dist_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let dist_str = dist_vec
        .iter()
        .take(15)
        .map(|(ext, n)| format!("{ext}×{n}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::from(
        "下面是我最近改动的文件（Spotlight 全盘发现，仅路径与类型元数据，不含文件内容）。\n语言/类型分布: ",
    );
    out.push_str(&dist_str);
    out.push_str("\n最近改动:\n");
    for e in entries.iter().take(limit) {
        out.push_str(&format!("- {} [{}]\n", to_label(&e.path, home), e.ext));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(path: &str, ext: &str, secs: u64) -> Entry {
        Entry {
            path: PathBuf::from(path),
            ext: ext.into(),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    #[test]
    fn ext_of_lowercases_and_skips_none() {
        assert_eq!(ext_of(Path::new("a/Main.RS")).as_deref(), Some("rs"));
        assert_eq!(ext_of(Path::new("README")), None);
    }

    #[test]
    fn expand_tilde_uses_home() {
        let home = Path::new("/Users/x");
        assert_eq!(expand_tilde("~/dev", Some(home)), PathBuf::from("/Users/x/dev"));
        assert_eq!(expand_tilde("/abs", Some(home)), PathBuf::from("/abs"));
    }

    #[test]
    fn is_noise_catches_system_and_build_dirs() {
        assert!(is_noise(Path::new("/Users/x/Library/Caches/a.db")));
        assert!(is_noise(Path::new("/Users/x/dev/proj/target/x.rs")));
        assert!(is_noise(Path::new("/Users/x/dev/.git/config")));
        assert!(is_noise(Path::new("/Users/x/.zshrc")));
        assert!(!is_noise(Path::new("/Users/x/dev/proj/src/main.rs")));
    }

    #[test]
    fn empty_entries_yield_none() {
        assert!(render(vec![], None, 10).is_none());
    }

    #[test]
    fn render_sorts_recent_first_and_shows_distribution() {
        let home = PathBuf::from("/Users/x");
        let entries = vec![
            entry("/Users/x/dev/a.rs", "rs", 100),
            entry("/Users/x/dev/b.rs", "rs", 300), // 最新
            entry("/Users/x/notes/c.md", "md", 200),
        ];
        let report = render(entries, Some(&home), 10).expect("应生成报告");
        // home 前缀被替换为 ~
        assert!(report.contains("~/dev/b.rs"));
        // 扩展名分布，rs 数量多排在前
        assert!(report.contains("rs×2"));
        assert!(report.contains("md×1"));
        // 最新的 b.rs 应排在 a.rs 之前
        let pb = report.find("b.rs").unwrap();
        let pa = report.find("a.rs").unwrap();
        assert!(pb < pa, "最近改动应排在前面");
    }
}
