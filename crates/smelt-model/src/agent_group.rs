//! 本地智能体群组（PR 1）的共享类型与纯函数。
//!
//! 群组消息投递复用 `peer_messages` + `acp_host::deliver_peer_message`，每条 @ 投递使用确定性
//! `pm-grp-<message_id>-<target_member_id>` 投递 ID：既满足 `<peer-message>` 历史重放识别，
//! 也在审批放行、重启恢复与重试时保持幂等去重。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// 群组路由模式。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// 自由 @ 路由：只有被 `@alias` 或 `@all` 提及的成员会被唤醒。
    #[default]
    Mention,
    /// 协调者模式：用户发消息未显式 `@` 时默认路由给协调者成员。
    Coordinator,
}

impl GroupMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mention => "mention",
            Self::Coordinator => "coordinator",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "mention" => Some(Self::Mention),
            "coordinator" => Some(Self::Coordinator),
            _ => None,
        }
    }
}

/// 群组成员与底层会话的所有权关系。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MemberOwnership {
    /// 由群组新建并托管的会话：从主侧栏会话列表隐藏；暂停中止或删群时可一并处理。
    #[default]
    GroupOwned,
    /// 挂接已有会话：保留在主侧栏；暂停中止或删群绝不中断或删除它。
    Linked,
}

impl MemberOwnership {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GroupOwned => "group_owned",
            Self::Linked => "linked",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "group_owned" => Some(Self::GroupOwned),
            "linked" => Some(Self::Linked),
            _ => None,
        }
    }
}

/// 成员角色种类。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MemberRoleKind {
    Coordinator,
    #[default]
    Worker,
    Reviewer,
    Observer,
}

impl MemberRoleKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Observer => "observer",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "coordinator" => Some(Self::Coordinator),
            "worker" => Some(Self::Worker),
            "reviewer" => Some(Self::Reviewer),
            "observer" => Some(Self::Observer),
            _ => None,
        }
    }
}

/// 群组消息发送方类别。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupMessageSenderType {
    #[default]
    User,
    Member,
    System,
}

impl GroupMessageSenderType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Member => "member",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "user" => Some(Self::User),
            "member" => Some(Self::Member),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// 群组消息种类。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupMessageKind {
    #[default]
    Chat,
    Task,
    Decision,
    System,
}

impl GroupMessageKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Task => "task",
            Self::Decision => "decision",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "chat" => Some(Self::Chat),
            "task" => Some(Self::Task),
            "decision" => Some(Self::Decision),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// 任务类消息的状态。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupTaskStatus {
    #[default]
    Open,
    InProgress,
    Done,
    Blocked,
}

impl GroupTaskStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "open" => Some(Self::Open),
            "in_progress" => Some(Self::InProgress),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// `@` 提及种类。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupMentionKind {
    #[default]
    Direct,
    All,
    CoordinatorDefault,
}

impl GroupMentionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::All => "all",
            Self::CoordinatorDefault => "coordinator_default",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "direct" => Some(Self::Direct),
            "all" => Some(Self::All),
            "coordinator_default" => Some(Self::CoordinatorDefault),
            _ => None,
        }
    }
}

/// 单条 `@` 投递的状态。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GroupDeliveryStatus {
    #[default]
    Queued,
    PendingApproval,
    Delivered,
    Coalesced,
    Skipped,
    Rejected,
    Failed,
}

impl GroupDeliveryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::PendingApproval => "pending_approval",
            Self::Delivered => "delivered",
            Self::Coalesced => "coalesced",
            Self::Skipped => "skipped",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "queued" => Some(Self::Queued),
            "pending_approval" => Some(Self::PendingApproval),
            "delivered" => Some(Self::Delivered),
            "coalesced" => Some(Self::Coalesced),
            "skipped" => Some(Self::Skipped),
            "rejected" => Some(Self::Rejected),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// 群组列表摘要。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workset_id: Option<String>,
    pub mode: GroupMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_member_id: Option<String>,
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
    pub member_count: usize,
    pub active_member_count: usize,
    pub last_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at_ms: Option<i64>,
    pub pending_approval_count: usize,
    pub open_task_count: usize,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// `group.changed` 与 `core.groups.v1` 快照载荷。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupsSnapshot {
    pub revision: u64,
    pub groups: Vec<GroupSummary>,
    /// 所有活跃的 `group_owned` 成员对应的会话 ID，供主侧栏过滤不重复展示。
    #[serde(default)]
    pub group_owned_session_ids: Vec<String>,
}

/// 群组成员视图（包含从内存工作区目录与运行时补齐的会话信息）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupMemberView {
    pub id: String,
    pub group_id: String,
    pub alias: String,
    pub display_name: String,
    pub ownership: MemberOwnership,
    pub session_id: String,
    pub role_kind: MemberRoleKind,
    pub role_title: String,
    pub role_prompt: String,
    pub tool_restriction_profile: String,
    pub briefing_sent: bool,
    pub last_delivered_seq: i64,
    pub last_read_seq: i64,
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at_ms: Option<i64>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub session_alive: bool,
}

/// 群组消息里的单条 `@` 投递视图。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupMentionView {
    pub message_id: String,
    pub target_member_id: String,
    pub target_alias: String,
    pub mention_kind: GroupMentionKind,
    pub delivery_status: GroupDeliveryStatus,
    pub delivery_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    pub updated_at_ms: i64,
}

/// 群组消息视图。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupMessageView {
    pub id: String,
    pub group_id: String,
    pub seq: i64,
    pub sender_type: GroupMessageSenderType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_member_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_alias: Option<String>,
    pub sender_display_name: String,
    pub kind: GroupMessageKind,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_root_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_status: Option<GroupTaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    pub chain_depth: u32,
    pub auto_bridged: bool,
    pub pinned: bool,
    pub created_at_ms: i64,
    #[serde(default)]
    pub mentions: Vec<GroupMentionView>,
}

/// 群组详情（`group.get` 返回值）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupDetail {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workset_id: Option<String>,
    pub mode: GroupMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_member_id: Option<String>,
    pub allow_worker_cross_talk: bool,
    pub max_chain_hops: u32,
    pub max_chain_depth: u32,
    pub max_pair_ping_pong: u32,
    pub auto_bridge_mentions: bool,
    pub paused: bool,
    pub next_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub members: Vec<GroupMemberView>,
    pub open_tasks: Vec<GroupMessageView>,
}

/// `group.message_appended` 事件载荷。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupMessageAppendedEvent {
    pub group_id: String,
    pub message: GroupMessageView,
}

const RESERVED_ALIASES: &[&str] = &["all", "everyone", "here", "user", "system", "group"];

/// 校验并规范化成员 alias：小写 ASCII 字母数字开头，1..=32 字符，只含 `a-z`/`0-9`/`-`/`_`，
/// 禁止保留名。
pub fn validate_member_alias(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_start_matches('@').to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.len() > 32 {
        return Err("Member alias must be 1..=32 ASCII characters".into());
    }
    let bytes = trimmed.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return Err("Member alias must start with a letter or digit".into());
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err("Member alias may only contain lowercase letters, digits, '-' and '_'".into());
    }
    if RESERVED_ALIASES.contains(&trimmed.as_str()) {
        return Err(format!("Member alias '{trimmed}' is reserved"));
    }
    Ok(trimmed)
}

/// 从消息正文中提取 `@alias`（含 `@all`），自动跳过围栏代码块、行内代码和邮箱地址，去重保序。
#[must_use]
pub fn extract_mentions(content: &str) -> Vec<String> {
    let stripped = strip_markdown_code(content);
    let bytes = stripped.as_bytes();
    let mut mentions = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let preceded_by_ident = i > 0
                && (bytes[i - 1].is_ascii_alphanumeric()
                    || matches!(bytes[i - 1], b'_' | b'.' | b'-' | b'+'));
            if !preceded_by_ident {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_'))
                {
                    end += 1;
                }
                if end > start {
                    let token = stripped[start..end].to_ascii_lowercase();
                    if !mentions.contains(&token) {
                        mentions.push(token);
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    mentions
}

fn strip_markdown_code(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_fence = false;
    let mut fence_marker: Option<char> = None;

    for line in input.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(marker) = fence_marker {
            let prefix = if marker == '`' { "```" } else { "~~~" };
            if trimmed.starts_with(prefix) {
                in_fence = false;
                fence_marker = None;
            }
            out.push('\n');
            continue;
        }
        if trimmed.starts_with("```") {
            in_fence = true;
            fence_marker = Some('`');
            out.push('\n');
            continue;
        }
        if trimmed.starts_with("~~~") {
            in_fence = true;
            fence_marker = Some('~');
            out.push('\n');
            continue;
        }
        if !in_fence {
            strip_inline_code_line(line, &mut out);
        }
    }
    out
}

fn strip_inline_code_line(line: &str, out: &mut String) {
    let mut in_code = false;
    for ch in line.chars() {
        if ch == '`' {
            in_code = !in_code;
            out.push(' ');
        } else if in_code {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
}

/// 生成确定性群组投递 ID（`pm-grp-<message_id>-<target_member_id>`）。
/// 以 `pm-` 开头以复用 `<peer-message>` 冷恢复解析，并在重试与重启恢复时保持幂等去重。
#[must_use]
pub fn deterministic_group_delivery_id(message_id: &str, target_member_id: &str) -> String {
    format!("pm-grp-{message_id}-{target_member_id}")
}

/// 判断一条投递 ID 是否属于智能体群组投递。
#[must_use]
pub fn is_group_delivery_id(delivery_id: &str) -> bool {
    delivery_id.starts_with("pm-grp-")
}

/// 群组唤醒正文里的成员名录条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeRosterEntry {
    pub alias: String,
    pub display_name: String,
    pub role_kind: MemberRoleKind,
    pub role_title: String,
    pub is_self: bool,
}

/// 群组唤醒正文里的增量消息条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeMessageEntry {
    pub seq: i64,
    pub sender_label: String,
    pub kind: GroupMessageKind,
    pub task_status: Option<GroupTaskStatus>,
    pub content: String,
}

/// 构造写入 `PeerDelivery.text` 的群组唤醒正文。
#[must_use]
pub fn format_group_wake_body(
    group_id: &str,
    group_title: &str,
    group_description: &str,
    mode: GroupMode,
    self_alias: &str,
    self_role_kind: MemberRoleKind,
    self_role_title: &str,
    self_role_prompt: &str,
    include_briefing: bool,
    use_mcp_tools: bool,
    roster: &[WakeRosterEntry],
    delta_messages: &[WakeMessageEntry],
    open_tasks: &[WakeMessageEntry],
    trigger_seq: i64,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "[Group: {group_title} ({group_id})]");
    let _ = writeln!(
        out,
        "You are @{self_alias} ({}{}). Triggered by message #{trigger_seq}.",
        self_role_kind.as_str(),
        if self_role_title.trim().is_empty() {
            String::new()
        } else {
            format!(" · {}", self_role_title.trim())
        }
    );

    if include_briefing {
        let _ = writeln!(out, "\n### Group Briefing");
        if !group_description.trim().is_empty() {
            let _ = writeln!(out, "Goal: {}", group_description.trim());
        }
        let _ = writeln!(out, "Routing mode: {}", mode.as_str());
        if !self_role_prompt.trim().is_empty() {
            let _ = writeln!(out, "Your role instructions: {}", self_role_prompt.trim());
        }
        if !roster.is_empty() {
            let _ = writeln!(out, "Members:");
            for entry in roster {
                let me = if entry.is_self { " (you)" } else { "" };
                let title = if entry.role_title.trim().is_empty() {
                    String::new()
                } else {
                    format!(" — {}", entry.role_title.trim())
                };
                let _ = writeln!(
                    out,
                    "- @{} [{}]{}{}",
                    entry.alias,
                    entry.role_kind.as_str(),
                    title,
                    me
                );
            }
        }
    }

    if !open_tasks.is_empty() {
        let _ = writeln!(out, "\n### Open Tasks");
        for task in open_tasks {
            let status = task
                .task_status
                .map(GroupTaskStatus::as_str)
                .unwrap_or("open");
            let first_line = task.content.lines().next().unwrap_or("").trim();
            let _ = writeln!(out, "- #{} [{}]: {}", task.seq, status, first_line);
        }
    }

    if !delta_messages.is_empty() {
        let _ = writeln!(out, "\n### Recent Group Messages");
        for msg in delta_messages {
            let kind_tag = match msg.kind {
                GroupMessageKind::Chat => String::new(),
                GroupMessageKind::Task => format!(
                    " [task:{}]",
                    msg.task_status
                        .map(GroupTaskStatus::as_str)
                        .unwrap_or("open")
                ),
                GroupMessageKind::Decision => " [decision]".to_string(),
                GroupMessageKind::System => " [system]".to_string(),
            };
            let _ = writeln!(
                out,
                "#{}{} {}:\n{}",
                msg.seq, kind_tag, msg.sender_label, msg.content
            );
        }
    }

    let _ = writeln!(out, "\n### How to Respond");
    if use_mcp_tools {
        let _ = writeln!(
            out,
            "- To reply in the group, call `group_post` (group_id: \"{group_id}\", reply_to_seq: {trigger_seq}). Mention other members with `@alias` only when you need them to act."
        );
        let _ = writeln!(
            out,
            "- If no reply is needed, call `group_yield` (group_id: \"{group_id}\", up_to_seq: {trigger_seq})."
        );
        let _ = writeln!(
            out,
            "- To read earlier context or update a task, use `group_read`, `group_members`, or `group_task_update`."
        );
    } else {
        let _ = writeln!(
            out,
            "- To reply in the group, run: `smelt group post --group {group_id} --reply-to {trigger_seq} --message \"...\"`"
        );
        let _ = writeln!(
            out,
            "- If no reply is needed, run: `smelt group yield --group {group_id} --up-to {trigger_seq}`"
        );
        let _ = writeln!(
            out,
            "- To inspect history or members, run `smelt group read --group {group_id}` or `smelt group members --group {group_id}`."
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_member_aliases() {
        assert_eq!(validate_member_alias("Coder_1").unwrap(), "coder_1");
        assert_eq!(
            validate_member_alias(" @review-bot ").unwrap(),
            "review-bot"
        );
        assert!(validate_member_alias("").is_err());
        assert!(validate_member_alias("-leading").is_err());
        assert!(validate_member_alias("all").is_err());
        assert!(validate_member_alias("system").is_err());
        assert!(validate_member_alias("bad space").is_err());
        assert!(validate_member_alias(&"a".repeat(33)).is_err());
    }

    #[test]
    fn extracts_mentions_ignoring_code_and_emails() {
        let content = "\
Hi @Coder and @reviewer!
Contact dev@example.com or `inline @ignored_inline`
```rust
// @ignored_fence
let x = \"@also_ignored\";
```
Also ping @coder again and @ALL.
~~~
@ignored_tilde
~~~
Done @qa_1.";
        assert_eq!(
            extract_mentions(content),
            vec!["coder", "reviewer", "all", "qa_1"]
        );
    }

    #[test]
    fn deterministic_delivery_id_has_pm_prefix() {
        let id = deterministic_group_delivery_id("msg-123", "mem-abc");
        assert_eq!(id, "pm-grp-msg-123-mem-abc");
        assert!(is_group_delivery_id(&id));
        assert!(id.starts_with("pm-"));
        assert!(!is_group_delivery_id("pm-abc123"));
    }

    #[test]
    fn formats_group_wake_body_with_briefing_and_delta() {
        let body = format_group_wake_body(
            "grp-1",
            "API Design",
            "Align on v2 endpoints",
            GroupMode::Coordinator,
            "coder",
            MemberRoleKind::Worker,
            "Backend",
            "Write idiomatic Rust",
            true,
            true,
            &[WakeRosterEntry {
                alias: "coder".into(),
                display_name: "Coder".into(),
                role_kind: MemberRoleKind::Worker,
                role_title: "Backend".into(),
                is_self: true,
            }],
            &[WakeMessageEntry {
                seq: 3,
                sender_label: "user".into(),
                kind: GroupMessageKind::Task,
                task_status: Some(GroupTaskStatus::Open),
                content: "@coder draft the schema".into(),
            }],
            &[WakeMessageEntry {
                seq: 3,
                sender_label: "user".into(),
                kind: GroupMessageKind::Task,
                task_status: Some(GroupTaskStatus::Open),
                content: "@coder draft the schema".into(),
            }],
            3,
        );
        assert!(body.contains("### Group Briefing"));
        assert!(body.contains("Write idiomatic Rust"));
        assert!(body.contains("### Open Tasks"));
        assert!(body.contains("group_post"));
        assert!(body.contains("group_yield"));
    }
}
