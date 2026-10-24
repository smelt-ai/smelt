//! 历史会话的展示数据（列表概览、对话轮次）和把它转成交接上下文的规则。扫描、解析各家存档在运行时。

use crate::session_handoff::HandoffTurn;
use chrono::{DateTime, Utc};

/// 一份历史会话的概览（列表用）。
#[derive(Clone)]
pub struct SessionSummary {
    /// 运行时那台机器上的存档路径。跨过协议的是字符串，不按桌面所在系统解释。
    pub path: String,
    /// 实际展示标题：用户设置过名称时优先，否则使用 agent_title。
    pub title: String,
    /// Agent transcript / summary 提供的原始标题，用作搜索和自定义标题下的辅助信息。
    pub agent_title: String,
    pub custom_title: Option<String>,
    /// agent 那头认得的 session id——续接时要发给协议的就是这个，不是 `path`。
    /// 各家 agent 取法不一样：Claude 是文件名去扩展名，Codex 是 `session_meta.id`，
    /// Grok/Copilot 是会话目录名，`path` 本身的形状（文件 vs 目录）各家也不一样，
    /// 不能拿 `path` 现算，得在各自的 summarize 里就近取一份存下来。
    pub resume_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    /// user + assistant 消息总数（不含被跳过的 tool_result / 内部记录）。
    pub message_count: usize,
    /// 本份会话消耗的 token 总量（input+output+两种 cache 相加），供总览卡片展示
    /// 「当前会话」口径的用量。
    pub total_tokens: u64,
}

/// 一轮对话：用户发言 / Assistant 回复（含它这轮调用了哪些工具）。
pub struct Turn {
    pub is_user: bool,
    pub timestamp: Option<DateTime<Utc>>,
    pub text: String,
    /// 这轮里 assistant 调用的工具名（user 轮恒为空）。
    pub tools: Vec<String>,
    /// 这轮工具入参里出现的文件路径，去重后按出现顺序排列。会话迁移要靠它告诉
    /// 下一个 agent「上一段动过哪些文件」；历史页本身不显示。
    ///
    /// 各家 agent 给得起的程度不一样（都是实测）：Claude 的 `tool_use.input.file_path`、
    /// Grok 的 `target_file`/`path`/`target_directory`、Copilot 的 `path` 都是结构化
    /// 参数，直接取；**Codex 取不到**——它的工具是 `exec_command`，参数只有一整条
    /// shell 命令（`cmd`），从命令行里猜路径不如不猜，留空。
    pub tool_paths: Vec<String>,
}

impl Turn {
    /// 纯文本轮次（没有工具信息）的构造快捷方式，省得各家读取器各写一遍
    /// `tools: Vec::new(), tool_paths: Vec::new()`。
    pub fn message(is_user: bool, timestamp: Option<DateTime<Utc>>, text: String) -> Self {
        Self {
            is_user,
            timestamp,
            text,
            tools: Vec::new(),
            tool_paths: Vec::new(),
        }
    }
}

pub struct SessionDetail {
    pub turns: Vec<Turn>,
    /// 原会话用的模型（读得到才有）。迁移时写进交接头部，作为「上一段是谁干的」
    /// 的交代——不会拿它去设置目标 agent 的模型，那是各家私有取值。
    pub model: Option<String>,
}

/// 历史 transcript → 交接轮次（`crate::session_handoff` 的中立 IR）。这是当前唯一
/// 的文本交接来源，生成的轮次统一交给同一个摘要渲染器。
///
/// 各家 transcript 本来就没有工具的类别/状态（`detail` 恒为 `None`），图片也只会
/// 落盘引用或干脆不落（`images` 恒为 0）。
/// 一轮里的工具名列表 → 一行摘要，连续同名的折成 `名字 ×n`。
///
/// 实测一份 398 轮的 Codex 会话里，一轮连着调五次 `wait` 很常见，原样列出来是
/// 「wait、wait、wait、wait、wait」——占预算却不多给一点信息。
fn summarize_tool_names(names: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut count = 1usize;
    for (ix, name) in names.iter().enumerate() {
        if names.get(ix + 1) == Some(name) {
            count += 1;
            continue;
        }
        parts.push(if count > 1 {
            format!("{name} ×{count}")
        } else {
            name.clone()
        });
        count = 1;
    }
    parts.join("、")
}

pub fn handoff_turns_from_history(detail: &SessionDetail) -> Vec<HandoffTurn> {
    let mut turns = Vec::new();
    for turn in &detail.turns {
        if turn.is_user {
            if !turn.text.trim().is_empty() {
                turns.push(HandoffTurn::User {
                    text: turn.text.clone(),
                    images: 0,
                });
            }
            continue;
        }
        if !turn.text.trim().is_empty() {
            turns.push(HandoffTurn::Assistant(turn.text.clone()));
        }
        // 一轮里的工具合成一条：历史侧没有逐个工具的状态，拆开写只是把同样的
        // 信息摊得更长，白占 prompt 预算。
        if !turn.tools.is_empty() {
            turns.push(HandoffTurn::Tool {
                title: summarize_tool_names(&turn.tools),
                detail: None,
                paths: turn.tool_paths.clone(),
            });
        }
    }
    turns
}
