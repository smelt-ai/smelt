//! 核心数据模型：Instinct 与 Scope。

use serde::{Deserialize, Serialize};

/// instinct 的作用域：全局或项目级。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// 全局 instinct，写入 ~/.smelt/global.md
    Global,
    /// 项目级 instinct，按 git remote 区分
    Project,
}

impl Scope {
    /// 转为存入 DB 的字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }

    /// 从 DB 字符串还原。
    pub fn from_db(s: &str) -> Self {
        match s {
            "project" => Scope::Project,
            _ => Scope::Global,
        }
    }
}

/// 一条提炼出的编码直觉。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instinct {
    /// 唯一标识（内容哈希）
    pub id: String,
    /// instinct 正文
    pub content: String,
    /// 置信度，范围 0.3-0.9
    pub confidence: f32,
    /// 所属领域，如 ["rust", "git"]
    pub domain: Vec<String>,
    /// 支撑证据出现次数
    pub evidence_count: u32,
    /// 最近一次观察到的时间（RFC3339 字符串）
    pub last_seen: String,
    /// 作用域
    pub scope: Scope,
    /// 所属项目（None=全局；Some=仓库名，配合 Scope::Project）
    pub project: Option<String>,
}
