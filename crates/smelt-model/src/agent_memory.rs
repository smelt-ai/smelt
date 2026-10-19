//! 智能体记忆的列表数据。读写记忆文件在运行时。

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// 事实类记忆这么久既没被读过也没更新过，就在界面上标成"长期未用"，由用户决定删不删。
/// 规则类（user / feedback）是用户给的约束，不按使用情况淘汰。
pub const MEMORY_STALE_AFTER: Duration = Duration::from_secs(60 * 24 * 60 * 60);

/// 与 `memory-files.ts` 的 `CORE_MEMORY_MAX_BYTES` 一致：每层 user / feedback 规则正文的总预算。
pub const CORE_MEMORY_MAX_BYTES: usize = 3_000;

const CORE_MEMORY_TYPES: [&str; 2] = ["user", "feedback"];

/// 规则类记忆（用户本人的情况、用户纠正过的做法）：常驻注入、占规则预算、不按使用情况淘汰。
/// 其余类型是按需召回的事实。
pub fn is_rule_memory_kind(kind: Option<&str>) -> bool {
    kind.is_some_and(|kind| CORE_MEMORY_TYPES.contains(&kind))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// 待确认的原因：读过外部内容的那一轮或无人值守的运行写的。待确认的记忆不注入、不召回，
    /// 用户采纳（[`approve_memory`]）后才生效。
    pub pending_reason: Option<String>,
    /// 去掉 frontmatter 的正文，卡片展开时看；frontmatter 坏了就是整个文件。
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryListing {
    pub entries: Vec<MemoryEntry>,
    /// 已生效的规则正文超出常驻预算（通常是手改的），启动时只会注入一部分。
    pub core_over_budget: bool,
}

/// 用户采纳一条待确认的记忆：之后照常注入和召回。
/// 设置页看的两层记忆：智能体自己的，和所有智能体共享的用户全局层。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentMemories {
    pub agent: MemoryListing,
    pub user: MemoryListing,
}
