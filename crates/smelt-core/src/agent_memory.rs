//! 智能体记忆：桌面端记忆卡片的列出、删除、采纳，以及 Pi 启动器读写记忆元数据的宿主协议。
//!
//! 一条记忆一个 `<slug>.md`，写入由 Pi 启动器里的 `smelt-memory` 扩展校验，注入用的索引也由
//! 它按 frontmatter 生成（`packages/pi-agent/src/memory-files.ts`），格式规则与那边保持一致。
//! 元数据（使用统计、来源）在主库，经 `smelt-store` 读写。Pi 进程不碰数据库，也不能交给
//! 托管它的会话宿主（会话宿主不初始化 SQLite）：它带着会话 id 和 capability token 调主
//! daemon 的 `agent.memory.*` 控制方法，由主 daemon 调这里的 [`launch_memory_meta`] 等函数。
//! 设计见 `docs/agent-memory.md`。

pub use smelt_model::agent_memory::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// 记忆所在的层，决定元数据的 scope。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryScope {
    Agent(String),
    User,
}

impl MemoryScope {
    pub fn key(&self) -> String {
        match self {
            Self::Agent(id) => smelt_store::memory_scope_for_agent(id),
            Self::User => smelt_store::USER_MEMORY_SCOPE.to_string(),
        }
    }

    /// 由启动器拿到的记忆目录反推它属于哪一层；只认 Smelt 自己分配的两种目录。
    pub fn for_dir(dir: &Path) -> Option<Self> {
        if crate::agent_definition_store::user_memory_dir().as_deref() == Some(dir) {
            return Some(Self::User);
        }
        let id = dir.parent()?.file_name()?.to_str()?;
        (crate::agent_definition_store::agent_memory_dir(id).as_deref() == Some(dir))
            .then(|| Self::Agent(id.to_string()))
    }
}

struct StoreMeta {
    usage: HashMap<String, Usage>,
    /// 显式待确认：文件 → 原因。
    pending: HashMap<String, String>,
    /// 显式可信：文件 → written_at_ms。
    trusted: HashMap<String, i64>,
}

/// 文件 mtime 比来源记录晚这么多，就当作来源记下之后被外部改过（含 bash 直写覆盖）。
/// 给写入再记来源留一点文件系统时间戳粒度余量。
const EXTERNAL_MODIFY_GRACE_MS: i64 = 5_000;

const NO_PROVENANCE_REASON: &str = "无来源记录，需用户采纳后生效";
const EXTERNAL_MODIFY_REASON: &str = "文件在记录来源后被外部修改，需重新采纳";
const LEGACY_BACKFILL_REASON: &str = "升级迁移：升级前已有的记忆";

fn store_meta(store: Option<&smelt_store::Store>, scope: &MemoryScope) -> StoreMeta {
    let snapshot = store
        .and_then(|store| store.memory_meta(&scope.key()).ok())
        .unwrap_or_default();
    StoreMeta {
        usage: snapshot
            .usage
            .into_iter()
            .map(|record| {
                (
                    record.file_name,
                    Usage {
                        read_count: record.read_count,
                        last_read: UNIX_EPOCH
                            + Duration::from_millis(record.last_read_at_ms.max(0) as u64),
                    },
                )
            })
            .collect(),
        pending: snapshot.pending.into_iter().collect(),
        trusted: snapshot
            .trusted
            .into_iter()
            .map(|record| (record.file_name, record.written_at_ms))
            .collect(),
    }
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis()
            .min(i64::MAX as u128) as i64,
    )
}

/// 一条磁盘上的记忆是否应视为待确认，以及原因。
fn pending_reason_for(
    file_name: &str,
    path: &Path,
    meta: &StoreMeta,
) -> Option<String> {
    if let Some(reason) = meta.pending.get(file_name) {
        return Some(reason.clone());
    }
    match meta.trusted.get(file_name) {
        Some(&written_at_ms) => {
            let Some(mtime) = mtime_ms(path) else {
                return Some(EXTERNAL_MODIFY_REASON.to_string());
            };
            (mtime > written_at_ms + EXTERNAL_MODIFY_GRACE_MS)
                .then(|| EXTERNAL_MODIFY_REASON.to_string())
        }
        None => Some(NO_PROVENANCE_REASON.to_string()),
    }
}

fn store() -> std::io::Result<smelt_store::Store> {
    crate::sqlite_state::default_sqlite_store().map_err(std::io::Error::other)
}

/// 升级时一次性：把磁盘上还没有来源记录的记忆文件记成 trusted。
/// 哨兵行保证只跑一次；之后新出现的无记录文件一律待确认。
pub fn ensure_legacy_trust_backfill(store: &smelt_store::Store) -> Result<(), MemoryRequestError> {
    if store
        .memory_legacy_trust_backfill_done()
        .map_err(|error| MemoryRequestError::Store(error.to_string()))?
    {
        return Ok(());
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut scopes: Vec<(MemoryScope, PathBuf)> = Vec::new();
    if let Some(dir) = crate::agent_definition_store::user_memory_dir() {
        scopes.push((MemoryScope::User, dir));
    }
    if let Ok(agents) = crate::agent_definition_store::list_agent_definitions_on(store) {
        for agent in agents {
            if let Some(dir) = crate::agent_definition_store::agent_memory_dir(&agent.id) {
                scopes.push((MemoryScope::Agent(agent.id), dir));
            }
        }
    }
    for (scope, dir) in scopes {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !is_memory_file_name(&file_name) {
                continue;
            }
            let path = entry.path();
            let meta = store
                .memory_meta(&scope.key())
                .map_err(|error| MemoryRequestError::Store(error.to_string()))?;
            let known = meta
                .pending
                .iter()
                .any(|(name, _)| name == &file_name)
                || meta.trusted.iter().any(|record| record.file_name == file_name);
            if known {
                continue;
            }
            let written_at = mtime_ms(&path).unwrap_or(now_ms);
            store
                .record_memory_write(
                    &scope.key(),
                    &file_name,
                    smelt_store::MemoryTrust::Trusted,
                    LEGACY_BACKFILL_REASON,
                    "upgrade",
                    written_at,
                )
                .map_err(|error| MemoryRequestError::Store(error.to_string()))?;
        }
    }
    store
        .mark_memory_legacy_trust_backfill_done(now_ms)
        .map_err(|error| MemoryRequestError::Store(error.to_string()))?;
    Ok(())
}

/// 一层记忆里不应注入的文件名（显式 pending + 无记录 + 来源后被外部改过）。
pub fn effective_pending_files(
    dir: &Path,
    store: &smelt_store::Store,
    scope: &MemoryScope,
) -> Result<Vec<String>, MemoryRequestError> {
    let _ = ensure_legacy_trust_backfill(store);
    let meta = store_meta(Some(store), scope);
    let mut pending: std::collections::BTreeSet<String> =
        meta.pending.keys().cloned().collect();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !is_memory_file_name(&file_name) {
                continue;
            }
            if pending_reason_for(&file_name, &entry.path(), &meta).is_some() {
                pending.insert(file_name);
            }
        }
    }
    Ok(pending.into_iter().collect())
}

pub fn list_memories(dir: &Path, scope: &MemoryScope) -> MemoryListing {
    let store = store().ok();
    if let Some(store) = store.as_ref() {
        let _ = ensure_legacy_trust_backfill(store);
    }
    list_memories_with(dir, store.as_ref(), scope, SystemTime::now())
}

fn list_memories_with(
    dir: &Path,
    store: Option<&smelt_store::Store>,
    scope: &MemoryScope,
    now: SystemTime,
) -> MemoryListing {
    let meta = store_meta(store, scope);
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
            let shown_body = if frontmatter.is_empty() {
                content.trim()
            } else {
                body.trim()
            };
            let field = |key| frontmatter_field(frontmatter, key).filter(|value| !value.is_empty());
            let kind = field("type").map(str::to_string);
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
            let used = meta.usage.get(&file_name).copied();
            let pending_reason = pending_reason_for(&file_name, &path, &meta);
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
            if pending_reason.is_none() && is_rule_memory_kind(kind.as_deref()) {
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
                pending_reason,
                body: shown_body.to_string(),
            });
        }
    }
    entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    MemoryListing {
        entries,
        core_over_budget: core_bytes > CORE_MEMORY_MAX_BYTES,
    }
}

fn ensure_memory_file_name(file_name: &str) -> std::io::Result<()> {
    if is_memory_file_name(file_name) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("不是记忆文件：{file_name}"),
        ))
    }
}

/// 删掉一条记忆和它的元数据。索引由启动器按 frontmatter 生成，不用另外改。
pub fn delete_memory(dir: &Path, scope: &MemoryScope, file_name: &str) -> std::io::Result<()> {
    delete_memory_with(&store()?, dir, scope, file_name)
}

fn delete_memory_with(
    store: &smelt_store::Store,
    dir: &Path,
    scope: &MemoryScope,
    file_name: &str,
) -> std::io::Result<()> {
    ensure_memory_file_name(file_name)?;
    match std::fs::remove_file(dir.join(file_name)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    // 同名记忆以后重建时，不该继承这条的使用次数和待确认状态。
    store
        .forget_memory(&scope.key(), file_name)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

fn layer_scope_and_dir(
    agent_id: &str,
    layer: crate::control_api::AgentMemoryLayer,
) -> Option<(MemoryScope, PathBuf)> {
    use crate::control_api::AgentMemoryLayer;
    match layer {
        AgentMemoryLayer::Agent => Some((
            MemoryScope::Agent(agent_id.to_string()),
            crate::agent_definition_store::agent_memory_dir(agent_id)?,
        )),
        AgentMemoryLayer::User => Some((
            MemoryScope::User,
            crate::agent_definition_store::user_memory_dir()?,
        )),
    }
}

/// 列出某个智能体的两层记忆。目录在运行时那台机器上。
pub fn list_for_agent(agent_id: &str) -> AgentMemories {
    use crate::control_api::AgentMemoryLayer;
    let list = |layer| {
        layer_scope_and_dir(agent_id, layer)
            .map(|(scope, dir)| list_memories(&dir, &scope))
            .unwrap_or_default()
    };
    AgentMemories {
        agent: list(AgentMemoryLayer::Agent),
        user: list(AgentMemoryLayer::User),
    }
}

pub fn delete_for_agent(
    agent_id: &str,
    layer: crate::control_api::AgentMemoryLayer,
    file_name: &str,
) -> std::io::Result<()> {
    let (scope, dir) = layer_scope_and_dir(agent_id, layer)
        .ok_or_else(|| std::io::Error::other("找不到记忆目录"))?;
    delete_memory(&dir, &scope, file_name)
}

pub fn approve_for_agent(
    agent_id: &str,
    layer: crate::control_api::AgentMemoryLayer,
    file_name: &str,
) -> std::io::Result<()> {
    let (scope, _) = layer_scope_and_dir(agent_id, layer)
        .ok_or_else(|| std::io::Error::other("找不到记忆目录"))?;
    approve_memory(&scope, file_name)
}

pub fn approve_memory(scope: &MemoryScope, file_name: &str) -> std::io::Result<()> {
    ensure_memory_file_name(file_name)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    store()?
        .approve_memory(&scope.key(), file_name, now_ms)
        .map(|_| ())
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// 会话请求读写记忆元数据时的失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum MemoryRequestError {
    /// 这次启动没有这一层记忆（普通会话，或层与目录对不上）。
    NoSuchLayer,
    /// 不是记忆文件名。
    InvalidFile,
    /// 智能体的记忆已经关掉。
    Disabled,
    Store(String),
}

impl std::fmt::Display for MemoryRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchLayer => formatter.write_str("这次会话没有这一层记忆"),
            Self::InvalidFile => formatter.write_str("不是记忆文件"),
            Self::Disabled => formatter.write_str("这个智能体的记忆已关闭"),
            Self::Store(error) => formatter.write_str(error),
        }
    }
}

/// 会话的一层记忆目录 → scope。目录只认 Smelt 分配的那两种，层也必须对得上。
pub fn launch_scope(
    dir: Option<&Path>,
    user_layer: bool,
) -> Result<MemoryScope, MemoryRequestError> {
    match dir.and_then(MemoryScope::for_dir) {
        Some(scope) if matches!(scope, MemoryScope::User) == user_layer => Ok(scope),
        _ => Err(MemoryRequestError::NoSuchLayer),
    }
}

/// 这次启动的智能体现在是否还开着记忆。会话启动后用户可能在配置页关掉；智能体被删了、
/// 或者这不是产品智能体的会话，都按关处理。
pub fn launch_memory_enabled(store: &smelt_store::Store, agent_dir: Option<&Path>) -> bool {
    match launch_scope(agent_dir, false) {
        Ok(MemoryScope::Agent(id)) => {
            crate::agent_definition_store::get_agent_definition_on(store, &id)
                .is_ok_and(|definition| definition.memory_enabled)
        }
        _ => false,
    }
}

/// 两层的使用统计和待确认列表，以及记忆是否还开着。关着时两层都为空。
///
/// `pending` 是**不应注入**的文件名：显式 pending、无来源记录、以及来源记下后被外部改过的。
pub fn launch_memory_meta(
    store: &smelt_store::Store,
    agent_dir: Option<&Path>,
    user_dir: Option<&Path>,
) -> Result<crate::control_api::AgentMemoryMetaResult, MemoryRequestError> {
    if !launch_memory_enabled(store, agent_dir) {
        return Ok(crate::control_api::AgentMemoryMetaResult::default());
    }
    let _ = ensure_legacy_trust_backfill(store);
    let layer = |scope: Result<MemoryScope, MemoryRequestError>, dir: Option<&Path>| {
        let Ok(scope) = scope else {
            return Ok(crate::control_api::AgentMemoryLayerMeta::default());
        };
        let meta = store
            .memory_meta(&scope.key())
            .map_err(|error| MemoryRequestError::Store(error.to_string()))?;
        let pending = match dir {
            Some(dir) => effective_pending_files(dir, store, &scope)?,
            None => meta.pending.into_iter().map(|(file, _)| file).collect(),
        };
        Ok(crate::control_api::AgentMemoryLayerMeta {
            usage: meta
                .usage
                .into_iter()
                .map(|record| crate::control_api::AgentMemoryUsageEntry {
                    file: record.file_name,
                    read_count: record.read_count,
                    last_read_at_ms: record.last_read_at_ms,
                })
                .collect(),
            pending,
        })
    };
    let embedding = crate::memory_settings::load_on(store)
        .ok()
        .filter(crate::memory_settings::EmbeddingSettings::is_configured)
        .map(|settings| crate::control_api::AgentMemoryEmbedding {
            base_url: settings.base_url,
            model: settings.model,
            api_key: settings.api_key,
        });
    let rerank_model = crate::memory_settings::load_rerank_model_on(store)
        .ok()
        .flatten()
        .map(|model| crate::control_api::AgentMemoryRerankModel {
            provider: model.provider,
            id: model.id,
        });
    Ok(crate::control_api::AgentMemoryMetaResult {
        enabled: true,
        embedding,
        rerank_model,
        agent: layer(launch_scope(agent_dir, false), agent_dir)?,
        user: layer(launch_scope(user_dir, true), user_dir)?,
    })
}

pub fn record_launch_read(
    store: &smelt_store::Store,
    scope: &MemoryScope,
    file: &str,
    now_ms: i64,
) -> Result<(), MemoryRequestError> {
    if !is_memory_file_name(file) {
        return Err(MemoryRequestError::InvalidFile);
    }
    store
        .record_memory_read(&scope.key(), file, now_ms)
        .map_err(|error| MemoryRequestError::Store(error.to_string()))
}

pub fn record_launch_write(
    store: &smelt_store::Store,
    scope: &MemoryScope,
    file: &str,
    trust: crate::control_api::AgentMemoryTrust,
    reason: &str,
    session_id: &str,
    now_ms: i64,
) -> Result<(), MemoryRequestError> {
    if !is_memory_file_name(file) {
        return Err(MemoryRequestError::InvalidFile);
    }
    let trust = match trust {
        crate::control_api::AgentMemoryTrust::Trusted => smelt_store::MemoryTrust::Trusted,
        crate::control_api::AgentMemoryTrust::Pending => smelt_store::MemoryTrust::Pending,
    };
    store
        .record_memory_write(&scope.key(), file, trust, reason, session_id, now_ms)
        .map_err(|error| MemoryRequestError::Store(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(name: &str, kind: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\ntype: {kind}\n---\n{body}\n")
    }

    fn temp_store(dir: &Path) -> smelt_store::Store {
        smelt_store::Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap()
    }

    fn agent() -> MemoryScope {
        MemoryScope::Agent("writer".into())
    }

    fn list(dir: &Path, store: &smelt_store::Store) -> MemoryListing {
        list_memories_with(dir, Some(store), &agent(), SystemTime::now())
    }

    #[test]
    fn lists_memory_files_with_their_frontmatter_and_skips_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("reply-in-chinese.md"),
            memory("reply-in-chinese", "feedback", "回复用中文", "正文"),
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.md"), "手改坏了").unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "- [a](a.md) — a\n").unwrap();
        std::fs::write(dir.path().join(".draft.md"), "x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();

        let listing = list(dir.path(), &temp_store(db.path()));

        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].file_name, "broken.md");
        assert_eq!(listing.entries[0].name, "broken");
        assert_eq!(listing.entries[0].kind, None);
        assert_eq!(listing.entries[1].description, "回复用中文");
        assert_eq!(listing.entries[1].kind.as_deref(), Some("feedback"));
        assert_eq!(
            listing.entries[1].body, "正文",
            "展开看的是去掉 frontmatter 的正文"
        );
        assert_eq!(
            listing.entries[0].body, "手改坏了",
            "格式坏了就给整个文件，用户才看得出哪里坏"
        );
        assert!(!listing.core_over_budget);
    }

    #[test]
    fn a_missing_directory_or_store_is_just_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            list_memories_with(&dir.path().join("nope"), None, &agent(), SystemTime::now()),
            MemoryListing::default()
        );
    }

    #[test]
    fn flags_rules_that_exceed_the_resident_budget() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        let half = "规".repeat(CORE_MEMORY_MAX_BYTES / 3 / 2 + 10);
        std::fs::write(
            dir.path().join("fact.md"),
            memory("fact", "project", "f", &"事".repeat(2_000)),
        )
        .unwrap();
        std::fs::write(dir.path().join("a.md"), memory("a", "feedback", "a", &half)).unwrap();
        store
            .record_memory_write(
                &agent().key(),
                "a.md",
                smelt_store::MemoryTrust::Trusted,
                "测试",
                "s",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        assert!(!list(dir.path(), &store).core_over_budget);
        std::fs::write(dir.path().join("b.md"), memory("b", "user", "b", &half)).unwrap();
        store
            .record_memory_write(
                &agent().key(),
                "b.md",
                smelt_store::MemoryTrust::Trusted,
                "测试",
                "s",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        assert!(list(dir.path(), &store).core_over_budget);
    }

    #[test]
    fn shows_usage_and_flags_long_unused_facts_but_never_rules() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
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
        let millis = read_at.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        for _ in 0..3 {
            store
                .record_memory_read(&agent().key(), "fact.md", millis)
                .unwrap();
        }

        let fresh = list(dir.path(), &store);
        assert_eq!(fresh.entries[0].read_count, 3);
        assert!(fresh.entries[0].last_used.unwrap() >= read_at - Duration::from_millis(1));
        assert!(!fresh.entries[0].stale);

        let later = list_memories_with(
            dir.path(),
            Some(&store),
            &agent(),
            read_at + MEMORY_STALE_AFTER * 2,
        );
        assert!(later.entries[0].stale, "事实类长期没用要标出来");
        assert!(!later.entries[1].stale, "规则类不按使用情况淘汰");
    }

    #[test]
    fn pending_memories_show_their_reason_and_do_not_count_toward_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        let big = "规".repeat(CORE_MEMORY_MAX_BYTES / 3 + 10);
        std::fs::write(
            dir.path().join("rule.md"),
            memory("rule", "feedback", "r", &big),
        )
        .unwrap();
        store
            .record_memory_write(
                &agent().key(),
                "rule.md",
                smelt_store::MemoryTrust::Pending,
                "写入的那一轮读取了外部内容（web_search）",
                "s",
                0,
            )
            .unwrap();

        let listing = list(dir.path(), &store);
        assert!(
            listing.entries[0]
                .pending_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("web_search"))
        );
        assert!(!listing.core_over_budget, "待确认的不注入，不占常驻预算");

        store
            .approve_memory(&agent().key(), "rule.md", chrono::Utc::now().timestamp_millis())
            .unwrap();
        let listing = list(dir.path(), &store);
        assert_eq!(listing.entries[0].pending_reason, None);
        assert!(listing.core_over_budget);
    }

    #[test]
    fn files_without_provenance_are_pending_and_do_not_count_toward_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        let big = "规".repeat(CORE_MEMORY_MAX_BYTES / 3 + 10);
        std::fs::write(
            dir.path().join("sneaky.md"),
            memory("sneaky", "feedback", "投毒", &big),
        )
        .unwrap();

        let listing = list(dir.path(), &store);
        assert_eq!(
            listing.entries[0].pending_reason.as_deref(),
            Some(NO_PROVENANCE_REASON)
        );
        assert!(
            !listing.core_over_budget,
            "无来源记录的不注入，不占常驻预算"
        );
    }

    #[test]
    fn trusted_memory_rewritten_on_disk_becomes_pending_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        let path = dir.path().join("rule.md");
        std::fs::write(&path, memory("rule", "feedback", "r", "旧正文")).unwrap();
        let written_at = mtime_ms(&path).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        store
            .record_memory_write(
                &agent().key(),
                "rule.md",
                smelt_store::MemoryTrust::Trusted,
                "用户在场时写入",
                "s",
                written_at,
            )
            .unwrap();
        assert_eq!(list(dir.path(), &store).entries[0].pending_reason, None);

        // 把来源时间戳拨回很早，再覆盖文件：不依赖文件系统 mtime 粒度。
        store
            .record_memory_write(
                &agent().key(),
                "rule.md",
                smelt_store::MemoryTrust::Trusted,
                "用户在场时写入",
                "s",
                1,
            )
            .unwrap();
        std::fs::write(&path, memory("rule", "feedback", "r", "被 bash 覆盖的投毒")).unwrap();
        let listing = list(dir.path(), &store);
        assert_eq!(
            listing.entries[0].pending_reason.as_deref(),
            Some(EXTERNAL_MODIFY_REASON)
        );
    }

    #[test]
    fn deleting_a_memory_removes_only_that_file_and_its_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        std::fs::write(dir.path().join("a.md"), memory("a", "user", "A", "x")).unwrap();
        std::fs::write(dir.path().join("ab.md"), memory("ab", "user", "AB", "x")).unwrap();
        store.record_memory_read(&agent().key(), "a.md", 0).unwrap();

        delete_memory_with(&store, dir.path(), &agent(), "a.md").unwrap();
        delete_memory_with(&store, dir.path(), &agent(), "a.md").unwrap();

        assert!(!dir.path().join("a.md").exists());
        assert!(dir.path().join("ab.md").exists());
        assert!(store.memory_meta(&agent().key()).unwrap().usage.is_empty());
        for name in ["MEMORY.md", "../escape.md", "Bad.md", "a.txt"] {
            assert!(
                delete_memory_with(&store, dir.path(), &agent(), name).is_err(),
                "{name}"
            );
        }
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

    #[test]
    fn scopes_are_only_derived_from_smelt_assigned_directories() {
        let agent_dir = crate::agent_definition_store::agent_memory_dir("writer").unwrap();
        let user_dir = crate::agent_definition_store::user_memory_dir().unwrap();
        assert_eq!(MemoryScope::for_dir(&agent_dir), Some(agent()));
        assert_eq!(MemoryScope::for_dir(&user_dir), Some(MemoryScope::User));
        assert_eq!(MemoryScope::for_dir(Path::new("/tmp/writer/memory")), None);
        assert_eq!(agent().key(), "agent:writer");
    }

    #[test]
    fn launch_requests_only_reach_this_launchs_layers() {
        let db = tempfile::tempdir().unwrap();
        let store = temp_store(db.path());
        let agent_dir = crate::agent_definition_store::agent_memory_dir("writer").unwrap();
        let user_dir = crate::agent_definition_store::user_memory_dir().unwrap();

        crate::agent_definition_store::create_agent_definition_on(
            &store,
            crate::agent_definition_store::AgentDefinitionCreate {
                id: Some("writer".into()),
                name: "写作".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let agent_scope = launch_scope(Some(&agent_dir), false).unwrap();
        let user_scope = launch_scope(Some(&user_dir), true).unwrap();
        record_launch_read(&store, &agent_scope, "fact.md", 5).unwrap();
        record_launch_write(
            &store,
            &user_scope,
            "me.md",
            crate::control_api::AgentMemoryTrust::Pending,
            "外部",
            "acp-1",
            5,
        )
        .unwrap();
        let meta = launch_memory_meta(&store, Some(&agent_dir), Some(&user_dir)).unwrap();
        assert!(meta.enabled);
        assert_eq!(meta.embedding, None, "没配向量检索就不下发");
        crate::memory_settings::apply_on(
            &store,
            crate::memory_settings::EMBEDDING_BASE_URL,
            &serde_json::Value::from("http://localhost:11434/v1"),
        )
        .unwrap();
        crate::memory_settings::apply_on(
            &store,
            crate::memory_settings::EMBEDDING_MODEL,
            &serde_json::Value::from("bge-m3"),
        )
        .unwrap();
        let embedding = launch_memory_meta(&store, Some(&agent_dir), Some(&user_dir))
            .unwrap()
            .embedding
            .unwrap();
        assert_eq!(embedding.model, "bge-m3");
        assert_eq!(meta.agent.usage[0].file, "fact.md");
        assert_eq!(meta.agent.usage[0].read_count, 1);
        assert_eq!(meta.user.pending, ["me.md"]);

        // 层和目录对不上、或会话没有这层，都不行；普通会话只拿到空元数据。
        assert_eq!(
            launch_scope(Some(&agent_dir), true),
            Err(MemoryRequestError::NoSuchLayer)
        );
        assert_eq!(
            launch_scope(None, false),
            Err(MemoryRequestError::NoSuchLayer)
        );
        assert_eq!(
            launch_scope(Some(Path::new("/tmp/writer/memory")), false),
            Err(MemoryRequestError::NoSuchLayer)
        );
        assert_eq!(
            record_launch_read(&store, &agent_scope, "../x.md", 5),
            Err(MemoryRequestError::InvalidFile)
        );
        assert_eq!(
            launch_memory_meta(&store, None, None).unwrap(),
            crate::control_api::AgentMemoryMetaResult::default()
        );

        // 会话启动后用户关掉记忆：之后拿到的是"已关闭"，不再给任何元数据。
        crate::agent_definition_store::update_agent_definition_on(
            &store,
            "writer",
            crate::agent_definition_store::AgentDefinitionPatch {
                memory_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!launch_memory_enabled(&store, Some(&agent_dir)));
        assert_eq!(
            launch_memory_meta(&store, Some(&agent_dir), Some(&user_dir)).unwrap(),
            crate::control_api::AgentMemoryMetaResult::default()
        );
    }
}
