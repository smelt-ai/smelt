//! ACP 对话消息流的共享数据模型：GPUI（`crates/smelt-acp-view`）与未来
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
        /// 嵌套 transcript：子代理的正文/工具挂在发起它的 Task/Agent 调用下。
        /// 旧快照没有这个字段，缺省为空。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        children: Vec<AcpEntry>,
    },
    /// 「重新开始」在旧对话和新对话之间插的分割线（不清空历史，只做标记）。
    Divider(String),
}

/// Agent 尚未通过通用 ACP 上报标题时，从首条用户消息生成稳定兜底。桌面侧栏、
/// 守护状态广播和移动端列表共用这一份规则，避免不同端显示成不同名字。
pub fn auto_title(entries: &[AcpEntry]) -> Option<String> {
    let prompt = entries.iter().find_map(|entry| match entry {
        AcpEntry::User(text) if !text.trim().is_empty() => Some(text.trim()),
        AcpEntry::UserWithImages { text, .. } if !text.trim().is_empty() => Some(text.trim()),
        _ => None,
    })?;
    crate::session_title::prompt_title(prompt)
}

/// ACP/app-server 图片的传输与热升级快照表示。长期历史仍以 agent
/// 自己的 transcript 为准，Smelt 不把图片重复写入工作区快照。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpImage {
    pub mime: String,
    pub data_b64: String,
}

/// 工具调用的一段输出：纯文本，文件 diff，或嵌入的 ACP 终端。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ToolOutputPart {
    Text(String),
    Diff {
        path: String,
        /// 新文件没有旧内容。
        old_text: Option<String>,
        new_text: String,
    },
    /// 工具返回的图片（`read` 读图、截图类工具）。渲染层直接出缩略图，
    /// 不再降级成 `[图片]` 文本——图片本来就是结果本身。
    Image(AcpImage),
    /// `terminal/create` 之后嵌进 tool_call 的 live 输出。旧快照没有这个变体。
    Terminal {
        id: String,
        #[serde(default)]
        output: String,
        #[serde(default)]
        truncated: bool,
        exit_code: Option<i32>,
        signal: Option<String>,
    },
}

impl AcpEntry {
    pub fn tool_call(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: ToolKind,
        status: ToolCallStatus,
        output: Vec<ToolOutputPart>,
    ) -> Self {
        Self::ToolCall {
            id: id.into(),
            title: title.into(),
            kind,
            status,
            output,
            children: Vec::new(),
        }
    }
}

/// 条目树里是否包含指定 id 的工具调用（含嵌套子代理）。
pub fn contains_tool_call(entries: &[AcpEntry], tool_id: &str) -> bool {
    entries.iter().any(|entry| match entry {
        AcpEntry::ToolCall { id, children, .. } => {
            id == tool_id || contains_tool_call(children, tool_id)
        }
        _ => false,
    })
}

/// 返回包含该工具（自身或子孙）的顶层下标，给增量广播用。
pub fn find_tool_top_level_index(entries: &[AcpEntry], tool_id: &str) -> Option<usize> {
    entries.iter().position(|entry| match entry {
        AcpEntry::ToolCall { id, children, .. } => {
            id == tool_id || contains_tool_call(children, tool_id)
        }
        _ => false,
    })
}

/// 在条目树里可变地找到指定工具。
pub fn find_tool_call_mut<'a>(
    entries: &'a mut [AcpEntry],
    tool_id: &str,
) -> Option<&'a mut AcpEntry> {
    for entry in entries {
        match entry {
            AcpEntry::ToolCall { id, .. } if id == tool_id => return Some(entry),
            AcpEntry::ToolCall { children, .. } if contains_tool_call(children, tool_id) => {
                return find_tool_call_mut(children, tool_id);
            }
            _ => {}
        }
    }
    None
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

/// 「从第 `through_index` 条回答分叉」的切点：其后第一条真实用户消息（跳过
/// 中断标记）。返回 `(原文, 同文本序号)`，供 agent 的 fork 列表定位——与
/// daemon 回退 `Rewind` 的 (text, occurrence) 同语义，切完新会话恰好包含到
/// 该回答为止。其后没有用户消息（点的已是最后一回合）→ `None`：整份拷贝
/// 本身就是所需历史。
pub fn fork_cut_after(entries: &[AcpEntry], through_index: usize) -> Option<(String, usize)> {
    let (next_index, next) = entries
        .iter()
        .enumerate()
        .skip(through_index + 1)
        .find(|(_, entry)| user_entry_text(entry).is_some_and(|text| !is_interrupt_marker(text)))?;
    let text = user_entry_text(next)?.to_string();
    // 同文本序号与 daemon `send_acp_rewind` 同一规则：只数同文本的用户
    // 消息，中断标记文本不会与真实用户消息相同，不单独排除。
    let occurrence = entries
        .iter()
        .take(next_index)
        .filter(|entry| user_entry_text(entry) == Some(text.as_str()))
        .count();
    Some((text, occurrence))
}

/// 「从指定用户消息编辑重发」的切点：分叉到该条消息之前，并返回原文和它在
/// 同文本用户消息中的序号，供 Pi `fork` 定位。中断标记、非用户条目和越界索引不是切点。
pub fn fork_cut_before(entries: &[AcpEntry], user_index: usize) -> Option<(String, usize)> {
    let text = user_entry_text(entries.get(user_index)?)?;
    if is_interrupt_marker(text) {
        return None;
    }
    let occurrence = entries
        .iter()
        .take(user_index)
        .filter(|entry| user_entry_text(entry) == Some(text))
        .count();
    Some((text.to_string(), occurrence))
}

fn user_entry_text(entry: &AcpEntry) -> Option<&str> {
    match entry {
        AcpEntry::User(text) => Some(text),
        AcpEntry::UserWithImages { text, .. } => Some(text),
        _ => None,
    }
}

/// Some ACP adapters report the final agent summary through a synthetic
/// `task_complete` tool call. It is a completion signal, not an action the
/// user needs to inspect as a tool.
pub fn is_task_completion_tool_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case("task_complete")
}

/// 工具名本身，不是「在干什么」。类型由 Bot 图标表达，这类标题不应再铺一遍。
pub fn is_generic_subagent_title(title: &str) -> bool {
    let title = title.trim();
    title.is_empty()
        || title.eq_ignore_ascii_case("subagent")
        || title.eq_ignore_ascii_case("task")
        || title.eq_ignore_ascii_case("tool")
        || title == "子代理"
        || title == "协作代理"
}

/// 子代理卡片标题：展示工作内容，而不是工具名。
///
/// 优先 `label` / 短描述 / 任务首行，再退到预设名或并行/串联摘要。已经是具体标题的
/// （如 Codex 的 `Start subagent explorer`）只剥启动前缀，不拿参数覆盖。
/// 并行/串联组只保留计数（「4 个并行」），名单留给卡片里的子行，避免标题把孩子焊一遍。
pub fn subagent_display_title(title: &str, args: Option<&serde_json::Value>) -> String {
    let stripped = strip_subagent_prefix(title);
    let derived = args.and_then(subagent_title_from_args);
    if is_generic_subagent_title(stripped) {
        derived.unwrap_or_else(|| stripped.to_string())
    } else {
        stripped.to_string()
    }
}

/// 去掉工具名之后，还值得写在卡片上的标题。泛称返回空串。
pub fn subagent_visible_title(title: &str) -> String {
    let title = subagent_display_title(title, None);
    if is_generic_subagent_title(&title) {
        String::new()
    } else {
        title
    }
}

/// 直接孩子里至少两个协作代理：这是一份名单，不是一条工具轨迹。
pub fn is_subagent_roster(children: &[AcpEntry]) -> bool {
    roster_children(children).count() >= 2
}

/// 卡片主标题。组头用计数，不用把孩子名字用间隔符焊在一起；旧快照里已经焊好的标题
/// 在展示时拆掉。单独一条子代理仍用它自己的工作名。
pub fn subagent_card_title(title: &str, children: &[AcpEntry]) -> String {
    let headline = subagent_visible_title(title);
    let titles = roster_titles(children);
    if titles.len() >= 2 {
        if headline.is_empty() || title_duplicates_roster(&headline, &titles) {
            return format!("{} 个子代理", titles.len());
        }
        return headline;
    }
    if headline.is_empty() && titles.len() == 1 {
        return titles[0].clone();
    }
    headline
}

/// 组头右侧的进度。并行名单跑着时是 `2/4`；结束后不写分数（留给完成勾）。
/// 单代理轨迹结束后才用「N 步」提示工作量。
pub fn subagent_progress_text(status: ToolCallStatus, children: &[AcpEntry]) -> String {
    let roster: Vec<&AcpEntry> = roster_children(children).collect();
    let running = matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress)
        || has_unfinished_tool_call(children);
    if roster.len() >= 2 {
        let failed = roster
            .iter()
            .copied()
            .filter(|entry| agent_failed(entry))
            .count();
        if !running {
            return if failed > 0 {
                format!("{failed} 个失败")
            } else {
                String::new()
            };
        }
        let done = roster
            .iter()
            .copied()
            .filter(|entry| agent_settled(entry))
            .count();
        return format!("{done}/{}", roster.len());
    }
    if running {
        return String::new();
    }
    let steps = children
        .iter()
        .filter(|child| matches!(child, AcpEntry::ToolCall { .. }))
        .count();
    if steps > 0 {
        format!("{steps} 步")
    } else {
        String::new()
    }
}

/// 遍历条目树里所有工具 id（含嵌套子代理），给展开态裁剪用。
pub fn for_each_tool_id<'a>(entries: &'a [AcpEntry], mut f: impl FnMut(&'a str)) {
    fn walk<'a>(entries: &'a [AcpEntry], f: &mut impl FnMut(&'a str)) {
        for entry in entries {
            if let AcpEntry::ToolCall { id, children, .. } = entry {
                f(id);
                walk(children, f);
            }
        }
    }
    walk(entries, &mut f);
}

pub fn subagent_title_from_args(args: &serde_json::Value) -> Option<String> {
    if let Some(label) = json_string(args, &["label"]) {
        return Some(first_nonempty_line(label).to_string());
    }
    if let Some(description) = json_string(args, &["description"]) {
        return Some(first_nonempty_line(description).to_string());
    }
    if let Some(task) = json_string(args, &["task", "prompt"]) {
        return Some(first_nonempty_line(task).to_string());
    }
    if let Some(agent) = json_string(
        args,
        &[
            "agent",
            "subagent_type",
            "agent_type",
            "subagentType",
            "agentType",
        ],
    ) {
        return Some(agent.to_string());
    }
    grouped_subagent_title(args, "tasks", "并行")
        .or_else(|| grouped_subagent_title(args, "chain", "串联"))
}

fn strip_subagent_prefix(title: &str) -> &str {
    let title = title.trim();
    for prefix in [
        "Start subagent ",
        "Start Subagent ",
        "subagent ",
        "Subagent ",
    ] {
        if let Some(rest) = title.strip_prefix(prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    title
}

fn grouped_subagent_title(args: &serde_json::Value, key: &str, kind: &str) -> Option<String> {
    let items = args
        .get(key)?
        .as_array()
        .filter(|items| !items.is_empty())?;
    if items.len() == 1 {
        return subagent_title_from_args(&items[0]).or_else(|| Some(kind.to_string()));
    }
    Some(format!("{} 个{}", items.len(), kind))
}

fn roster_children(children: &[AcpEntry]) -> impl Iterator<Item = &AcpEntry> {
    children.iter().filter(|child| {
        matches!(
            child,
            AcpEntry::ToolCall {
                kind: ToolKind::Collaborate,
                ..
            }
        )
    })
}

fn roster_titles(children: &[AcpEntry]) -> Vec<String> {
    roster_children(children)
        .filter_map(|child| match child {
            AcpEntry::ToolCall { title, .. } => {
                let visible = subagent_visible_title(title);
                Some(if visible.is_empty() {
                    title.clone()
                } else {
                    visible
                })
            }
            _ => None,
        })
        .collect()
}

fn title_duplicates_roster(title: &str, roster: &[String]) -> bool {
    if roster.len() < 2 {
        return false;
    }
    for sep in [" · ", "、", ", ", " / "] {
        let parts: Vec<&str> = title
            .split(sep)
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if parts.len() == roster.len() && parts.iter().zip(roster).all(|(part, name)| *part == name)
        {
            return true;
        }
    }
    false
}

fn agent_failed(entry: &AcpEntry) -> bool {
    matches!(
        entry,
        AcpEntry::ToolCall {
            status: ToolCallStatus::Failed,
            ..
        }
    )
}

fn agent_settled(entry: &AcpEntry) -> bool {
    match entry {
        AcpEntry::ToolCall {
            status: ToolCallStatus::Completed | ToolCallStatus::Failed,
            children,
            ..
        } => !has_unfinished_tool_call(children),
        _ => false,
    }
}

fn json_string<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_nonempty_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| text.trim())
}

/// 判断消息流里是否还留有未结束的工具调用。
///
/// ACP 的 `StopReason` 与工具更新来自同一条异步事件流，但部分 adapter 可能先
/// 交付回合结束、再交付最后一条工具状态。调用方不能只看 `DaemonPhase::Idle`，否则
/// 会把仍处于 Pending/InProgress 的工具误显示成空闲，并可能提前发送下一条 prompt。
pub fn has_unfinished_tool_call(entries: &[AcpEntry]) -> bool {
    entries.iter().any(|entry| match entry {
        AcpEntry::ToolCall {
            status: ToolCallStatus::Pending | ToolCallStatus::InProgress,
            ..
        } => true,
        AcpEntry::ToolCall { children, .. } => has_unfinished_tool_call(children),
        _ => false,
    })
}

/// 协作代理仍在跑：自身未结束，或挂在它下面的嵌套工具还没收尾。
pub fn is_active_agent_tool(kind: ToolKind, status: ToolCallStatus, children: &[AcpEntry]) -> bool {
    kind == ToolKind::Collaborate
        && (matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress)
            || has_unfinished_tool_call(children))
}

/// 完成信号里那段给人看的总结（diff 不算总结，跳过）。渲染层和会话交接
/// （`session_handoff`）都要读它，放这里避免两处各写一份切分规则。
pub fn completion_summary_text(output: &[ToolOutputPart]) -> String {
    output
        .iter()
        .filter_map(|part| match part {
            ToolOutputPart::Text(text) => {
                let text = strip_code_fence(text).trim();
                (!text.is_empty()).then(|| text.to_string())
            }
            ToolOutputPart::Diff { .. }
            | ToolOutputPart::Image(_)
            | ToolOutputPart::Terminal { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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

    fn user(text: &str) -> AcpEntry {
        AcpEntry::User(text.to_string())
    }

    fn assistant() -> AcpEntry {
        AcpEntry::Assistant {
            text: "回答".to_string(),
            thought: false,
        }
    }

    #[test]
    fn fork_cut_picks_next_user_message_after_answer() {
        // 点击第 1 条回答：切点是其后第一条用户消息，不含它
        let entries = vec![user("一"), assistant(), user("二"), assistant(), user("三")];
        assert_eq!(fork_cut_after(&entries, 0), Some(("二".to_string(), 0)));
        // 点击第 2 条回答：切到末条用户消息之前
        assert_eq!(fork_cut_after(&entries, 2), Some(("三".to_string(), 0)));
    }

    #[test]
    fn fork_cut_returns_none_when_answer_is_last_turn() {
        // 最后一回合之后没有用户消息：整拷即所需，无需切点
        let entries = vec![user("一"), assistant(), user("二"), assistant()];
        assert_eq!(fork_cut_after(&entries, 2), None);
        // 越界/空投影同样返回 None
        assert_eq!(fork_cut_after(&entries, 99), None);
        assert_eq!(fork_cut_after(&[], 0), None);
    }

    #[test]
    fn fork_cut_skips_interrupt_markers() {
        // 中断标记不是真实用户消息：不能当切点，也不计入 occurrence
        let entries = vec![
            user("一"),
            assistant(),
            AcpEntry::User("[Request interrupted by user]".to_string()),
            user("二"),
        ];
        assert_eq!(fork_cut_after(&entries, 0), Some(("二".to_string(), 0)));
    }

    #[test]
    fn fork_cut_counts_occurrence_of_duplicate_texts() {
        // 同文本消息靠序号区分：切点是第 2 条「再来」时 occurrence = 1
        let entries = vec![
            user("再来"),
            assistant(),
            user("其他"),
            assistant(),
            user("再来"),
            assistant(),
            user("末尾"),
        ];
        assert_eq!(fork_cut_after(&entries, 5), Some(("末尾".to_string(), 0)));
        assert_eq!(fork_cut_after(&entries, 3), Some(("再来".to_string(), 1)));
    }

    #[test]
    fn fork_cut_before_user_message_counts_identical_prompts() {
        let entries = vec![
            user("same"),
            assistant(),
            user("other"),
            assistant(),
            user("same"),
        ];

        assert_eq!(fork_cut_before(&entries, 2), Some(("other".to_string(), 0)));
        assert_eq!(fork_cut_before(&entries, 4), Some(("same".to_string(), 1)));
        assert_eq!(fork_cut_before(&entries, 1), None);
        assert_eq!(fork_cut_before(&entries, 99), None);
    }

    #[test]
    fn fork_cut_reads_text_of_image_messages() {
        // 带图用户消息同样可作切点，文本参与匹配
        let entries = vec![
            user("一"),
            assistant(),
            AcpEntry::UserWithImages {
                text: "带图的二".to_string(),
                images: Vec::new(),
            },
        ];
        assert_eq!(
            fork_cut_after(&entries, 0),
            Some(("带图的二".to_string(), 0))
        );
    }

    #[test]
    fn detects_task_completion_tool_title() {
        assert!(is_task_completion_tool_title("task_complete"));
        assert!(is_task_completion_tool_title(" TASK_COMPLETE "));
        assert!(!is_task_completion_tool_title("task_status"));
    }

    #[test]
    fn detects_unfinished_tool_calls() {
        let pending = AcpEntry::tool_call(
            "pending",
            "read",
            ToolKind::Read,
            ToolCallStatus::Pending,
            Vec::new(),
        );
        let completed = AcpEntry::tool_call(
            "completed",
            "read",
            ToolKind::Read,
            ToolCallStatus::Completed,
            Vec::new(),
        );

        assert!(has_unfinished_tool_call(&[pending]));
        assert!(!has_unfinished_tool_call(&[completed]));
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
                children,
            } => {
                assert_eq!(id, "call-1");
                assert_eq!(title, "Read foo.rs");
                assert_eq!(kind, ToolKind::Read);
                assert_eq!(status, ToolCallStatus::Completed);
                assert!(children.is_empty());
                assert_eq!(output.len(), 2);
            }
            _ => panic!("应当反序列化成 ToolCall"),
        }
    }

    #[test]
    fn nested_unfinished_tool_keeps_parent_visibly_running() {
        let nested = AcpEntry::tool_call(
            "child",
            "Read",
            ToolKind::Read,
            ToolCallStatus::InProgress,
            Vec::new(),
        );
        let mut parent = AcpEntry::tool_call(
            "agent",
            "Task",
            ToolKind::Collaborate,
            ToolCallStatus::Completed,
            Vec::new(),
        );
        if let AcpEntry::ToolCall { children, .. } = &mut parent {
            children.push(nested);
        }
        let AcpEntry::ToolCall {
            kind,
            status,
            children,
            ..
        } = &parent
        else {
            panic!("parent should be a tool call");
        };
        assert!(has_unfinished_tool_call(std::slice::from_ref(&parent)));
        assert!(is_active_agent_tool(*kind, *status, children));
    }

    #[test]
    fn empty_children_are_omitted_from_json() {
        let entry = AcpEntry::tool_call(
            "call-1",
            "Read",
            ToolKind::Read,
            ToolCallStatus::Completed,
            Vec::new(),
        );
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("children"));
    }

    #[test]
    fn subagent_display_title_prefers_job_over_tool_name() {
        assert_eq!(
            subagent_display_title("subagent", Some(&serde_json::json!({"label": "查登录"}))),
            "查登录"
        );
        assert_eq!(
            subagent_display_title(
                "subagent",
                Some(&serde_json::json!({"agent": "scout", "task": "find auth"}))
            ),
            "find auth"
        );
        assert_eq!(
            subagent_display_title(
                "subagent",
                Some(&serde_json::json!({"task": "find auth\nmore detail"}))
            ),
            "find auth"
        );
        assert_eq!(
            subagent_display_title("subagent", Some(&serde_json::json!({"agent": "scout"}))),
            "scout"
        );
        assert_eq!(
            subagent_display_title(
                "Task",
                Some(&serde_json::json!({
                    "description": "Inspect the ACP pipeline",
                    "prompt": "Find where updates are dropped"
                }))
            ),
            "Inspect the ACP pipeline"
        );
        assert_eq!(
            subagent_display_title("Start subagent explorer", None),
            "explorer"
        );
        assert_eq!(
            subagent_display_title(
                "subagent",
                Some(&serde_json::json!({"tasks": [{"label": "UI"}, {"label": "RPC"}]}))
            ),
            "2 个并行"
        );
        assert_eq!(
            subagent_display_title(
                "subagent",
                Some(&serde_json::json!({"tasks": [{"label": "UI"}]}))
            ),
            "UI"
        );
        assert_eq!(
            subagent_display_title("subagent", Some(&serde_json::json!({"chain": [{}, {}]}))),
            "2 个串联"
        );
        assert_eq!(subagent_display_title("subagent", None), "subagent");
        assert!(is_generic_subagent_title("Task"));
        assert!(!is_generic_subagent_title("explorer"));
    }

    fn collab(id: &str, title: &str, status: ToolCallStatus) -> AcpEntry {
        AcpEntry::tool_call(id, title, ToolKind::Collaborate, status, Vec::new())
    }

    #[test]
    fn subagent_card_title_does_not_weld_roster_names() {
        let children = vec![
            collab("a", "smeltd 遗留审查", ToolCallStatus::InProgress),
            collab("b", "核心存储审查", ToolCallStatus::InProgress),
            collab("c", "GUI 遗留审查", ToolCallStatus::InProgress),
            collab("d", "依赖与全局结构审查", ToolCallStatus::InProgress),
        ];
        let welded = "smeltd 遗留审查 · 核心存储审查 · GUI 遗留审查 · 依赖与全局结构审查";
        assert!(is_subagent_roster(&children));
        assert_eq!(subagent_card_title(welded, &children), "4 个子代理");
        assert_eq!(subagent_card_title("4 个并行", &children), "4 个并行");
        assert_eq!(subagent_card_title("全面审查", &children), "全面审查");
        assert_eq!(subagent_card_title("subagent", &children), "4 个子代理");
        assert_eq!(
            subagent_progress_text(ToolCallStatus::InProgress, &children),
            "0/4"
        );
    }

    #[test]
    fn subagent_roster_progress_counts_settled_children() {
        let children = vec![
            collab("a", "UI", ToolCallStatus::Completed),
            collab("b", "RPC", ToolCallStatus::Completed),
            collab("c", "GUI", ToolCallStatus::InProgress),
            collab("d", "存储", ToolCallStatus::Pending),
        ];
        assert_eq!(
            subagent_progress_text(ToolCallStatus::InProgress, &children),
            "2/4"
        );

        let done = vec![
            collab("a", "UI", ToolCallStatus::Completed),
            collab("b", "RPC", ToolCallStatus::Completed),
        ];
        assert_eq!(subagent_progress_text(ToolCallStatus::Completed, &done), "");

        let failed = vec![
            collab("a", "UI", ToolCallStatus::Completed),
            collab("b", "RPC", ToolCallStatus::Failed),
        ];
        assert_eq!(
            subagent_progress_text(ToolCallStatus::Completed, &failed),
            "1 个失败"
        );
    }

    #[test]
    fn subagent_single_agent_keeps_job_title_and_step_count() {
        let mut parent = collab("agent", "Task", ToolCallStatus::Completed);
        let reads = vec![
            AcpEntry::tool_call(
                "r1",
                "README",
                ToolKind::Read,
                ToolCallStatus::Completed,
                Vec::new(),
            ),
            AcpEntry::tool_call(
                "r2",
                "lib.rs",
                ToolKind::Read,
                ToolCallStatus::Completed,
                Vec::new(),
            ),
        ];
        if let AcpEntry::ToolCall { children, .. } = &mut parent {
            *children = reads;
        }
        assert!(!is_subagent_roster(parent_children(&parent)));
        assert_eq!(subagent_card_title("Task", parent_children(&parent)), "");
        assert_eq!(
            subagent_card_title("查登录", parent_children(&parent)),
            "查登录"
        );
        assert_eq!(
            subagent_progress_text(ToolCallStatus::Completed, parent_children(&parent)),
            "2 步"
        );
        assert_eq!(
            subagent_progress_text(ToolCallStatus::InProgress, parent_children(&parent)),
            ""
        );
    }

    #[test]
    fn subagent_generic_parent_adopts_the_only_child_name() {
        let children = vec![collab("a", "查登录", ToolCallStatus::InProgress)];
        assert_eq!(subagent_card_title("subagent", &children), "查登录");
        assert!(!is_subagent_roster(&children));
    }

    fn parent_children(entry: &AcpEntry) -> &[AcpEntry] {
        match entry {
            AcpEntry::ToolCall { children, .. } => children,
            _ => panic!("expected tool call"),
        }
    }
}
