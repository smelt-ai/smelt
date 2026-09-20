//! 新建 Git worktree 时继承主仓库里未被 Git 跟踪的内容。
//!
//! `git worktree add` 只铺开被跟踪的文件，`.env`、本地凭据、IDE 配置这类没入库
//! 的东西不会跟过去，于是每开一个 worktree 都要重新配一遍环境。这里在 worktree
//! 建好之后，把主仓库根下未跟踪的条目**软链**过去，让它们共享同一份真源：主仓库
//! 改了配置，所有 worktree 立即生效；某个 worktree 想要不一样的，删掉软链自己放
//! 一份真文件即可（本模块只在目标不存在时创建，不会覆盖）。
//!
//! 三类条目区别对待：
//! - **可重建产物**（`target/`、`node_modules/`、`Pods/` 等）跳过。软链过去会让多
//!   个 worktree 争抢同一个构建目录、互相覆盖产物；复制也没意义——Rust 的 dep-info
//!   之类记的是绝对路径，换个位置基本等于全量重建。让它各自重建反而最省事。
//! - **配置/凭据/工具状态**软链，这是本功能的目标。
//! - **想要分叉的**由用户手动删链接放真文件，不需要额外配置。
//!
//! 软链有个反直觉的坑必须一并处理：`.gitignore` 里带斜杠的规则（如 `dist/`）**只
//! 匹配目录**，而软链在 Git 眼里是文件，规则会失配，于是 worktree 的
//! `git status` 里凭空多出一堆 `??`。所以链完还要把这些条目写进 worktree **私有**
//! 的 excludes 文件。注意不能用 `.git/info/exclude`——它在主仓库和所有 worktree
//! 之间是同一个文件，写进去会连主仓库的 status 一起改掉。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 默认跳过的可重建产物目录。用户可在设置里改这份清单。
///
/// 只放"删掉能自动重建、且多 worktree 共享会出问题"的东西。IDE 配置（`.idea/`
/// 等）刻意不在此列——它们属于希望跨 worktree 共享的工具状态。
pub const DEFAULT_SKIP_PATTERNS: &[&str] = &[
    "target",
    "build",
    "dist",
    "out",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".dart_tool",
    "Pods",
    ".gradle",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    "DerivedData",
    ".DS_Store",
];

/// worktree 私有 excludes 文件在 worktree gitdir 里的文件名。
const EXCLUDES_FILE_NAME: &str = "smelt-inherit-excludes";

pub fn default_skip_patterns() -> Vec<String> {
    DEFAULT_SKIP_PATTERNS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// Worktree 未跟踪内容继承配置。
///
/// 默认关闭：这会改变 worktree 的既有行为，且对已存在的 worktree 不追溯，
/// 让用户显式开启比静默改变更安全。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInheritSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_skip_patterns")]
    pub skip_patterns: Vec<String>,
}

impl Default for WorktreeInheritSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            skip_patterns: default_skip_patterns(),
        }
    }
}

impl WorktreeInheritSettings {
    /// 设置页把清单当一行文本编辑，这里做「文本 ↔ 清单」的转换。
    /// 逗号和空白都算分隔符，空项丢弃。
    pub fn skip_patterns_text(&self) -> String {
        self.skip_patterns.join(", ")
    }

    pub fn set_skip_patterns_from_text(&mut self, text: &str) {
        self.skip_patterns = parse_skip_patterns(text);
    }
}

pub fn parse_skip_patterns(text: &str) -> Vec<String> {
    text.split([',', '\n', '\t', ' '])
        .map(|s| s.trim().trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn load_settings() -> WorktreeInheritSettings {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return WorktreeInheritSettings::default();
    };
    match store.get_worktree_inherit_snapshot() {
        Ok(Some(snapshot)) => settings_from_snapshot(snapshot),
        Ok(None) | Err(_) => WorktreeInheritSettings::default(),
    }
}

pub fn save_settings(settings: &WorktreeInheritSettings) {
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        let _ = store.put_worktree_inherit_snapshot(&snapshot_from_settings(settings));
    }
}

fn snapshot_from_settings(
    settings: &WorktreeInheritSettings,
) -> smelt_store::WorktreeInheritSnapshot {
    smelt_store::WorktreeInheritSnapshot {
        enabled: settings.enabled,
        skip_patterns: settings.skip_patterns.clone(),
    }
}

fn settings_from_snapshot(
    snapshot: smelt_store::WorktreeInheritSnapshot,
) -> WorktreeInheritSettings {
    WorktreeInheritSettings {
        enabled: snapshot.enabled,
        skip_patterns: snapshot.skip_patterns,
    }
}

/// 一次继承的结果。任何一条失败都不该让 worktree 创建整体失败，
/// 所以错误收集进 `warnings` 由调用方打日志。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InheritReport {
    /// 实际建立软链的相对路径。
    pub linked: Vec<String>,
    /// 命中跳过清单的条目数。
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// 按当前设置继承；关闭时直接返回空结果。供 worktree 创建路径调用。
pub fn inherit_if_enabled(main_root: &Path, worktree_dir: &Path) -> InheritReport {
    let settings = load_settings();
    if !settings.enabled {
        return InheritReport::default();
    }
    inherit_untracked(main_root, worktree_dir, &settings.skip_patterns)
}

/// 把 `main_root` 下未跟踪的条目软链进 `worktree_dir`，并写 worktree 私有 excludes。
///
/// 幂等：目标已存在（哪怕是悬空软链）就跳过，可以重复调用做「重新同步」。
pub fn inherit_untracked(
    main_root: &Path,
    worktree_dir: &Path,
    skip_patterns: &[String],
) -> InheritReport {
    let mut report = InheritReport::default();
    let entries = match untracked_entries(main_root) {
        Ok(entries) => entries,
        Err(err) => {
            report.warnings.push(format!("枚举未跟踪文件失败: {err}"));
            return report;
        }
    };

    // 软链的目标必须写绝对路径：相对路径会被当成「相对软链所在目录」解析，
    // 主仓库根若是相对路径就会指向 worktree 里根本不存在的位置。
    let main_root = main_root
        .canonicalize()
        .unwrap_or_else(|_| main_root.to_path_buf());
    // 比较用规范化路径：worktree 可能落在主仓库内部（手动新建时用户可以给任意
    // 路径），那样它自己就会被枚举成一个未跟踪目录，链过去就是自己链自己。
    let canonical_worktree = worktree_dir
        .canonicalize()
        .unwrap_or_else(|_| worktree_dir.to_path_buf());

    for entry in entries {
        let rel = entry.trim_end_matches('/');
        if rel.is_empty() || rel == ".git" {
            continue;
        }
        let src = main_root.join(rel);
        let canonical_src = src.canonicalize().unwrap_or_else(|_| src.clone());
        if canonical_worktree.starts_with(&canonical_src) {
            continue;
        }
        if should_skip(rel, skip_patterns) {
            report.skipped += 1;
            continue;
        }
        let dst = worktree_dir.join(rel);
        // symlink_metadata 而不是 exists：后者对悬空软链返回 false，会导致我们
        // 反复尝试在已有软链的位置再建一个。
        if dst.symlink_metadata().is_ok() {
            continue;
        }
        if let Some(parent) = dst.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            report
                .warnings
                .push(format!("创建目录失败 {}: {err}", parent.display()));
            continue;
        }
        match link_or_copy(&src, &dst) {
            Ok(()) => report.linked.push(rel.to_string()),
            Err(err) => report.warnings.push(format!("继承 {rel} 失败: {err}")),
        }
    }

    if !report.linked.is_empty()
        && let Err(err) = write_worktree_excludes(&main_root, worktree_dir, &report.linked)
    {
        report
            .warnings
            .push(format!("写 worktree 私有 excludes 失败: {err}"));
    }
    report
}

#[cfg(unix)]
fn link_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// 非 Unix（Windows）建软链要开发者模式或管理员权限，普通用户大概率失败，
/// 直接退化成复制：能用比报错强，代价是配置会漂移。
#[cfg(not(unix))]
fn link_or_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        copy_dir_recursive(src, dst)
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

#[cfg(not(unix))]
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// 主仓库根下所有未跟踪条目（**包含**被 ignore 的）。
///
/// 不加 `--exclude-standard` 才会把 ignore 的也列出来——`.env`、`target/` 正是
/// 我们要处理的对象。`--directory` 把「整个都没被跟踪」的目录折叠成一条，避免把
/// `target/` 里几万个文件逐个列出来。`-z` 保证带空格/换行的文件名不会被切错。
fn untracked_entries(main_root: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(main_root)
        .args(["ls-files", "--others", "--directory", "-z"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "git ls-files 失败".to_string()
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

/// 跳过判定：清单里的模式对**完整相对路径**或**最后一段**任一命中即跳过。
///
/// 按最后一段匹配是为了让 `build` 这样一条就能盖住 `mobile/build/`，不必让用户
/// 把每个嵌套位置都写一遍。模式支持 `*` / `?` 通配。
fn should_skip(rel: &str, patterns: &[String]) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim().trim_end_matches('/');
        !pattern.is_empty() && (glob_match(pattern, rel) || glob_match(pattern, base))
    })
}

/// 只支持 `*`（任意字符）和 `?`（单字符）的极简通配，够描述产物目录名了。
/// 双指针 + 回溯，不递归，避免病态模式炸栈。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut after_star) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            after_star = ti;
        } else if let Some(star_pos) = star {
            pi = star_pos + 1;
            after_star += 1;
            ti = after_star;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|ch| *ch == '*')
}

/// 生成并挂上 worktree 私有的 excludes 文件。
///
/// 必须用 `core.excludesFile` 而不是 `info/exclude`：后者在主仓库和所有 worktree
/// 之间共享，写进去会把主仓库的 status 也改掉。`--worktree` 作用域要求先打开
/// `extensions.worktreeConfig`。
fn write_worktree_excludes(
    main_root: &Path,
    worktree_dir: &Path,
    linked: &[String],
) -> Result<(), String> {
    let excludes_path = worktree_git_path(worktree_dir, EXCLUDES_FILE_NAME)?;

    // core.excludesFile 是单值，我们一设置就会顶掉用户原本的全局忽略规则，
    // 所以要把原内容先并进来。必须从主仓库读——从 worktree 读会读到我们上一次
    // 写进去的值，重复执行会自己套自己。
    let mut content = String::from("# 由 smelt 生成：worktree 继承的未跟踪条目\n");
    content.push_str("# 这些是指向主仓库的软链，不该出现在 git status 里。\n");
    if let Some(inherited) = global_excludes_content(main_root) {
        content.push_str("\n# --- 以下来自用户原有的 core.excludesFile ---\n");
        content.push_str(&inherited);
        if !inherited.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("# --- 用户规则结束 ---\n\n");
    }
    for rel in linked {
        content.push_str(&escape_gitignore(rel));
        content.push('\n');
    }

    if let Some(parent) = excludes_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    std::fs::write(&excludes_path, content).map_err(|err| err.to_string())?;

    run_git_ok(
        worktree_dir,
        &["config", "extensions.worktreeConfig", "true"],
    )?;
    run_git_ok(
        worktree_dir,
        &[
            "config",
            "--worktree",
            "core.excludesFile",
            &excludes_path.to_string_lossy(),
        ],
    )
}

/// worktree 私有 gitdir 下某个文件的绝对路径。
/// `git rev-parse --git-path` 在 worktree 里返回的是它自己的 gitdir
/// （`<main>/.git/worktrees/<name>/`），正是放私有文件的地方。
fn worktree_git_path(worktree_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_dir)
        .args(["rev-parse", "--git-path", name])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Err("git rev-parse --git-path 返回空".to_string());
    }
    let path = PathBuf::from(&raw);
    // 主仓库里 git 会返回相对路径（相对 cwd，也就是我们传的 -C 目录）。
    Ok(if path.is_absolute() {
        path
    } else {
        worktree_dir.join(path)
    })
}

/// 用户原有的忽略规则内容：显式配置的 `core.excludesFile` 优先，
/// 没配则用 Git 的默认位置 `~/.config/git/ignore`。
fn global_excludes_content(main_root: &Path) -> Option<String> {
    let configured = Command::new("git")
        .arg("-C")
        .arg(main_root)
        .args(["config", "--get", "core.excludesFile"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let path = match configured {
        Some(raw) => expand_tilde(&raw)?,
        None => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
            base.join("git").join("ignore")
        }
    };
    std::fs::read_to_string(path).ok()
}

fn expand_tilde(raw: &str) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    Some(PathBuf::from(raw))
}

/// 把相对路径转成锚定在仓库根的 gitignore 条目，特殊字符逐个转义。
fn escape_gitignore(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len() + 2);
    out.push('/');
    for ch in rel.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\' | '!' | '#' | ' ') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn run_git_ok(dir: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git {} 失败", args.join(" "))
    } else {
        stderr
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_handles_literals_and_wildcards() {
        assert!(glob_match("target", "target"));
        assert!(!glob_match("target", "targets"));
        assert!(glob_match("*.log", "debug.log"));
        assert!(glob_match("build*", "build"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*c", "abbbc"));
        assert!(!glob_match("a*c", "abbb"));
        assert!(glob_match("?ods", "Pods"));
        assert!(!glob_match("?ods", "Pods2"));
    }

    #[test]
    fn should_skip_matches_full_path_or_last_segment() {
        let patterns = default_skip_patterns();
        assert!(should_skip("target", &patterns));
        // 一条 build 要能盖住嵌套位置，否则用户得把每个子项目都写一遍。
        assert!(should_skip("mobile/build", &patterns));
        assert!(should_skip("mobile/ios/Pods", &patterns));
        assert!(!should_skip(".env", &patterns));
        assert!(!should_skip("mobile/android/local.properties", &patterns));
    }

    #[test]
    fn should_skip_tolerates_trailing_slash_and_blank_patterns() {
        let patterns = vec!["dist/".to_string(), String::new(), "  ".to_string()];
        assert!(should_skip("dist", &patterns));
        // 空模式不能退化成「匹配一切」，否则清单里多一个逗号就全跳过了。
        assert!(!should_skip(".env", &patterns));
    }

    #[test]
    fn parse_skip_patterns_splits_on_comma_and_whitespace() {
        let parsed = parse_skip_patterns("target, node_modules\n dist/ ,, ");
        assert_eq!(parsed, vec!["target", "node_modules", "dist"]);
    }

    #[test]
    fn skip_patterns_text_round_trips() {
        let mut settings = WorktreeInheritSettings::default();
        settings.set_skip_patterns_from_text("a, b");
        assert_eq!(settings.skip_patterns, vec!["a", "b"]);
        assert_eq!(settings.skip_patterns_text(), "a, b");
    }

    #[test]
    fn settings_default_is_disabled_with_builtin_skips() {
        let settings = WorktreeInheritSettings::default();
        assert!(!settings.enabled, "改变既有行为的功能默认必须是关的");
        assert!(settings.skip_patterns.contains(&"target".to_string()));
    }

    #[test]
    fn settings_missing_fields_fall_back_to_defaults() {
        // 老配置文件没有这些字段，不能反序列化成空清单——那样 target/ 就会被链。
        let settings: WorktreeInheritSettings = serde_json::from_str("{}").unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.skip_patterns, default_skip_patterns());
    }

    #[test]
    fn explicit_empty_skip_patterns_survive_snapshot_roundtrip() {
        let settings = WorktreeInheritSettings {
            enabled: true,
            skip_patterns: Vec::new(),
        };
        let restored = settings_from_snapshot(snapshot_from_settings(&settings));
        assert!(restored.enabled);
        assert!(
            restored.skip_patterns.is_empty(),
            "用户显式清空跳过清单后，重启不得恢复内置列表"
        );
        let missing: WorktreeInheritSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(missing.skip_patterns, default_skip_patterns());
    }

    #[test]
    fn escape_gitignore_anchors_and_escapes() {
        assert_eq!(escape_gitignore(".env"), "/.env");
        assert_eq!(escape_gitignore("a b"), "/a\\ b");
        assert_eq!(escape_gitignore("weird#name"), "/weird\\#name");
        assert_eq!(escape_gitignore("g[0]*"), "/g\\[0\\]\\*");
    }

    // —— 以下是打真 git 的端到端测试：纯函数测不出软链、excludes 作用域这些
    // 真正容易出错的地方 ——

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// 建一个主仓库，带：被跟踪文件、`.gitignore`（含带斜杠的目录规则）、
    /// 未跟踪的配置文件、被 ignore 的产物目录、嵌套未跟踪文件。
    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "smelt-inherit-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        std::fs::create_dir_all(main.join("nested")).unwrap();

        std::fs::write(main.join("tracked.txt"), "t\n").unwrap();
        // 带斜杠的规则只匹配目录——软链会失配，正是要验的坑。
        std::fs::write(main.join(".gitignore"), "dist/\n.env\n").unwrap();
        std::fs::write(main.join("nested/tracked.txt"), "t\n").unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@t"]);
        git(&main, &["config", "user.name", "t"]);
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-qm", "init"]);

        std::fs::write(main.join(".env"), "SECRET=1\n").unwrap();
        std::fs::create_dir_all(main.join("dist")).unwrap();
        std::fs::write(main.join("dist/bundle.js"), "x\n").unwrap();
        std::fs::create_dir_all(main.join("target")).unwrap();
        std::fs::write(main.join("target/artifact"), "x\n").unwrap();
        // nested/ 有被跟踪文件，所以里面的未跟踪文件会被单独列出来，
        // 目标端的父目录已存在，正好覆盖嵌套路径。
        std::fs::write(main.join("nested/local.conf"), "k=v\n").unwrap();

        let wt = root.join("wt");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );
        (main, wt)
    }

    #[test]
    fn inherits_untracked_config_and_skips_build_output() {
        let (main, wt) = fixture("basic");
        let report = inherit_untracked(&main, &wt, &default_skip_patterns());
        assert!(
            report.warnings.is_empty(),
            "不该有告警: {:?}",
            report.warnings
        );

        // 配置继承过来了，且读到的是主仓库那份内容。
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=1\n"
        );
        assert!(
            wt.join(".env")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // 嵌套路径也要能继承。
        assert_eq!(
            std::fs::read_to_string(wt.join("nested/local.conf")).unwrap(),
            "k=v\n"
        );
        // 产物目录必须跳过，否则多个 worktree 会抢同一个构建目录。
        assert!(wt.join("target").symlink_metadata().is_err());
        assert!(wt.join("dist").symlink_metadata().is_err());
        assert!(report.skipped >= 2);
    }

    #[test]
    fn linked_symlinks_do_not_pollute_worktree_status() {
        let (main, wt) = fixture("status");
        // 让 dist 也被继承，制造「带斜杠规则匹配不到软链」的场景。
        let patterns = vec!["target".to_string()];
        let report = inherit_untracked(&main, &wt, &patterns);
        assert!(report.linked.iter().any(|p| p == "dist"), "dist 应被继承");

        let status = git_stdout(&wt, &["status", "--porcelain"]);
        assert!(
            status.trim().is_empty(),
            "worktree status 应保持干净，实际: {status}"
        );
    }

    #[test]
    fn worktree_excludes_do_not_leak_into_main_repo() {
        let (main, wt) = fixture("scope");
        // 主仓库里放一个未被 ignore 的未跟踪文件，它必须继续出现在主仓库 status 里。
        std::fs::write(main.join("visible.txt"), "v\n").unwrap();
        inherit_untracked(&main, &wt, &default_skip_patterns());

        let main_status = git_stdout(&main, &["status", "--porcelain"]);
        assert!(
            main_status.contains("visible.txt"),
            "主仓库 status 不该被 worktree 的排除规则影响: {main_status}"
        );
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let (main, wt) = fixture("override");
        // 用户想要一份不一样的 .env：删掉软链自己放真文件，重跑不能被覆盖回去。
        std::fs::write(wt.join(".env"), "SECRET=local\n").unwrap();
        let report = inherit_untracked(&main, &wt, &default_skip_patterns());
        assert!(!report.linked.iter().any(|p| p == ".env"));
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=local\n"
        );
    }

    #[test]
    fn is_idempotent_across_repeated_runs() {
        let (main, wt) = fixture("idempotent");
        let first = inherit_untracked(&main, &wt, &default_skip_patterns());
        assert!(!first.linked.is_empty());
        // 第二次是「重新同步」的语义：已有的不重复建，也不该报错。
        let second = inherit_untracked(&main, &wt, &default_skip_patterns());
        assert!(
            second.linked.is_empty(),
            "重复执行不该再建: {:?}",
            second.linked
        );
        assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    }

    #[test]
    fn writing_through_symlink_reaches_main_repo() {
        let (main, wt) = fixture("shared");
        inherit_untracked(&main, &wt, &default_skip_patterns());
        // 这是软链方案的既定语义（单一真源），用测试把它钉住：
        // 哪天改成复制，这里会失败，提醒同步更新设置页的说明。
        std::fs::write(wt.join(".env"), "SECRET=2\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(main.join(".env")).unwrap(),
            "SECRET=2\n"
        );
    }

    #[test]
    fn removing_worktree_leaves_main_repo_intact() {
        let (main, wt) = fixture("remove");
        inherit_untracked(&main, &wt, &default_skip_patterns());
        git(
            &main,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        // git 不跟随软链删除，主仓库内容必须完好——这是本方案敢用软链的前提。
        assert_eq!(
            std::fs::read_to_string(main.join(".env")).unwrap(),
            "SECRET=1\n"
        );
        assert!(main.join("nested/local.conf").is_file());
    }
}
