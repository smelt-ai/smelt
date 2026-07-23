//! ACP 对话消息流的共享数据模型：GPUI（`crates/smelt/src/acp_view.rs`）与未来
//! web/mobile 渲染器共用同一份结构，不依赖 `agent-client-protocol` crate 本身——
//! 协议 schema 怎么演进不该牵连渲染层，这份结构还要能被非 Rust 客户端直接当
//! JSON 消费。枚举 tag 对齐 agent-client-protocol 1.x 的 snake_case 线格式。
//!
//! 协议类型 → 这份类型的转换函数就近放在调用方（那边本来就依赖
//! agent-client-protocol，这个 crate 不许依赖，也不许引 GPUI）。

use serde::{Deserialize, Serialize};

/// 消息流里的一条。落盘持久化，进程重启/会话「重新开始」都要保住历史。
#[derive(Clone, Serialize, Deserialize)]
pub enum AcpEntry {
    User(String),
    /// assistant 正文或思考块（thought 弱化显示）；连续 chunk 就地追加。
    Assistant { text: String, thought: bool },
    ToolCall {
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolCallStatus,
        /// 保留结构（不压扁成一行文本）——diff 要能逐行渲染红/绿，压扁了就
        /// 回不去了。
        output: Vec<ToolOutputPart>,
    },
    /// 「重新开始」在旧对话和新对话之间插的分割线（不清空历史，只做标记）。
    Divider(String),
}

/// 工具调用的一段输出：纯文本，或者一份文件 diff。
#[derive(Clone, Serialize, Deserialize)]
pub enum ToolOutputPart {
    Text(String),
    Diff {
        path: String,
        /// 新文件没有旧内容。
        old_text: Option<String>,
        new_text: String,
    },
}

/// 工具类别，跟 `agent-client-protocol::ToolKind` 的 wire 格式（snake_case）
/// 对齐，落盘数据能跨协议版本读。`Other` 兜底未来协议新增的分类，不会因为一个
/// 陌生 tag 就让整条记录反序列化失败。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    #[serde(other)]
    Other,
}

/// 工具调用状态，同上对齐 agent-client-protocol 的 wire 格式。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// diff 里的一行相对旧文本的属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineTag {
    Added,
    Removed,
    Context,
}

/// diff 逐行结果：GPUI 上色渲染、以后转发给 web 端都消费这份，不用各自再跑一遍
/// diff 算法——数字和实际渲染的行对不上比不显示还糟。
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: DiffLineTag,
    pub text: String,
}

/// 把新旧文本切成逐行 diff。
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|change| {
            let tag = match change.tag() {
                similar::ChangeTag::Insert => DiffLineTag::Added,
                similar::ChangeTag::Delete => DiffLineTag::Removed,
                similar::ChangeTag::Equal => DiffLineTag::Context,
            };
            DiffLine {
                tag,
                text: change.value().trim_end_matches('\n').to_string(),
            }
        })
        .collect()
}

/// 逐行 diff 的增删行数统计（"+N -M"）。基于 `diff_lines` 的同一份结果统计，
/// 保证头部摘要数字和下方逐行渲染永远一致。
pub fn diff_line_stats(old: &str, new: &str) -> (usize, usize) {
    let lines = diff_lines(old, new);
    let added = lines.iter().filter(|l| l.tag == DiffLineTag::Added).count();
    let removed = lines.iter().filter(|l| l.tag == DiffLineTag::Removed).count();
    (added, removed)
}

/// 剥掉整段被 markdown 围栏包住的工具输出（```lang\n…\n```）。只在「整段就是
/// 一个围栏块」时剥——正文里穿插的代码块交给 markdown 渲染器，别在这里瞎切。
pub fn strip_code_fence(text: &str) -> &str {
    let t = text.trim();
    let Some(rest) = t.strip_prefix("```") else { return text };
    // 跳过围栏后面的语言标注那一行
    let Some(nl) = rest.find('\n') else { return text };
    let Some(body) = rest[nl + 1..].strip_suffix("```") else { return text };
    body.trim_end_matches('\n')
}

/// agent 回显的「用户中断」标记——它走的是 UserMessageChunk 通道，但不是用户
/// 打的字，UI 得把它渲染成状态提示而不是消息气泡。
pub fn is_interrupt_marker(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[Request interrupted by user") && t.ends_with(']')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_whole_output_code_fence_only() {
        // adapter 把工具输出整段包在围栏里 → 剥掉，别把 ``` 显示给人看
        assert_eq!(strip_code_fence("```console\nhello\nworld\n```"), "hello\nworld");
        // 无语言标注同理
        assert_eq!(strip_code_fence("```\nplain\n```"), "plain");
        // 正文里穿插的代码块不属于「整段就是一个围栏」，原样返回交给 markdown
        let mixed = "前言\n```rs\nlet x = 1;\n```\n后记";
        assert_eq!(strip_code_fence(mixed), mixed);
        // 没有围栏的普通输出原样返回
        assert_eq!(strip_code_fence("exit 0"), "exit 0");
    }

    #[test]
    fn detects_interrupt_marker() {
        assert!(is_interrupt_marker("[Request interrupted by user]"));
        assert!(is_interrupt_marker("[Request interrupted by user for tool use]"));
        assert!(!is_interrupt_marker("请把这段中断逻辑说清楚"));
    }

    #[test]
    fn diff_stats_match_diff_lines() {
        let old = "a\nb\nc\n";
        let new = "a\nx\nc\n";
        let lines = diff_lines(old, new);
        let (added, removed) = diff_line_stats(old, new);
        assert_eq!(added, lines.iter().filter(|l| l.tag == DiffLineTag::Added).count());
        assert_eq!(removed, lines.iter().filter(|l| l.tag == DiffLineTag::Removed).count());
    }

    #[test]
    fn tool_kind_roundtrips_snake_case_json() {
        assert_eq!(serde_json::to_string(&ToolKind::SwitchMode).unwrap(), "\"switch_mode\"");
        assert_eq!(serde_json::to_string(&ToolCallStatus::InProgress).unwrap(), "\"in_progress\"");
        let unknown: ToolKind = serde_json::from_str("\"some_future_kind\"").unwrap();
        assert_eq!(unknown, ToolKind::Other);
    }

    /// 这份类型是从 `crates/smelt/src/acp_view.rs` 搬过来的（此前 id 字段是
    /// agent_client_protocol::ToolCallId，kind/status 是协议原始类型）。旧存档
    /// 里躺着这种 JSON——搬家不能让用户已经落盘的对话历史读不回来。
    #[test]
    fn deserializes_pre_migration_tool_call_json() {
        let old_json = r#"{"ToolCall":{"id":"call-1","title":"Read foo.rs","kind":"read","status":"completed","output":[{"Text":"ok"},{"Diff":{"path":"foo.rs","old_text":"a\n","new_text":"b\n"}}]}}"#;
        let entry: AcpEntry = serde_json::from_str(old_json).expect("旧存档 ToolCall 条目应能读入");
        match entry {
            AcpEntry::ToolCall { id, title, kind, status, output } => {
                assert_eq!(id, "call-1");
                assert_eq!(title, "Read foo.rs");
                assert_eq!(kind, ToolKind::Read);
                assert_eq!(status, ToolCallStatus::Completed);
                assert_eq!(output.len(), 2);
            }
            _ => panic!("应当反序列化成 ToolCall"),
        }
    }
}
