//! 智能体记忆目录的只读视图和删除操作，供桌面端记忆卡片使用。
//!
//! 一条记忆一个 `<slug>.md`，写入由 Pi 启动器里的 `smelt-memory` 扩展校验，注入用的索引也由
//! 它按 frontmatter 生成（`packages/pi-agent/src/memory-files.ts`）。这里只列出和删除，
//! 格式规则与那边保持一致。
//! 设计见 `docs/agent-memory.md`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 与 `memory-usage.ts` 的 `MEMORY_USAGE_DB` 一致：每个记忆目录里的使用统计。
pub const MEMORY_USAGE_DB: &str = ".usage.sqlite";
/// 事实类记忆这么久既没被读过也没更新过，就在界面上标成"长期未用"，由用户决定删不删。
/// 规则类（user / feedback）是用户给的约束，不按使用情况淘汰。
pub const MEMORY_STALE_AFTER: Duration = Duration::from_secs(60 * 24 * 60 * 60);

/// 与 `memory-files.ts` 的 `CORE_MEMORY_MAX_BYTES` 一致：每层 user / feedback 规则正文的总预算。
pub const CORE_MEMORY_MAX_BYTES: usize = 3_000;
const CORE_MEMORY_TYPES: [&str; 2] = ["user", "feedback"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryEntry {
    pub file_name: String,
    pub name: String,
    pub description: String,
    /// frontmatter 的 type；手改坏了就是 None，照样列出来让用户能删。
    pub kind: Option<String>,
    pub path: PathBuf,
    /// 模型 read 过这条记忆的次数。
    pub read_count: u64,
    /// 最近一次被读或被修改的时间。
    pub last_used: Option<SystemTime>,
    /// 事实类且超过 [`MEMORY_STALE_AFTER`] 没用过。
    pub stale: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryListing {
    pub entries: Vec<MemoryEntry>,
    /// 规则正文超出常驻预算（通常是手改的），启动时只会注入一部分。
    pub core_over_budget: bool,
}

/// 记忆文件名：小写字母、数字、连字符组成的 slug 加 `.md`，最长 64 个字符。
pub fn is_memory_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let mut chars = stem.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit())
        && stem.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 拆出 frontmatter 和正文；没有 frontmatter 时返回 None。
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?.trim_start_matches('\r');
    let rest = rest.strip_prefix('\n')?;
    let end = rest.find("\n---")?;
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    Some((&rest[..end], body))
}

fn frontmatter_field<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    frontmatter.lines().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        (field.trim() == key).then(|| {
            let value = value.trim();
            value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value)
        })
    })
}

#[derive(Clone, Copy)]
struct Usage {
    read_count: u64,
    last_read: SystemTime,
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn read_usage(dir: &Path) -> HashMap<String, Usage> {
    use rusqlite::{Connection, OpenFlags};
    let path = dir.join(MEMORY_USAGE_DB);
    if !path.is_file() {
        return HashMap::new();
    }
    let read = || -> rusqlite::Result<HashMap<String, Usage>> {
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        let mut statement =
            conn.prepare("SELECT file_name, read_count, last_read_at_ms FROM memory_usage")?;
        let rows = statement.query_map([], |row| {
            let millis: i64 = row.get(2)?;
            Ok((
                row.get::<_, String>(0)?,
                Usage {
                    read_count: row.get::<_, i64>(1)?.max(0) as u64,
                    last_read: UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64),
                },
            ))
        })?;
        rows.collect()
    };
    read().unwrap_or_default()
}

#[cfg(any(target_os = "ios", target_os = "android"))]
fn read_usage(_dir: &Path) -> HashMap<String, Usage> {
    HashMap::new()
}

pub fn list_memories(dir: &Path) -> MemoryListing {
    list_memories_at(dir, SystemTime::now())
}

fn list_memories_at(dir: &Path, now: SystemTime) -> MemoryListing {
    let usage = read_usage(dir);
    let mut entries = Vec::new();
    let mut core_bytes = 0;
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !is_memory_file_name(&file_name) {
                continue;
            }
            let path = entry.path();
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (frontmatter, body) = split_frontmatter(&content).unwrap_or(("", ""));
            let field = |key| frontmatter_field(frontmatter, key).filter(|value| !value.is_empty());
            let kind = field("type").map(str::to_string);
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
            let used = usage.get(&file_name).copied();
            let last_used = match (modified, used.map(|usage| usage.last_read)) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            let is_fact = matches!(kind.as_deref(), Some("project" | "reference"));
            let stale = is_fact
                && last_used.is_some_and(|last| {
                    now.duration_since(last)
                        .is_ok_and(|age| age > MEMORY_STALE_AFTER)
                });
            if kind
                .as_deref()
                .is_some_and(|kind| CORE_MEMORY_TYPES.contains(&kind))
            {
                core_bytes += body.trim().len();
            }
            entries.push(MemoryEntry {
                name: field("name")
                    .unwrap_or(file_name.trim_end_matches(".md"))
                    .to_string(),
                description: field("description").unwrap_or_default().to_string(),
                kind,
                file_name,
                path,
                read_count: used.map(|usage| usage.read_count).unwrap_or(0),
                last_used,
                stale,
            });
        }
    }
    entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    MemoryListing {
        entries,
        core_over_budget: core_bytes > CORE_MEMORY_MAX_BYTES,
    }
}

/// 删掉一条记忆。索引由启动器按 frontmatter 生成，删文件就够了。
pub fn delete_memory(dir: &Path, file_name: &str) -> std::io::Result<()> {
    if !is_memory_file_name(file_name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("不是记忆文件：{file_name}"),
        ));
    }
    match std::fs::remove_file(dir.join(file_name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(name: &str, kind: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\ntype: {kind}\n---\n{body}\n")
    }

    #[test]
    fn lists_memory_files_with_their_frontmatter_and_skips_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("reply-in-chinese.md"),
            memory("reply-in-chinese", "feedback", "回复用中文", "正文"),
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.md"), "手改坏了").unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "- [a](a.md) — a\n").unwrap();
        std::fs::write(dir.path().join(".draft.md"), "x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();

        let listing = list_memories(dir.path());

        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].file_name, "broken.md");
        assert_eq!(listing.entries[0].name, "broken");
        assert_eq!(listing.entries[0].kind, None);
        assert_eq!(listing.entries[1].description, "回复用中文");
        assert_eq!(listing.entries[1].kind.as_deref(), Some("feedback"));
        assert!(!listing.core_over_budget);
    }

    #[test]
    fn a_missing_directory_is_just_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            list_memories(&dir.path().join("nope")),
            MemoryListing::default()
        );
    }

    #[test]
    fn flags_rules_that_exceed_the_resident_budget() {
        let dir = tempfile::tempdir().unwrap();
        let half = "规".repeat(CORE_MEMORY_MAX_BYTES / 3 / 2 + 10);
        std::fs::write(
            dir.path().join("fact.md"),
            memory("fact", "project", "f", &"事".repeat(2_000)),
        )
        .unwrap();
        std::fs::write(dir.path().join("a.md"), memory("a", "feedback", "a", &half)).unwrap();
        assert!(!list_memories(dir.path()).core_over_budget);
        std::fs::write(dir.path().join("b.md"), memory("b", "user", "b", &half)).unwrap();
        assert!(list_memories(dir.path()).core_over_budget);
    }

    #[test]
    fn reads_usage_written_by_the_pi_launcher_and_flags_long_unused_facts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fact.md"),
            memory("fact", "reference", "f", "x"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rule.md"),
            memory("rule", "feedback", "r", "x"),
        )
        .unwrap();
        let read_at = SystemTime::now() + Duration::from_secs(60);
        {
            let conn = rusqlite::Connection::open(dir.path().join(MEMORY_USAGE_DB)).unwrap();
            conn.execute_batch(
                "CREATE TABLE memory_usage (file_name TEXT PRIMARY KEY, read_count INTEGER NOT NULL, last_read_at_ms INTEGER NOT NULL) STRICT",
            )
            .unwrap();
            let millis = read_at.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
            conn.execute(
                "INSERT INTO memory_usage VALUES ('fact.md', 3, ?1)",
                [millis],
            )
            .unwrap();
        }

        let fresh = list_memories(dir.path());
        let fact = &fresh.entries[0];
        assert_eq!(fact.read_count, 3);
        assert!(fact.last_used.unwrap() >= read_at - Duration::from_millis(1));
        assert!(!fact.stale);

        let later = list_memories_at(dir.path(), read_at + MEMORY_STALE_AFTER * 2);
        assert!(later.entries[0].stale, "事实类长期没用要标出来");
        assert!(!later.entries[1].stale, "规则类不按使用情况淘汰");
    }

    #[test]
    fn deleting_a_memory_removes_only_that_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), memory("a", "user", "A", "x")).unwrap();
        std::fs::write(dir.path().join("ab.md"), memory("ab", "user", "AB", "x")).unwrap();

        delete_memory(dir.path(), "a.md").unwrap();
        delete_memory(dir.path(), "a.md").unwrap();

        assert!(!dir.path().join("a.md").exists());
        assert!(dir.path().join("ab.md").exists());
    }

    #[test]
    fn deleting_refuses_anything_that_is_not_a_memory_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "keep\n").unwrap();
        for name in ["MEMORY.md", "../escape.md", "Bad.md", "a.txt"] {
            assert!(delete_memory(dir.path(), name).is_err(), "{name}");
        }
        assert!(dir.path().join("MEMORY.md").exists());
    }

    #[test]
    fn memory_file_names_follow_the_writer_rules() {
        for ok in ["a.md", "reply-in-chinese.md", "0-start.md"] {
            assert!(is_memory_file_name(ok), "{ok}");
        }
        for bad in [
            "MEMORY.md",
            "-a.md",
            "A.md",
            "a b.md",
            "a.txt",
            ".md",
            "../a.md",
        ] {
            assert!(!is_memory_file_name(bad), "{bad}");
        }
    }
}
