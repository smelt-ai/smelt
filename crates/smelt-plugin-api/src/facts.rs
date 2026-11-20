//! 几个功能都要的会话派生事实：对话叫什么、是哪家 CLI、是不是自动化跑出来的、摆在侧栏
//! 哪儿。状态见 [`WorkspaceConversation::status`]。都从工作区目录和会话状态现算，规则只有这一份：Rust 插件
//! 直接调，JS 插件经 `conversation_facts` 拿。

use crate::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use crate::model::daemon_state::DaemonSessionState;
use crate::model::runtime_path::PathStyle;
use crate::model::session_control::is_background_acp_session_id;
use crate::model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, project_root_of,
};

/// 对话的名字：运行时算好的展示名（对话的、或第一格终端的），没有就用用户起的名字；都没有
/// 是 `None`，由画的一方写通用名（「对话」「终端」）。
pub fn conversation_name<'a>(
    conversation: &'a WorkspaceConversation,
    states: &'a [DaemonSessionState],
) -> Option<&'a str> {
    let lead = conversation
        .acp
        .as_ref()
        .map(|acp| acp.sid.as_str())
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|terminal| terminal.id.as_deref())
        });
    let named = lead
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.conversation_name.as_deref());
    [named, conversation.custom_title.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
}

/// 对话是哪家 CLI。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationAgent {
    /// 跟智能体的对话（ACP）。
    Conversation(ConversationAgentKind),
    /// 终端里跑的 CLI。
    Terminal(TerminalAgentKind),
}

impl ConversationAgent {
    pub fn id(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.id(),
            Self::Terminal(kind) => kind.id(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.label(),
            Self::Terminal(kind) => kind.label(),
        }
    }

    /// 图标的资源路径（`smelt-icons/agent-<id>.svg`）。
    pub fn icon_asset(self) -> String {
        match self {
            Self::Conversation(kind) => kind.icon_asset(),
            Self::Terminal(kind) => kind.icon_asset(),
        }
    }
}

/// 对话是哪家 CLI：跟智能体的对话按它的智能体；终端按第一格的会话状态报的提供方，没报的
/// 按它的启动命令。认不出是 `None`。
pub fn conversation_agent(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
) -> Option<ConversationAgent> {
    if let Some(acp) = &conversation.acp {
        return ConversationAgentKind::from_id(&acp.agent).map(ConversationAgent::Conversation);
    }
    let first = conversation.terminals.first()?;
    first
        .id
        .as_deref()
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.provider.as_deref())
        .and_then(TerminalAgentKind::from_id)
        .or_else(|| {
            first
                .launch_cmd
                .as_deref()
                .and_then(TerminalAgentKind::from_command_prefix)
        })
        .map(ConversationAgent::Terminal)
}

/// 自动化跑出来的会话：智能体对话绑着自动化，或者是后台起的 ACP 会话。
pub fn is_automation_run(conversation: &WorkspaceConversation) -> bool {
    conversation
        .acp
        .as_ref()
        .is_some_and(|acp| acp.automation_id.is_some() || is_background_acp_session_id(&acp.sid))
}

/// 对话的工作目录：跟智能体的对话的，或第一格终端起的时候的。是起的时候定的，不随终端里
/// `cd` 变（那是会话状态里的 `cwd`），所以分组稳定。空的算没有。
pub fn conversation_cwd(conversation: &WorkspaceConversation) -> Option<&str> {
    let cwd = match &conversation.acp {
        Some(acp) => acp.cwd.as_deref(),
        None => conversation
            .terminals
            .first()
            .and_then(|terminal| terminal.cwd.as_deref()),
    };
    cwd.filter(|cwd| !cwd.is_empty())
}

/// 对话摆在侧栏哪儿（见 [`placement`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// 在一个项目组里。`root` 是打开的项目，或者不在任何项目里时它自己的工作目录（自成
    /// 一组，没有工作目录的是 `""`）。`workset` 是它绑的工作集。
    Project {
        root: String,
        workset: Option<String>,
    },
    /// 「对话」那一段：跟智能体的对话，工作目录不在任何打开的项目里。
    Conversations,
    /// 不在侧栏：已归档（去归档视图看），或自动化的运行（去自动化看）。
    Hidden,
}

/// 对话摆在侧栏哪儿：归档的、自动化跑的不摆；绑着工作集的进工作集所在的项目；工作目录落在
/// 打开的项目里的进那个项目（路径按那台机器的系统比，`style`）；跟智能体的对话进「对话」一段；
/// 其余按工作目录自成一组。
pub fn placement(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
) -> Placement {
    if conversation.archived_at.is_some() || is_automation_run(conversation) {
        return Placement::Hidden;
    }
    if let Some(workset) = conversation
        .workset_id
        .as_ref()
        .and_then(|id| directory.worksets.iter().find(|workset| &workset.id == id))
    {
        return Placement::Project {
            root: workset.project_root.clone(),
            workset: Some(workset.id.clone()),
        };
    }
    let cwd = conversation_cwd(conversation);
    if let Some(root) = cwd.and_then(|cwd| project_root_of(style, &directory.projects, cwd)) {
        return Placement::Project {
            root: root.to_string(),
            workset: None,
        };
    }
    if conversation.agent_definition().is_some() {
        return Placement::Conversations;
    }
    Placement::Project {
        root: cwd.unwrap_or_default().to_string(),
        workset: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::workspace_directory::{WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset};

    fn acp(agent: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: "c".into(),
            acp: Some(WorkspaceAcp {
                sid: "a1".into(),
                agent: agent.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn terminal(launch: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: "t".into(),
            terminals: vec![WorkspaceTerminal {
                id: Some("t1".into()),
                launch_cmd: launch.map(Into::into),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn state(id: &str) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn name_prefers_the_runtime_then_the_users_then_none() {
        let mut conversation = acp("claude");
        assert_eq!(conversation_name(&conversation, &[]), None);
        conversation.custom_title = Some(" 我起的 ".into());
        assert_eq!(conversation_name(&conversation, &[]), Some("我起的"));
        let mut named = state("a1");
        named.conversation_name = Some("运行时的".into());
        let states = [named];
        assert_eq!(conversation_name(&conversation, &states), Some("运行时的"));
        // 空白的名字不算。
        conversation.custom_title = Some("  ".into());
        assert_eq!(conversation_name(&conversation, &[]), None);
    }

    #[test]
    fn agent_comes_from_the_acp_agent_or_the_first_terminal() {
        assert_eq!(
            conversation_agent(&acp("claude"), &[]),
            Some(ConversationAgent::Conversation(
                ConversationAgentKind::Claude
            ))
        );
        assert_eq!(conversation_agent(&acp("unknown"), &[]), None);

        // 终端：会话状态报了提供方的按它，没报的按启动命令的程序名。
        let launched = terminal(Some("/usr/local/bin/claude --resume"));
        assert_eq!(
            conversation_agent(&launched, &[]),
            Some(ConversationAgent::Terminal(
                TerminalAgentKind::from_id("claude").unwrap()
            ))
        );
        let mut reported = state("t1");
        reported.provider = Some("copilot".into());
        assert_eq!(
            conversation_agent(&launched, &[reported]).map(ConversationAgent::id),
            Some("copilot")
        );
        assert_eq!(conversation_agent(&terminal(Some("zsh")), &[]), None);
        assert_eq!(conversation_agent(&terminal(None), &[]), None);
    }

    fn terminal_at(id: &str, terminal: &str, cwd: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: vec![WorkspaceTerminal {
                id: Some(terminal.into()),
                cwd: Some(cwd.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn acp_at(id: &str, sid: &str, cwd: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(WorkspaceAcp {
                sid: sid.into(),
                cwd: cwd.map(Into::into),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn placement_groups_by_workset_project_agent_then_cwd() {
        let style = PathStyle::Posix;
        let mut in_workset = terminal_at("w", "t0", "/elsewhere");
        in_workset.workset_id = Some("ws".into());
        let in_project = terminal_at("p", "t1", "/repo/sub");
        let mut agent_in_project = acp_at("ap", "a0", Some("/repo"));
        agent_in_project.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let mut agent_space = acp_at("a", "a1", Some("/home/u/.smelt/agents/writer"));
        agent_space.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let loose = terminal_at("l", "t2", "/tmp/x");
        let mut no_cwd = terminal_at("n", "t3", "");
        no_cwd.terminals[0].cwd = None;
        let mut archived = terminal_at("old", "t4", "/repo");
        archived.archived_at = Some(1);
        let mut automation = acp_at("auto", "a2", Some("/repo"));
        automation.acp.as_mut().unwrap().automation_id = Some("nightly".into());
        let background = acp_at("bg", "acp-automation-1", Some("/repo"));
        let directory = WorkspaceDirectory {
            projects: vec!["/repo".into()],
            worksets: vec![WorkspaceWorkset {
                id: "ws".into(),
                project_root: "/repo".into(),
                ..Default::default()
            }],
            conversations: vec![
                in_workset,
                in_project,
                agent_in_project,
                agent_space,
                loose,
                no_cwd,
                archived,
                automation,
                background,
            ],
            ..Default::default()
        };
        let placed: Vec<Placement> = directory
            .conversations
            .iter()
            .map(|conversation| placement(style, &directory, conversation))
            .collect();
        let project = |root: &str, workset: Option<&str>| Placement::Project {
            root: root.into(),
            workset: workset.map(Into::into),
        };
        assert_eq!(
            placed,
            [
                project("/repo", Some("ws")),
                project("/repo", None),
                project("/repo", None),
                Placement::Conversations,
                project("/tmp/x", None),
                project("", None),
                Placement::Hidden,
                Placement::Hidden,
                Placement::Hidden,
            ]
        );
    }
}
