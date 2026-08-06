//! ACP 对话消息流的共享数据模型：GPUI（`crates/smelt/src/acp_view.rs`）与未来
//! web/mobile 渲染器共用同一份结构，不依赖 `agent-client-protocol` crate 本身——
//! 协议 schema 怎么演进不该牵连渲染层，这份结构还要能被非 Rust 客户端直接当
//! JSON 消费。枚举 tag 对齐 agent-client-protocol 1.x 的 snake_case 线格式。
//!
//! 协议类型 → 这份类型的转换函数就近放在调用方（那边本来就依赖
//! agent-client-protocol，这个 crate 不许依赖，也不许引 GPUI）。

use serde::{Deserialize, Serialize};

/// 消息流里的一条。由 agent 会话历史重放，也可随 smeltd 热升级快照交接。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AcpEntry {
    User(String),
    /// 带图片的用户消息。保留旧的 `User(String)` 变体，兼容已有快照。
    UserWithImages {
        text: String,
        images: Vec<AcpImage>,
    },
    /// assistant 正文或思考块（thought 弱化显示）；连续 chunk 就地追加。
    Assistant {
        text: String,
        thought: bool,
    },
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

/// 从首条用户消息生成稳定的会话标题。桌面侧栏、守护状态广播和移动端列表
/// 共用这一份规则，避免同一会话在不同端显示成不同名字。
pub fn auto_title(entries: &[AcpEntry]) -> Option<String> {
    let prompt = entries.iter().find_map(|entry| match entry {
        AcpEntry::User(text) if !text.trim().is_empty() => Some(text.trim()),
        AcpEntry::UserWithImages { text, .. } if !text.trim().is_empty() => Some(text.trim()),
        _ => None,
    })?;
    let single_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let title: String = chars.by_ref().take(36).collect();
    Some(if chars.next().is_some() {
        format!("{title}...")
    } else {
        title
    })
}

/// ACP/app-server 图片的传输与热升级快照表示。长期历史仍以 agent
/// 自己的 transcript 为准，Smelt 不把图片重复写入 workspace.json。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcpImage {
    pub mime: String,
    pub data_b64: String,
}

/// 工具调用的一段输出：纯文本，或者一份文件 diff。
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    Collaborate,
    Review,
    Image,
    Compact,
    Wait,
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
    let removed = lines
        .iter()
        .filter(|l| l.tag == DiffLineTag::Removed)
        .count();
    (added, removed)
}

/// 把完整逐行 diff 压成适合卡片预览的 unified diff：每个变更块只保留前后
/// `context` 行，长段未变化内容折成一条提示。这样大文件的小改动不会在 UI 中
/// 创建几千个不可见行元素。
pub fn compact_diff_lines(lines: &[DiffLine], context: usize) -> Vec<DiffLine> {
    if lines.is_empty() {
        return Vec::new();
    }
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(ix, line)| (line.tag != DiffLineTag::Context).then_some(ix))
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for ix in changed {
        let start = ix.saturating_sub(context);
        let end = (ix + context + 1).min(lines.len());
        if let Some((_, last_end)) = ranges.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
        } else {
            ranges.push((start, end));
        }
    }

    let mut out = Vec::new();
    let mut previous_end = 0;
    for (start, end) in ranges {
        if start > previous_end {
            out.push(DiffLine {
                tag: DiffLineTag::Context,
                text: format!("... 省略 {} 行未修改内容 ...", start - previous_end),
            });
        }
        out.extend(lines[start..end].iter().cloned());
        previous_end = end;
    }
    if previous_end < lines.len() {
        out.push(DiffLine {
            tag: DiffLineTag::Context,
            text: format!("... 省略 {} 行未修改内容 ...", lines.len() - previous_end),
        });
    }
    out
}

/// 剥掉整段被 markdown 围栏包住的工具输出（```lang\n…\n```）。只在「整段就是
/// 一个围栏块」时剥——正文里穿插的代码块交给 markdown 渲染器，别在这里瞎切。
pub fn strip_code_fence(text: &str) -> &str {
    let t = text.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return text;
    };
    // 跳过围栏后面的语言标注那一行
    let Some(nl) = rest.find('\n') else {
        return text;
    };
    let Some(body) = rest[nl + 1..].strip_suffix("```") else {
        return text;
    };
    body.trim_end_matches('\n')
}

/// agent 回显的「用户中断」标记——它走的是 UserMessageChunk 通道，但不是用户
/// 打的字，UI 得把它渲染成状态提示而不是消息气泡。
pub fn is_interrupt_marker(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[Request interrupted by user") && t.ends_with(']')
}

/// Some ACP adapters report the final agent summary through a synthetic
/// `task_complete` tool call. It is a completion signal, not an action the
/// user needs to inspect as a tool.
pub fn is_task_completion_tool_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case("task_complete")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_whole_output_code_fence_only() {
        // adapter 把工具输出整段包在围栏里 → 剥掉，别把 ``` 显示给人看
        assert_eq!(
            strip_code_fence("```console\nhello\nworld\n```"),
            "hello\nworld"
        );
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
        assert!(is_interrupt_marker(
            "[Request interrupted by user for tool use]"
        ));
        assert!(!is_interrupt_marker("请把这段中断逻辑说清楚"));
    }

    #[test]
    fn detects_task_completion_tool_title() {
        assert!(is_task_completion_tool_title("task_complete"));
        assert!(is_task_completion_tool_title(" TASK_COMPLETE "));
        assert!(!is_task_completion_tool_title("task_status"));
    }

    #[test]
    fn user_images_roundtrip_without_breaking_legacy_entries() {
        let legacy: AcpEntry = serde_json::from_str(r#"{"User":"hello"}"#).unwrap();
        assert!(matches!(legacy, AcpEntry::User(text) if text == "hello"));

        let entry = AcpEntry::UserWithImages {
            text: "看这里".into(),
            images: vec![AcpImage {
                mime: "image/png".into(),
                data_b64: "QUJD".into(),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let restored: AcpEntry = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            restored,
            AcpEntry::UserWithImages { text, images }
                if text == "看这里"
                    && images.len() == 1
                    && images[0].mime == "image/png"
                    && images[0].data_b64 == "QUJD"
        ));
    }

    #[test]
    fn auto_title_uses_first_non_empty_user_message() {
        let entries = vec![
            AcpEntry::Assistant {
                text: "ignored".into(),
                thought: false,
            },
            AcpEntry::User("  第一行\n  第二行  ".into()),
            AcpEntry::User("later".into()),
        ];
        assert_eq!(auto_title(&entries).as_deref(), Some("第一行 第二行"));
    }

    #[test]
    fn auto_title_supports_images_and_truncates_by_character() {
        let text = "一".repeat(40);
        let entries = vec![AcpEntry::UserWithImages {
            text,
            images: vec![],
        }];
        assert_eq!(
            auto_title(&entries),
            Some(format!("{}...", "一".repeat(36)))
        );
    }

    #[test]
    fn diff_stats_match_diff_lines() {
        let old = "a\nb\nc\n";
        let new = "a\nx\nc\n";
        let lines = diff_lines(old, new);
        let (added, removed) = diff_line_stats(old, new);
        assert_eq!(
            added,
            lines.iter().filter(|l| l.tag == DiffLineTag::Added).count()
        );
        assert_eq!(
            removed,
            lines
                .iter()
                .filter(|l| l.tag == DiffLineTag::Removed)
                .count()
        );
    }

    #[test]
    fn compact_diff_omits_long_unchanged_regions() {
        let old = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
        let new = old.replace("line 50\n", "changed\n");
        let full = diff_lines(&old, &new);
        let compact = compact_diff_lines(&full, 3);

        assert!(compact.len() < 12);
        assert!(compact.iter().any(|line| line.text == "changed"));
        assert!(compact.iter().any(|line| line.text.contains("省略")));
        assert_eq!(
            compact
                .iter()
                .filter(|line| line.tag == DiffLineTag::Added)
                .count(),
            1
        );
    }

    #[test]
    fn tool_kind_roundtrips_snake_case_json() {
        assert_eq!(
            serde_json::to_string(&ToolKind::SwitchMode).unwrap(),
            "\"switch_mode\""
        );
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
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
            AcpEntry::ToolCall {
                id,
                title,
                kind,
                status,
                output,
            } => {
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
