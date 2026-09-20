//! 会话交接：把 transcript 压成目标会话的首条 prompt。
//!
//! 历史会话页的「迁移到」仍用这份摘要。Pi 活体「分叉对话」走官方 `--fork`，
//! 复制源 session 文件，不再把摘要当首条 prompt 发给模型。
//!
//! 为什么跨 agent 只能这么做：ACP 的 `session/load` 只认目标 agent 自己 session
//! store 里的 id（见 `acp_conn.rs` 冷恢复那段注释），协议里没有「导入一段外部历史」
//! 的方法。剩下的选项只有两个——把历史转成 prompt，或者把历史伪造进目标 agent 的
//! 私有存档目录。后者要跟各家各自的私有格式（如 Grok 一个会话就是 12 个文件外加一份
//! FTS sqlite 索引）赛跑，上游一改格式就静默错位，因此不做。
//!
//! 各家历史读取器先把私有格式转成 `HandoffTurn`，渲染器只认这份中立 IR；因此裁剪、
//! 工具摘要和目标能力提示不会散落到各 provider 分支。

use crate::acp_chat::{AcpEntry, ToolCallStatus};
use crate::agent_kind::ConversationAgentKind;

/// 整份交接 prompt 的字符上限。
pub const HANDOFF_MAX_CHARS: usize = 24_000;
/// 单条消息的字符上限——一条超长消息不该把其余上下文全挤掉。
const HANDOFF_MESSAGE_MAX_CHARS: usize = 4_000;

/// 交接的一端：哪家 agent，以及（可选的）workspace profile 名。
///
/// profile 参与身份判断：Claude 默认 workspace → 「Claude Quant」profile 也是一次
/// 真实迁移（换了数据目录、换了记忆和配置），不能因为 `kind` 相同就当成原地续接。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandoffPeer<'a> {
    pub agent: ConversationAgentKind,
    /// `None` = 该 agent 的默认 workspace。
    pub profile_label: Option<&'a str>,
}

impl<'a> HandoffPeer<'a> {
    pub fn new(agent: ConversationAgentKind, profile_label: Option<&'a str>) -> Self {
        Self {
            agent,
            profile_label,
        }
    }

    /// 给人看的名字：profile 会带上底层 agent，否则「Claude Quant」这种自定义名
    /// 单独出现时看不出是哪家。
    pub fn display(&self) -> String {
        match self.profile_label {
            Some(label) => format!("{label}（{}）", self.agent.label()),
            None => self.agent.label().to_string(),
        }
    }
}

/// 历史 transcript 迁移用的中立轮次，渲染器只认它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffTurn {
    User {
        text: String,
        /// 附带图片张数。图片本体不交接（见文件头），只交接「有几张」。
        images: usize,
    },
    Assistant(String),
    Tool {
        /// 工具名或读取器生成的工具摘要。
        title: String,
        /// 可选的类别/状态补充信息。
        detail: Option<String>,
        /// 这一步碰过的文件，由历史读取器从工具入参提取。
        paths: Vec<String>,
    },
}

/// 一次交接请求的全部输入。
pub struct HandoffContext<'a> {
    pub turns: Vec<HandoffTurn>,
    pub source_title: &'a str,
    pub source: HandoffPeer<'a>,
    pub target: HandoffPeer<'a>,
    pub cwd: Option<&'a str>,
    /// 历史读取器发现的源模型。只作为交代写进头部，不会真去设置目标模型——
    /// 那是各家私有取值。
    pub source_model: Option<String>,
}

impl HandoffContext<'_> {
    /// 目标与来源不是同一个 agent 身份 = 这是一次迁移，不是原地开新会话。
    pub fn is_migration(&self) -> bool {
        self.source != self.target
    }
}

/// 活体最终回答上的分叉目前只开放给 Pi：走官方 `--fork` 复制 session 文件。
pub fn live_fork_is_available(agent: ConversationAgentKind) -> bool {
    agent == ConversationAgentKind::Pi
}

/// 把活体消息流压成交接 IR。思考块和分割线丢掉；子 agent 的嵌套 transcript 摊平。
pub fn handoff_turns_from_entries(entries: &[AcpEntry]) -> Vec<HandoffTurn> {
    let mut turns = Vec::new();
    push_handoff_turns(entries, &mut turns);
    turns
}

fn push_handoff_turns(entries: &[AcpEntry], turns: &mut Vec<HandoffTurn>) {
    for entry in entries {
        match entry {
            AcpEntry::User(text) => turns.push(HandoffTurn::User {
                text: text.clone(),
                images: 0,
            }),
            AcpEntry::UserWithImages { text, images } => turns.push(HandoffTurn::User {
                text: text.clone(),
                images: images.len(),
            }),
            AcpEntry::Assistant {
                text,
                thought: false,
            } if !text.trim().is_empty() => {
                turns.push(HandoffTurn::Assistant(text.clone()));
            }
            AcpEntry::Assistant { thought: true, .. } => {}
            AcpEntry::ToolCall {
                title,
                status,
                children,
                ..
            } => {
                turns.push(HandoffTurn::Tool {
                    title: title.clone(),
                    detail: tool_status_detail(*status),
                    paths: Vec::new(),
                });
                push_handoff_turns(children, turns);
            }
            AcpEntry::Divider(_) | AcpEntry::Assistant { thought: false, .. } => {}
        }
    }
}

fn tool_status_detail(status: ToolCallStatus) -> Option<String> {
    match status {
        ToolCallStatus::Failed => Some("失败".into()),
        ToolCallStatus::Completed => None,
        ToolCallStatus::Pending | ToolCallStatus::InProgress => Some("进行中".into()),
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let value: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{value}\n[内容已截断]")
    } else {
        value
    }
}

/// 图片说明：目标不收图时要写清楚原因，否则新 agent 只会看到「有图」却永远等不到图。
/// Grok 的 `promptCapabilities.image = false`（见 `agent_kind::default_acp_grok_cmd`）。
fn image_note(count: usize, target: &HandoffPeer<'_>) -> String {
    if target.agent.accepts_images() {
        format!("[附带 {count} 张图片，图片未复制]")
    } else {
        format!(
            "[附带 {count} 张图片；{} 不接受图片输入，未复制]",
            target.agent.label()
        )
    }
}

fn render_turn(turn: &HandoffTurn, target: &HandoffPeer<'_>) -> Option<String> {
    let segment = match turn {
        HandoffTurn::User { text, images } => {
            let text = truncate_chars(text.trim(), HANDOFF_MESSAGE_MAX_CHARS);
            match (*images, text.is_empty()) {
                (0, _) => format!("用户：{text}"),
                (n, true) => format!("用户：{}", image_note(n, target)),
                (n, false) => format!("用户：{text}\n{}", image_note(n, target)),
            }
        }
        HandoffTurn::Assistant(text) => {
            format!(
                "助手：{}",
                truncate_chars(text.trim(), HANDOFF_MESSAGE_MAX_CHARS)
            )
        }
        HandoffTurn::Tool {
            title,
            detail,
            paths,
        } => {
            let detail = detail
                .as_ref()
                .map(|d| format!("（{d}）"))
                .unwrap_or_default();
            let paths = if paths.is_empty() {
                String::new()
            } else {
                format!("；{}", paths.join("，"))
            };
            format!("工具：{title}{detail}{paths}")
        }
    };
    (!segment.trim().is_empty()).then_some(segment)
}

/// 把连续的同名工具折叠成一行带次数的记录。
///
/// 真实会话里这个差别很大：一份 262 轮的 Claude 会话展开后有上百行孤零零的
/// 「工具：Bash」，每一行都占预算却不携带任何新信息，把真正的对话内容挤出窗口。
/// 只折叠**没有文件路径**的连续同名调用——带路径的每一次都指向不同文件，是下一个
/// agent 定位进度的依据，不能合并掉。
fn collapse_repeated_tools(turns: &[HandoffTurn]) -> Vec<HandoffTurn> {
    let mut out: Vec<HandoffTurn> = Vec::with_capacity(turns.len());
    let mut repeat = 1usize;
    for turn in turns {
        let mergeable = matches!(
            (out.last(), turn),
            (
                Some(HandoffTurn::Tool { title: prev, paths: prev_paths, .. }),
                HandoffTurn::Tool { title, paths, .. },
            ) if prev == title && prev_paths.is_empty() && paths.is_empty()
        );
        if mergeable {
            repeat += 1;
            continue;
        }
        if repeat > 1
            && let Some(HandoffTurn::Tool { title, .. }) = out.last_mut()
        {
            *title = format!("{title} ×{repeat}");
        }
        repeat = 1;
        out.push(turn.clone());
    }
    if repeat > 1
        && let Some(HandoffTurn::Tool { title, .. }) = out.last_mut()
    {
        *title = format!("{title} ×{repeat}");
    }
    out
}

/// 生成受控的交接提示：头尾交代身份与核对要求，中间是按预算裁剪过的对话记录。
pub fn build_handoff_prompt(ctx: &HandoffContext<'_>) -> String {
    let segments: Vec<String> = collapse_repeated_tools(&ctx.turns)
        .iter()
        .filter_map(|turn| render_turn(turn, &ctx.target))
        .collect();

    let header = build_header(ctx);
    let footer = build_footer(ctx);
    let fixed = header.chars().count() + footer.chars().count() + 8;
    let budget = HANDOFF_MAX_CHARS.saturating_sub(fixed);
    let mut selected = Vec::new();
    let mut used = 0usize;
    // 倒着装填：预算不够时优先保住最近的上下文，最早那几轮才是可以丢的。
    for segment in segments.into_iter().rev() {
        let len = segment.chars().count() + 2;
        if used + len > budget {
            continue;
        }
        used += len;
        selected.push(segment);
    }
    selected.reverse();
    format!("{header}\n\n{}\n\n{footer}", selected.join("\n\n"))
}

fn build_header(ctx: &HandoffContext<'_>) -> String {
    let mut header = if ctx.is_migration() {
        format!(
            "这是从 Smelt 原会话「{title}」迁移过来的新会话：原会话由 {from} 进行，现在由你（{to}）接手。",
            title = ctx.source_title,
            from = ctx.source.display(),
            to = ctx.target.display(),
        )
    } else {
        format!(
            "这是从 Smelt 原会话「{title}」创建的新 ACP 会话。",
            title = ctx.source_title,
        )
    };
    header.push_str(&format!("\n工作目录：{}", ctx.cwd.unwrap_or("未提供")));
    if let Some(model) = &ctx.source_model {
        header.push_str(&format!("\n原会话使用的模型：{model}"));
    }
    header.push_str("\n以下是原会话的精简交接记录；它不是原会话的无损副本。");
    header
}

fn build_footer(ctx: &HandoffContext<'_>) -> String {
    let base =
        "请先核对当前工作区文件和 Git 状态，再从上述进度继续。不要假设未列出的工具输出仍然有效。";
    if ctx.is_migration() {
        // 跨 agent 时这句是必要的：模型、推理档位、权限模式、MCP 配置、各家自己
        // 注入的规则文件都留在原会话，目标 agent 按自己的环境重新来一遍才是对的。
        format!(
            "{base}\n原会话的模型、权限模式、MCP 配置和 agent 规则文件都不随本次迁移带过来，一切以你当前环境为准。"
        )
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> HandoffPeer<'static> {
        HandoffPeer::new(ConversationAgentKind::Claude, None)
    }

    fn sample_turns() -> Vec<HandoffTurn> {
        vec![
            HandoffTurn::User {
                text: "修复滚动".into(),
                images: 1,
            },
            HandoffTurn::Tool {
                title: "Edit src/acp_view.rs".into(),
                detail: Some("Edit，Completed".into()),
                paths: vec!["src/acp_view.rs".into()],
            },
            HandoffTurn::Assistant("已修复".into()),
        ]
    }

    fn ctx<'a>(
        turns: Vec<HandoffTurn>,
        source: HandoffPeer<'a>,
        target: HandoffPeer<'a>,
    ) -> HandoffContext<'a> {
        HandoffContext {
            turns,
            source_title: "滚动问题",
            source,
            target,
            cwd: Some("/tmp/project"),
            source_model: None,
        }
    }

    #[test]
    fn handoff_keeps_verifiable_history_summary() {
        let prompt = build_handoff_prompt(&ctx(sample_turns(), claude(), claude()));
        assert!(prompt.contains("修复滚动"));
        assert!(prompt.contains("图片未复制"));
        assert!(prompt.contains("src/acp_view.rs"));
        assert!(prompt.contains("已修复"));
    }

    #[test]
    fn handoff_is_bounded_and_keeps_recent_context() {
        let turns = vec![
            HandoffTurn::User {
                text: "x".repeat(HANDOFF_MAX_CHARS * 2),
                images: 0,
            },
            HandoffTurn::Assistant("最近回答".into()),
        ];
        let prompt = build_handoff_prompt(&ctx(turns, claude(), claude()));
        assert!(prompt.chars().count() <= HANDOFF_MAX_CHARS);
        assert!(prompt.contains("最近回答"));
        assert!(prompt.contains("[内容已截断]"));
    }

    #[test]
    fn same_agent_handoff_reads_as_a_new_session_not_a_migration() {
        let ctx = ctx(sample_turns(), claude(), claude());
        assert!(!ctx.is_migration());
        let prompt = build_handoff_prompt(&ctx);
        assert!(prompt.contains("创建的新 ACP 会话"));
        assert!(!prompt.contains("迁移"));
        assert!(!prompt.contains("MCP 配置"));
    }

    #[test]
    fn cross_agent_handoff_names_both_ends_and_disclaims_inherited_config() {
        let ctx = ctx(
            sample_turns(),
            claude(),
            HandoffPeer::new(ConversationAgentKind::Codex, None),
        );
        assert!(ctx.is_migration());
        let prompt = build_handoff_prompt(&ctx);
        assert!(prompt.contains("原会话由 Claude Code 进行"));
        assert!(prompt.contains("现在由你（Codex）接手"));
        assert!(prompt.contains("模型、权限模式、MCP 配置"));
        // 内容裁剪规则不因为换了目标而改变
        assert!(prompt.contains("已修复"));
    }

    #[test]
    fn migrating_to_a_profile_of_the_same_agent_still_counts_as_migration() {
        let ctx = ctx(
            sample_turns(),
            claude(),
            HandoffPeer::new(ConversationAgentKind::Claude, Some("Claude Quant")),
        );
        assert!(ctx.is_migration());
        let prompt = build_handoff_prompt(&ctx);
        assert!(prompt.contains("现在由你（Claude Quant（Claude Code））接手"));
    }

    #[test]
    fn image_note_explains_why_grok_gets_no_images() {
        let prompt = build_handoff_prompt(&ctx(
            sample_turns(),
            claude(),
            HandoffPeer::new(ConversationAgentKind::Grok, None),
        ));
        assert!(prompt.contains("Grok 不接受图片输入，未复制"));
    }

    #[test]
    fn history_sourced_turns_render_without_agent_only_metadata() {
        // 历史 transcript 没有工具类别/状态，只有工具名和入参里的路径。
        let turns = vec![
            HandoffTurn::User {
                text: "接着改".into(),
                images: 0,
            },
            HandoffTurn::Tool {
                title: "Edit".into(),
                detail: None,
                paths: vec!["src/main.rs".into()],
            },
            HandoffTurn::Assistant("改完了".into()),
        ];
        let prompt = build_handoff_prompt(&ctx(
            turns,
            claude(),
            HandoffPeer::new(ConversationAgentKind::Codex, None),
        ));
        assert!(prompt.contains("工具：Edit；src/main.rs"));
        // 没有 detail 时不该留下一对空括号
        assert!(!prompt.contains("Edit（）"));
    }

    #[test]
    fn model_is_declared_in_the_header() {
        let mut context = ctx(
            sample_turns(),
            claude(),
            HandoffPeer::new(ConversationAgentKind::Grok, None),
        );
        context.source_model = Some("claude-opus-5".into());
        let prompt = build_handoff_prompt(&context);
        assert!(prompt.contains("原会话使用的模型：claude-opus-5"));
        assert!(!prompt.contains("完整逐字记录"));
    }

    /// 实测一份 262 轮的真实 Claude 会话：展开后有上百行「工具：Bash」，把对话
    /// 挤出预算。折叠后这些行变成一条带次数的记录，带路径的调用逐条保留。
    #[test]
    fn repeated_pathless_tools_collapse_but_paths_stay_separate() {
        let tool = |title: &str, paths: Vec<&str>| HandoffTurn::Tool {
            title: title.into(),
            detail: None,
            paths: paths.into_iter().map(str::to_string).collect(),
        };
        let turns = vec![
            tool("Bash", vec![]),
            tool("Bash", vec![]),
            tool("Bash", vec![]),
            tool("Read", vec!["a.rs"]),
            tool("Read", vec!["b.rs"]),
            HandoffTurn::Assistant("说明".into()),
            tool("Bash", vec![]),
        ];
        let prompt = build_handoff_prompt(&ctx(turns, claude(), claude()));

        assert!(prompt.contains("工具：Bash ×3"));
        // 带路径的两次 Read 指向不同文件，必须逐条保留
        assert!(prompt.contains("工具：Read；a.rs"));
        assert!(prompt.contains("工具：Read；b.rs"));
        // 被 Assistant 隔开的那次 Bash 不该并进前面的计数
        assert_eq!(prompt.matches("工具：Bash").count(), 2);
    }

    #[test]
    fn live_fork_is_only_offered_for_pi() {
        assert!(live_fork_is_available(ConversationAgentKind::Pi));
        assert!(!live_fork_is_available(ConversationAgentKind::Claude));
        assert!(!live_fork_is_available(ConversationAgentKind::Codex));
    }

    #[test]
    fn live_entries_flatten_subagent_output_and_drop_thoughts() {
        let turns = handoff_turns_from_entries(&[
            AcpEntry::User("用子 agent 输出英文的你好".into()),
            AcpEntry::Assistant {
                text: "先委派".into(),
                thought: true,
            },
            AcpEntry::ToolCall {
                id: "t1".into(),
                title: "subagent".into(),
                kind: crate::acp_chat::ToolKind::Collaborate,
                status: ToolCallStatus::Completed,
                output: Vec::new(),
                children: vec![AcpEntry::Assistant {
                    text: "Hello".into(),
                    thought: false,
                }],
            },
            AcpEntry::Assistant {
                text: "子 agent 已输出：\n\nHello".into(),
                thought: false,
            },
        ]);
        assert_eq!(
            turns,
            vec![
                HandoffTurn::User {
                    text: "用子 agent 输出英文的你好".into(),
                    images: 0,
                },
                HandoffTurn::Tool {
                    title: "subagent".into(),
                    detail: None,
                    paths: Vec::new(),
                },
                HandoffTurn::Assistant("Hello".into()),
                HandoffTurn::Assistant("子 agent 已输出：\n\nHello".into()),
            ]
        );
    }
}
