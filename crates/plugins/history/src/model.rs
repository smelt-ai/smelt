//! 不碰界面的那部分：列表快照、来源标签、行文案、交接说明。单测在这里。
//!
//! 不 `use gpui::*`：带进测试模块会让 trait 解析图爆炸，`cargo test` 编译期撞 rustc 的
//! 递归限制。

use std::collections::HashMap;
use std::rc::Rc;

use gpui::SharedString;
use smelt_plugin_api::client::protocol::history::{HistoryDetail, HistorySession};
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::agent_kind::{AcpProfile, ConversationAgentKind, HistorySourceKind};
use smelt_plugin_api::model::agent_paths::agent_definition_id_for_space_in;
use smelt_plugin_api::model::chrono::{self, DateTime};
use smelt_plugin_api::model::session_handoff::{HandoffContext, HandoffPeer, build_handoff_prompt};
use smelt_plugin_api::model::session_history::{
    SessionDetail, SessionSummary, Turn, handoff_turns_from_history,
};
use smelt_plugin_api::{Project, RuntimeKey};

pub(crate) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 有明显跨度（超过 1 分钟）才标耗时。只有一条消息时只显示时间点。
pub(crate) fn session_when(s: &SessionSummary) -> String {
    match (s.started_at, s.last_active_at) {
        (Some(start), Some(last)) if (last - start).num_minutes() >= 1 => format!(
            "{} · 跑了 {} 分钟",
            last.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
            (last - start).num_minutes()
        ),
        (_, Some(last)) => last
            .with_timezone(&chrono::Local)
            .format("%m-%d %H:%M")
            .to_string(),
        _ => String::new(),
    }
}

/// 行元素键。有 resume id 用它；空 id 退回路径。
fn row_key(session: &SessionSummary) -> SharedString {
    if session.resume_id.is_empty() {
        SharedString::from(session.path.clone())
    } else {
        SharedString::from(session.resume_id.clone())
    }
}

/// 主行是展示标题。有自定义名称时副行留原始标题，否则副行是时间。
fn row_labels(session: &SessionSummary) -> (SharedString, SharedString) {
    let title = SharedString::from(session.title.clone());
    let when = session_when(session);
    let secondary = if session.custom_title.is_some() && !when.is_empty() {
        format!("{} · {when}", session.agent_title)
    } else if session.custom_title.is_some() {
        session.agent_title.clone()
    } else {
        when
    };
    (title, SharedString::from(secondary))
}

/// 一次扫描得到的历史列表。行文案和行元素键在这里算好，render 只克隆。
pub(crate) struct ListSnapshot {
    pub sessions: Rc<Vec<SessionSummary>>,
    pub row_text: Rc<Vec<(SharedString, SharedString)>>,
    pub row_keys: Rc<Vec<SharedString>>,
}

impl ListSnapshot {
    pub(crate) fn new(sessions: Vec<SessionSummary>) -> Self {
        Self {
            row_keys: Rc::new(sessions.iter().map(row_key).collect()),
            row_text: Rc::new(sessions.iter().map(row_labels).collect()),
            sessions: Rc::new(sessions),
        }
    }
}

/// 运行时给的一条历史 → 展示用的概览。
pub(crate) fn summary_from_wire(session: HistorySession) -> SessionSummary {
    SessionSummary {
        title: session
            .custom_title
            .clone()
            .unwrap_or_else(|| session.title.clone()),
        path: session.path,
        agent_title: session.title,
        custom_title: session.custom_title,
        resume_id: session.resume_id,
        started_at: session
            .started_at_ms
            .and_then(DateTime::from_timestamp_millis),
        last_active_at: session
            .last_active_at_ms
            .and_then(DateTime::from_timestamp_millis),
        message_count: session.message_count,
        total_tokens: session.total_tokens,
    }
}

/// 运行时给的一份对话内容 → 展示用的轮次。
pub(crate) fn detail_from_wire(detail: HistoryDetail) -> SessionDetail {
    SessionDetail {
        turns: detail
            .turns
            .into_iter()
            .map(|turn| Turn {
                is_user: turn.is_user,
                timestamp: turn.timestamp_ms.and_then(DateTime::from_timestamp_millis),
                text: turn.text,
                tools: turn.tools,
                tool_paths: turn.tool_paths,
            })
            .collect(),
        model: detail.model,
    }
}

/// 列表缓存的键：各家 agent、各个自定义 Agent、各台机器上的同一个目录都是不同的数据。
pub(crate) fn list_key(
    machine: &RuntimeKey,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    cwd: &str,
) -> String {
    let machine = match machine {
        RuntimeKey::Local => "local",
        RuntimeKey::Remote(id) => id.as_str(),
    };
    format!(
        "{machine}:{}:{}:{cwd}",
        agent.id(),
        profile_id.unwrap_or("default")
    )
}

/// 顶上一个来源标签：基础 agent 或自定义 Agent。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceTab {
    pub kind: HistorySourceKind,
    pub profile_id: Option<String>,
    pub label: String,
}

/// 该有哪些来源标签。智能体的 space 只留它的引擎；普通项目列出所有来源，再加上能读历史
/// 的自定义 Agent。
///
/// 遍历 [`HistorySourceKind::ALL`] 而不是 `ConversationAgentKind::ALL`：能不能读历史
/// 看那台机器上有没有落盘的 transcript，和有没有 ACP 无关。
pub(crate) fn source_tabs<'a>(
    engine: Option<ConversationAgentKind>,
    profiles: impl IntoIterator<Item = &'a AcpProfile>,
) -> Vec<SourceTab> {
    let mut tabs: Vec<SourceTab> = HistorySourceKind::ALL
        .into_iter()
        // 引擎总是 ACP 种类，纯终端来源在智能体 space 里天然不匹配。
        .filter(|kind| engine.is_none_or(|only| kind.acp() == Some(only)))
        .map(|kind| SourceTab {
            kind,
            profile_id: None,
            label: kind.short_label().to_string(),
        })
        .collect();
    if engine.is_none() {
        for profile in profiles {
            // 通用 ACP 的自定义 Agent 没有本地 transcript，不给它开标签。
            let Some(kind) = profile
                .kind()
                .map(HistorySourceKind::from)
                .filter(|kind| HistorySourceKind::ALL.contains(kind))
            else {
                continue;
            };
            tabs.push(SourceTab {
                kind,
                profile_id: Some(profile.id.clone()),
                label: profile.label.clone(),
            });
        }
    }
    tabs
}

/// 标签栏只留有历史会话的来源：没接入本地历史的、这个项目里没用过的，点进去永远是空的。
/// 还没扫完的先不显示，扫到有会话再出现，免得一排标签先全亮再陆续消失。当前选中的那个
/// 始终保留，哪怕它是空的，不然选中态会落在看不见的标签上。
pub(crate) fn tabs_with_sessions(
    tabs: Vec<SourceTab>,
    current: (HistorySourceKind, Option<&str>),
    has_sessions: impl Fn(&SourceTab) -> bool,
) -> Vec<SourceTab> {
    tabs.into_iter()
        .filter(|tab| (tab.kind, tab.profile_id.as_deref()) == current || has_sessions(tab))
        .collect()
}

/// 项目是某个智能体的 space 时，那个智能体的引擎：历史只看这一家。
pub(crate) fn space_engine(
    project: &Project,
    smelt_home: Option<&str>,
    settings: Option<&AgentHostSettings>,
) -> Option<ConversationAgentKind> {
    let id = agent_definition_id_for_space_in(smelt_home?, &project.root)?;
    settings?.find_agent(&id)?.engine_kind()
}

/// 智能体 space 里的历史，或项目里绑过智能体定义的那几条，都按智能体续接。
pub(crate) fn is_agent_session(in_agent_space: bool, bound_definition: Option<&str>) -> bool {
    in_agent_space || bound_definition.is_some()
}

pub(crate) fn continue_label(is_agent_session: bool) -> &'static str {
    if is_agent_session {
        "继续对话"
    } else {
        "ACP 继续"
    }
}

/// 改名框提交的结果：`None` 是没改；`Some(None)` 是清空、恢复默认名称。
pub(crate) fn submitted_title(initial: &str, text: &str) -> Option<Option<String>> {
    let text = text.trim();
    if text == initial.trim() {
        return None;
    }
    Some((!text.is_empty()).then(|| text.to_string()))
}

/// 交接给谁：基础 agent，或某个自定义 Agent。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandoffTarget {
    pub agent: ConversationAgentKind,
    pub profile_id: Option<String>,
    /// 自定义 Agent 的显示名；基础 agent 是 `None`。
    pub profile_label: Option<String>,
}

impl HandoffTarget {
    pub(crate) fn label(&self) -> String {
        self.profile_label
            .clone()
            .unwrap_or_else(|| self.agent.label().to_string())
    }
}

/// 「迁移到」能选的目标：各家基础 agent，再加自定义 Agent，跳过源自己（同一家接着聊走
/// 「继续」）。启动不了的自定义 Agent（命令、目录不对）不列。
pub(crate) fn handoff_targets(
    source: ConversationAgentKind,
    source_profile_id: Option<&str>,
    settings: Option<&AgentHostSettings>,
) -> Vec<HandoffTarget> {
    let mut targets: Vec<HandoffTarget> = ConversationAgentKind::ALL
        .into_iter()
        .filter(|agent| *agent != source || source_profile_id.is_some())
        .map(|agent| HandoffTarget {
            agent,
            profile_id: None,
            profile_label: None,
        })
        .collect();
    let Some(settings) = settings else {
        return targets;
    };
    for profile in &settings.profiles {
        if source_profile_id == Some(profile.id.as_str())
            || settings.profile_launch_spec(profile).is_err()
        {
            continue;
        }
        let Some(agent) = profile.kind() else {
            continue;
        };
        targets.push(HandoffTarget {
            agent,
            profile_id: Some(profile.id.clone()),
            profile_label: Some(profile.label.clone()),
        });
    }
    targets
}

/// 迁移的源：历史页上那一行。
#[derive(Clone, Debug)]
pub(crate) struct HandoffSource {
    pub agent: ConversationAgentKind,
    /// 源自定义 Agent 的显示名；基础 agent 是 `None`。
    pub profile_label: Option<String>,
    pub title: String,
    pub cwd: String,
}

/// 交给目标 agent 的第一句话：前面聊了什么、动过哪些文件、谁接手。
pub(crate) fn handoff_prompt(
    source: &HandoffSource,
    detail: &SessionDetail,
    target: &HandoffTarget,
) -> String {
    build_handoff_prompt(&HandoffContext {
        turns: handoff_turns_from_history(detail),
        source_title: &source.title,
        source: HandoffPeer::new(source.agent, source.profile_label.as_deref()),
        target: HandoffPeer::new(target.agent, target.profile_label.as_deref()),
        cwd: Some(source.cwd.as_str()),
        source_model: detail.model.clone(),
    })
}

/// 运行时随列表给的「会话 → 智能体定义」，只留这台机器上还在的定义。
pub(crate) fn known_bindings(
    bindings: &HashMap<String, String>,
    settings: Option<&AgentHostSettings>,
) -> HashMap<String, String> {
    let Some(settings) = settings else {
        return HashMap::new();
    };
    bindings
        .iter()
        .filter(|(_, id)| settings.find_agent(id).is_some())
        .map(|(resume_id, id)| (resume_id.clone(), id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::agent_definition::AgentDefinition;
    use smelt_plugin_api::model::agent_kind::TerminalAgentKind;
    use smelt_plugin_api::model::session_handoff::HandoffTurn;

    fn summary(title: &str, custom: Option<&str>, agent_title: &str) -> SessionSummary {
        SessionSummary {
            path: String::new(),
            title: title.to_string(),
            agent_title: agent_title.to_string(),
            custom_title: custom.map(str::to_string),
            resume_id: String::new(),
            started_at: None,
            last_active_at: None,
            message_count: 0,
            total_tokens: 0,
        }
    }

    fn turn(is_user: bool, text: &str, tools: &[&str], paths: &[&str]) -> Turn {
        Turn {
            is_user,
            timestamp: None,
            text: text.into(),
            tools: tools.iter().map(|tool| tool.to_string()).collect(),
            tool_paths: paths.iter().map(|path| path.to_string()).collect(),
        }
    }

    #[test]
    fn list_key_separates_machines_agents_and_profiles() {
        let claude = ConversationAgentKind::Claude.into();
        let local = list_key(&RuntimeKey::Local, claude, None, "/app");
        assert_ne!(
            local,
            list_key(&RuntimeKey::Remote("box".into()), claude, None, "/app")
        );
        assert_ne!(
            local,
            list_key(&RuntimeKey::Local, claude, Some("quant"), "/app")
        );
        let codex = ConversationAgentKind::Codex.into();
        assert_ne!(local, list_key(&RuntimeKey::Local, codex, None, "/app"));
    }

    #[test]
    fn snapshot_labels_custom_titles_with_the_agent_title() {
        let named = ListSnapshot::new(vec![summary("用户起的名", Some("用户起的名"), "原始标题")]);
        assert_eq!(named.row_text[0].0.as_ref(), "用户起的名");
        assert_eq!(named.row_text[0].1.as_ref(), "原始标题");
    }

    #[test]
    fn snapshot_row_key_prefers_resume_id() {
        let mut with_id = summary("标题", None, "原始");
        with_id.resume_id = "resume-1".into();
        with_id.path = "/ignored".into();
        assert_eq!(
            ListSnapshot::new(vec![with_id]).row_keys[0].as_ref(),
            "resume-1"
        );

        let mut path_only = summary("标题", None, "原始");
        path_only.path = "/tmp/session".into();
        assert_eq!(
            ListSnapshot::new(vec![path_only]).row_keys[0].as_ref(),
            "/tmp/session"
        );
    }

    #[test]
    fn wire_summary_shows_the_custom_title_first() {
        let summary = summary_from_wire(HistorySession {
            path: "/p".into(),
            resume_id: "r".into(),
            title: "原始".into(),
            custom_title: Some("改过的".into()),
            started_at_ms: Some(0),
            last_active_at_ms: Some(120_000),
            message_count: 3,
            total_tokens: 10,
        });
        assert_eq!(summary.title, "改过的");
        assert_eq!(summary.agent_title, "原始");
        assert!(session_when(&summary).contains("跑了 2 分钟"));
    }

    #[test]
    fn ordinary_project_lists_every_source_including_terminal_only() {
        let tabs = source_tabs(None, []);
        assert_eq!(tabs.len(), HistorySourceKind::ALL.len());
        assert!(tabs.iter().all(|tab| tab.profile_id.is_none()));
        // 只有 TUI 的 agent 也要有：能不能读历史和有没有 ACP 无关。
        assert!(
            tabs.iter()
                .any(|tab| tab.kind
                    == HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity))
        );
    }

    #[test]
    fn agent_space_keeps_only_its_engine() {
        assert_eq!(
            source_tabs(Some(ConversationAgentKind::Pi), []),
            vec![SourceTab {
                kind: ConversationAgentKind::Pi.into(),
                profile_id: None,
                label: ConversationAgentKind::Pi.short_label().to_string(),
            }]
        );
    }

    #[test]
    fn tabs_hide_empty_sources_but_keep_the_selection() {
        let claude: HistorySourceKind = ConversationAgentKind::Claude.into();
        let codex: HistorySourceKind = ConversationAgentKind::Codex.into();
        let shown: Vec<_> = tabs_with_sessions(source_tabs(None, []), (claude, None), |tab| {
            tab.kind == codex
        })
        .into_iter()
        .map(|tab| tab.kind)
        .collect();
        assert_eq!(shown, [claude, codex]);
    }

    fn settings_with_writer() -> AgentHostSettings {
        let mut settings = AgentHostSettings::fresh();
        settings.agents.push(AgentDefinition {
            id: "writer".into(),
            engine_kind_id: ConversationAgentKind::Pi.id().into(),
            ..Default::default()
        });
        settings
    }

    #[test]
    fn space_engine_needs_a_known_agent_under_that_machines_home() {
        let mut settings = settings_with_writer();
        let project = Project {
            machine: RuntimeKey::Remote("box".into()),
            root: "/home/u/.smelt/agents/writer".into(),
        };
        assert_eq!(
            space_engine(&project, Some("/home/u/.smelt"), Some(&settings)),
            Some(ConversationAgentKind::Pi)
        );
        // 不在主目录下、主目录不知道、定义已经删了，都不算 space。
        assert_eq!(
            space_engine(&project, Some("/other"), Some(&settings)),
            None
        );
        assert_eq!(space_engine(&project, None, Some(&settings)), None);
        settings.agents.clear();
        assert_eq!(
            space_engine(&project, Some("/home/u/.smelt"), Some(&settings)),
            None
        );
    }

    #[test]
    fn bound_sessions_continue_as_the_agent() {
        assert!(!is_agent_session(false, None));
        assert!(is_agent_session(false, Some("writer")));
        assert!(is_agent_session(true, None));
        assert_eq!(continue_label(false), "ACP 继续");
        assert_eq!(continue_label(true), "继续对话");
    }

    #[test]
    fn submitting_the_prefilled_name_changes_nothing() {
        assert_eq!(submitted_title("旧名", " 旧名 "), None);
        assert_eq!(submitted_title("旧名", "新名"), Some(Some("新名".into())));
        assert_eq!(submitted_title("旧名", "  "), Some(None));
    }

    #[test]
    fn handoff_targets_skip_the_source_itself() {
        let targets = handoff_targets(ConversationAgentKind::Claude, None, None);
        assert!(
            targets
                .iter()
                .all(|t| t.agent != ConversationAgentKind::Claude)
        );
        assert_eq!(targets.len(), ConversationAgentKind::ALL.len() - 1);
        // 源是自定义 Agent 时，同一家的基础 agent 是另一个身份，可以选。
        let targets = handoff_targets(ConversationAgentKind::Claude, Some("quant"), None);
        assert!(
            targets
                .iter()
                .any(|t| t.agent == ConversationAgentKind::Claude)
        );
    }

    #[test]
    fn history_turns_become_handoff_turns() {
        let detail = SessionDetail {
            model: Some("claude-opus-5".into()),
            turns: vec![
                turn(true, "改一下滚动", &[], &[]),
                turn(false, "改好了", &["Edit", "Read", "Read"], &["src/main.rs"]),
                // 只有工具、没有正文的轮次不该产出空的 Assistant。
                turn(false, "", &["exec_command"], &[]),
            ],
        };
        assert_eq!(
            handoff_turns_from_history(&detail),
            vec![
                HandoffTurn::User {
                    text: "改一下滚动".into(),
                    images: 0,
                },
                HandoffTurn::Assistant("改好了".into()),
                HandoffTurn::Tool {
                    title: "Edit、Read ×2".into(),
                    detail: None,
                    paths: vec!["src/main.rs".into()],
                },
                HandoffTurn::Tool {
                    title: "exec_command".into(),
                    detail: None,
                    paths: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn handoff_prompt_names_both_sides_and_the_source_model() {
        let detail = SessionDetail {
            model: Some("grok-4.5".into()),
            turns: vec![turn(true, "接着干", &[], &[])],
        };
        let source = HandoffSource {
            agent: ConversationAgentKind::Grok,
            profile_label: None,
            title: "老会话".into(),
            cwd: "/repo".into(),
        };
        let target = HandoffTarget {
            agent: ConversationAgentKind::Claude,
            profile_id: None,
            profile_label: None,
        };
        let prompt = handoff_prompt(&source, &detail, &target);
        assert!(prompt.contains("原会话由 Grok 进行"));
        assert!(prompt.contains("现在由你（Claude Code）接手"));
        assert!(prompt.contains("原会话使用的模型：grok-4.5"));
        assert!(prompt.contains("接着干"));
    }

    #[test]
    fn bindings_keep_only_definitions_still_on_that_machine() {
        let mut bindings = HashMap::new();
        bindings.insert("a".to_string(), "gone".to_string());
        bindings.insert("b".to_string(), "writer".to_string());
        let kept = known_bindings(&bindings, Some(&settings_with_writer()));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.get("b").map(String::as_str), Some("writer"));
        assert!(known_bindings(&bindings, None).is_empty());
    }
}
