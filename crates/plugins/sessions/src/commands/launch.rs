//! 新会话在工作区目录里的那条怎么拼：只看命令参数、那台机器的智能体配置和目录，都是纯
//! 函数。启动命令、环境变量、要定下的选项都按会话所在那台机器的配置现算：远程机器有自己
//! 的 CLI 和配置，本机的命令和环境变量（可能含密钥）不能带过去。

use std::collections::BTreeMap;

use smelt_plugin_api::model::agent_definition::agent_engine_kinds;
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::agent_kind::{
    ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind,
};
use smelt_plugin_api::model::agent_paths::agent_definition_id_for_space_in;
use smelt_plugin_api::model::new_session::{LaunchEntry, default_launch_entries};
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory,
};
use smelt_plugin_api::model::workspace_override::split_env_assignment;
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::contract::{Handoff, Resume, StartWith};
use crate::model::{self, Placement};
use crate::peers::agents::{AgentSettingsState, MachineAgentSettings};

/// 从各台机器的智能体配置里取 `machine` 那份。还没收到、给不了都开不了对话：按出厂命令
/// 起的话，用户配的命令和环境变量全不生效。
pub(super) fn settings_of(
    all: Vec<MachineAgentSettings>,
    machine: &RuntimeKey,
) -> Result<AgentHostSettings, String> {
    let state = all
        .into_iter()
        .find(|entry| &entry.machine == machine)
        .map(|entry| entry.state);
    match state {
        Some(AgentSettingsState::Ready { settings }) => Ok(settings),
        Some(AgentSettingsState::Unavailable { reason }) => {
            Err(format!("那台机器的智能体配置不可用：{reason}"))
        }
        Some(AgentSettingsState::Absent) => Err(api::MACHINE_REMOVED.to_string()),
        None => Err("正在读取那台机器的智能体配置，请稍后再试".to_string()),
    }
}

/// [`crate::contract::START_CONVERSATION`] 的那段对话。
pub(super) fn started_acp(
    settings: &AgentHostSettings,
    with: &StartWith,
    cwd: String,
    sid: String,
) -> Result<WorkspaceAcp, String> {
    match with {
        StartWith::Agent {
            agent_definition_id,
        } => agent_acp(settings, agent_definition_id, cwd, sid),
        StartWith::Engine { agent, profile_id } => {
            let kind = ConversationAgentKind::from_id(agent)
                .ok_or_else(|| format!("认不出 agent「{agent}」"))?;
            engine_acp(settings, kind, profile_id.as_deref(), cwd, sid)
        }
    }
}

/// 跟一个智能体开：引擎、启动命令按它的定义，模型定成它选的，权限放开。智能体不在了、
/// 引擎认不出或还没开放都开不了。
fn agent_acp(
    settings: &AgentHostSettings,
    definition_id: &str,
    cwd: String,
    sid: String,
) -> Result<WorkspaceAcp, String> {
    let definition = settings
        .find_agent(definition_id)
        .ok_or_else(|| "智能体已经不存在".to_string())?;
    let Some(kind) = definition.engine_kind() else {
        return Err("请选择一个可用的执行引擎".to_string());
    };
    if !agent_engine_kinds().contains(&kind) {
        return Err(format!(
            "{} 暂未开放为产品级智能体引擎，请改用 Pi",
            kind.short_label()
        ));
    }
    let (kind, launch) = settings.resolve_agent(definition)?;
    let mut config_values = agent_config(kind);
    if let Some(model) = definition.model_config_value() {
        config_values.push(("model".to_string(), model));
    }
    Ok(WorkspaceAcp {
        cwd: Some(cwd),
        sid,
        agent: kind.id().to_string(),
        launch,
        config_values,
        agent_definition_id: Some(definition.id.clone()),
        ..WorkspaceAcp::default()
    })
}

/// 智能体的对话要定下的选项：权限放开（产品智能体由用户显式建、长期托管，默认可自主执行，
/// 不继承普通对话里选过的逐次审批）。别的选项按这个 agent 记住的，由对话插件接上时合进去，
/// 这里不另存一份。
fn agent_config(kind: ConversationAgentKind) -> Vec<(String, String)> {
    kind.task_params()
        .full_access_mode
        .map(|mode| ("mode".to_string(), mode.to_string()))
        .into_iter()
        .collect()
}

/// 裸引擎（或它的一个自定义 Agent）的对话。
fn engine_acp(
    settings: &AgentHostSettings,
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
    cwd: String,
    sid: String,
) -> Result<WorkspaceAcp, String> {
    Ok(WorkspaceAcp {
        cwd: Some(cwd),
        sid,
        agent: kind.id().to_string(),
        launch: engine_launch(settings, kind, profile_id)?,
        profile_id: profile_id.map(str::to_string),
        ..WorkspaceAcp::default()
    })
}

/// 裸引擎的启动规格：自定义 Agent 用它自己的命令和环境变量，基础 agent 用那台机器给它配的。
/// 通用 ACP 只有自定义 Agent，没有自己的命令。
pub(super) fn engine_launch(
    settings: &AgentHostSettings,
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
) -> Result<ConversationLaunchSpec, String> {
    let Some(id) = profile_id else {
        if kind == ConversationAgentKind::Custom {
            return Err("通用 ACP 要选一个自定义 Agent".to_string());
        }
        return Ok(settings.acp_launch_for(kind));
    };
    let profile = settings
        .find_profile(id)
        .ok_or_else(|| format!("找不到自定义 Agent「{id}」，可能已经删了"))?;
    if profile.kind() != Some(kind) {
        return Err(format!(
            "自定义 Agent「{}」不是 {} 的",
            profile.label,
            kind.short_label()
        ));
    }
    settings.profile_launch_spec(profile)
}

/// 分叉出的那段对话：照源对话的 agent、工作目录、启动命令、选项和智能体归属，换一个
/// 守护会话 id；历史由对话插件拉起时复制。源对话的续接身份、待发的话、自动化归属不带。
pub(super) fn forked_acp(source: &WorkspaceAcp, sid: String) -> WorkspaceAcp {
    WorkspaceAcp {
        cwd: source.cwd.clone(),
        sid,
        agent: source.agent.clone(),
        profile_id: source.profile_id.clone(),
        launch: source.launch.clone(),
        config_values: source.config_values.clone(),
        agent_definition_id: source.agent_definition_id.clone(),
        ..WorkspaceAcp::default()
    }
}

/// 侧栏里已经有的同一段历史会话：同一个 agent、工作目录和续接身份。归档了的、自动化
/// 跑的不算（选不了），照常新开一段。
pub(super) fn resumed(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    agent: ConversationAgentKind,
    cwd: &str,
    resume_id: &str,
) -> Option<String> {
    let cwd = style.clean(cwd);
    directory
        .conversations
        .iter()
        .find(|conversation| {
            conversation.acp.as_ref().is_some_and(|acp| {
                acp.agent == agent.id()
                    && acp.history_session_id.as_deref() == Some(resume_id)
                    && acp
                        .cwd
                        .as_deref()
                        .is_some_and(|own| style.clean(own) == cwd)
            }) && model::placement(style, directory, conversation) != Placement::Hidden
        })
        .map(|conversation| conversation.id.clone())
}

/// 续接一段历史会话的那段对话：引擎按 `agent`（自定义 Agent 按它的命令），续接身份记成
/// `resume_id`，对话插件拉起时 `session/load`。属于某个智能体的（工作目录是它的 space，
/// 或参数里给了）按那个智能体续接，权限照智能体对话放开；那个智能体已经不在了就当裸引擎。
/// `smelt_home` 是那台机器的 `SMELT_HOME`，还不知道是 `None`。
pub(super) fn resumed_acp(
    settings: &AgentHostSettings,
    args: &Resume,
    smelt_home: Option<&str>,
    sid: String,
) -> Result<WorkspaceAcp, String> {
    let kind = HistorySourceKind::from_id(&args.agent)
        .and_then(HistorySourceKind::acp)
        .ok_or_else(|| format!("「{}」不支持 ACP 续接", args.agent))?;
    let mut acp = engine_acp(
        settings,
        kind,
        args.profile_id.as_deref(),
        args.cwd.clone(),
        sid,
    )?;
    let definition = args
        .agent_definition_id
        .clone()
        .or_else(|| smelt_home.and_then(|home| agent_definition_id_for_space_in(home, &args.cwd)))
        .filter(|id| settings.find_agent(id).is_some());
    if definition.is_some() {
        acp.config_values = agent_config(kind);
    }
    acp.agent_definition_id = definition;
    acp.history_session_id = Some(args.resume_id.clone());
    Ok(acp)
}

/// 交接的那段对话：目标 agent 从零开始，第一句话是交接说明，接上后由对话插件发出。
pub(super) fn handoff_acp(
    settings: &AgentHostSettings,
    args: &Handoff,
    sid: String,
) -> Result<WorkspaceAcp, String> {
    let kind = ConversationAgentKind::from_id(&args.agent)
        .ok_or_else(|| format!("认不出 agent「{}」，交接取消", args.agent))?;
    let mut acp = engine_acp(
        settings,
        kind,
        args.profile_id.as_deref(),
        args.cwd.clone(),
        sid,
    )?;
    acp.pending_prompt = Some(args.prompt.clone()).filter(|prompt| !prompt.trim().is_empty());
    Ok(acp)
}

/// 新会话绑哪个工作集：工作目录落在哪个工作集的任务目录里就是哪个。
pub(super) fn workset_of(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    cwd: &str,
) -> Option<String> {
    directory
        .worksets
        .iter()
        .find(|workset| workset.contains_cwd(style, cwd))
        .map(|workset| workset.id.clone())
}

/// 在终端里续接的命令行：那台机器的启动项里认得出这家的那条（`entries` 是那台机器的，
/// 用户改过的在前，后面补出厂的），后面接上各家 CLI 自己的续接参数；`env`（自定义 Agent
/// 的环境变量）加在最前面。这家没有能在终端里续接的 CLI 是 `None`。
pub(super) fn cli_resume_line(
    style: PathStyle,
    entries: &[LaunchEntry],
    agent: HistorySourceKind,
    resume_id: &str,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let base = entries
        .iter()
        .filter(|entry| !entry.label.trim().is_empty() && !entry.command.trim().is_empty())
        .cloned()
        .chain(default_launch_entries())
        .find(|entry| command_matches_agent(style, &entry.command, agent))
        .map(|entry| entry.command)
        .unwrap_or_default();
    let command = cli_resume_command(agent, &base, resume_id)?;
    let mut line = String::new();
    for (name, value) in env {
        line.push_str(name);
        line.push('=');
        line.push_str(&env_value(value));
        line.push(' ');
    }
    line.push_str(&command);
    Some(line)
}

/// 各家 CLI 的续接语法不同，但都用同一个 agent 自己的会话 id。`base` 是启动命令（可带
/// 环境变量前缀和参数），空的用这家的程序名。
fn cli_resume_command(agent: HistorySourceKind, base: &str, resume_id: &str) -> Option<String> {
    let terminal = agent.terminal()?;
    let suffix = agent.cli_resume_syntax()?.suffix(&shell_quote(resume_id));
    let base = base.trim();
    Some(if base.is_empty() {
        format!("{} {suffix}", terminal.cli_program())
    } else {
        format!("{base} {suffix}")
    })
}

/// 启动命令跑的是不是这家的 CLI：跳过开头的 `VAR=value`、`env`，看程序名。没有终端 CLI
/// 的 agent 什么都匹配不上。
fn command_matches_agent(style: PathStyle, command: &str, agent: HistorySourceKind) -> bool {
    let Some(terminal) = agent.terminal() else {
        return false;
    };
    let program = command
        .split_whitespace()
        .find(|token| *token != "env" && split_env_assignment(token).is_none())
        .and_then(|token| style.file_name(token));
    program == Some(terminal.cli_program())
}

/// 单引号包住一个 shell 参数：会话 id、路径都不能直接拼进命令行。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 环境变量的值：开头的 `~` 换成那台机器 shell 里的 `$HOME`（加了引号的 `~` shell 不会
/// 展开，在这边展开又成了本机的主目录），其余照原样加引号。
fn env_value(value: &str) -> String {
    if value == "~" {
        return "\"$HOME\"".to_string();
    }
    match value.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", shell_quote(rest)),
        None => shell_quote(value),
    }
}

/// 新会话在目录里的那条（还没有对话、终端）。
pub(super) fn blank_conversation(id: String, workset_id: Option<String>) -> WorkspaceConversation {
    WorkspaceConversation {
        id,
        workset_id,
        ..WorkspaceConversation::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::agent_definition::AgentDefinition;
    use smelt_plugin_api::model::agent_kind::{AcpProfile, TerminalAgentKind};
    use smelt_plugin_api::model::workspace_directory::WorkspaceWorkset;

    fn ready(machine: RuntimeKey, settings: AgentHostSettings) -> MachineAgentSettings {
        MachineAgentSettings {
            machine,
            state: AgentSettingsState::Ready { settings },
        }
    }

    #[test]
    fn settings_come_from_that_machine_only() {
        let local = AgentHostSettings::fresh();
        let all = vec![ready(RuntimeKey::Local, local.clone())];
        assert_eq!(settings_of(all.clone(), &RuntimeKey::Local).unwrap(), local);
        let remote = settings_of(all, &RuntimeKey::Remote("box".into())).unwrap_err();
        assert!(remote.contains("正在读取"), "{remote}");

        let unavailable = vec![MachineAgentSettings {
            machine: RuntimeKey::Local,
            state: AgentSettingsState::Unavailable {
                reason: "运行时太旧".into(),
            },
        }];
        let error = settings_of(unavailable, &RuntimeKey::Local).unwrap_err();
        assert!(error.contains("运行时太旧"), "{error}");
    }

    fn with_agent(engine: &str, model: (&str, &str)) -> AgentHostSettings {
        let mut settings = AgentHostSettings::fresh();
        settings.agents.push(AgentDefinition {
            id: "writer".into(),
            name: "写手".into(),
            engine_kind_id: engine.into(),
            model_provider: model.0.into(),
            model_id: model.1.into(),
            ..AgentDefinition::default()
        });
        settings
    }

    #[test]
    fn an_agent_starts_on_its_engine_with_its_model() {
        let settings = with_agent("pi", ("anthropic", "opus"));
        let with = StartWith::Agent {
            agent_definition_id: "writer".into(),
        };
        let acp = started_acp(&settings, &with, "/space".into(), "acp-1".into()).unwrap();
        assert_eq!(acp.agent, ConversationAgentKind::Pi.id());
        assert_eq!(
            acp.launch,
            settings.acp_launch_for(ConversationAgentKind::Pi)
        );
        assert_eq!(acp.agent_definition_id.as_deref(), Some("writer"));
        assert_eq!(
            (acp.sid.as_str(), acp.cwd.as_deref()),
            ("acp-1", Some("/space"))
        );
        assert!(
            acp.config_values
                .contains(&("model".to_string(), "anthropic/opus".to_string()))
        );
    }

    #[test]
    fn an_agent_that_cannot_converse_does_not_start() {
        let gone = StartWith::Agent {
            agent_definition_id: "gone".into(),
        };
        let settings = AgentHostSettings::fresh();
        let error = started_acp(&settings, &gone, "/r".into(), "s".into()).unwrap_err();
        assert_eq!(error, "智能体已经不存在");

        let writer = StartWith::Agent {
            agent_definition_id: "writer".into(),
        };
        let unknown = with_agent("no-such-engine", ("", ""));
        let error = started_acp(&unknown, &writer, "/r".into(), "s".into()).unwrap_err();
        assert_eq!(error, "请选择一个可用的执行引擎");

        let closed = with_agent(ConversationAgentKind::Claude.id(), ("", ""));
        let error = started_acp(&closed, &writer, "/r".into(), "s".into()).unwrap_err();
        assert!(error.contains("请改用 Pi"), "{error}");
    }

    #[test]
    fn a_bare_engine_uses_the_machines_command_or_its_profile() {
        let mut settings = AgentHostSettings::fresh();
        let claude = ConversationAgentKind::Claude;
        let bare = StartWith::Engine {
            agent: claude.id().into(),
            profile_id: None,
        };
        let acp = started_acp(&settings, &bare, "/r".into(), "s".into()).unwrap();
        assert_eq!(acp.launch, settings.acp_launch_for(claude));
        assert_eq!((acp.profile_id, acp.agent_definition_id), (None, None));
        assert!(acp.config_values.is_empty());

        settings.profiles.push(AcpProfile {
            id: "quant".into(),
            kind_id: claude.id().into(),
            label: "Quant".into(),
            command: String::new(),
            env: BTreeMap::new(),
            workspace_dir: String::new(),
        });
        let profiled = StartWith::Engine {
            agent: claude.id().into(),
            profile_id: Some("quant".into()),
        };
        let acp = started_acp(&settings, &profiled, "/r".into(), "s".into()).unwrap();
        let profile = settings.find_profile("quant").unwrap();
        assert_eq!(acp.launch, settings.profile_launch_spec(profile).unwrap());
        assert_eq!(acp.profile_id.as_deref(), Some("quant"));

        let wrong_kind = StartWith::Engine {
            agent: ConversationAgentKind::Codex.id().into(),
            profile_id: Some("quant".into()),
        };
        assert!(started_acp(&settings, &wrong_kind, "/r".into(), "s".into()).is_err());
        let missing = StartWith::Engine {
            agent: claude.id().into(),
            profile_id: Some("gone".into()),
        };
        let error = started_acp(&settings, &missing, "/r".into(), "s".into()).unwrap_err();
        assert!(error.contains("可能已经删了"), "{error}");
    }

    #[test]
    fn a_fork_keeps_the_source_setup_but_not_its_identity() {
        let source = WorkspaceAcp {
            cwd: Some("/repo".into()),
            sid: "acp-a".into(),
            agent: "pi".into(),
            profile_id: Some("p".into()),
            history_session_id: Some("h".into()),
            launch: ConversationLaunchSpec::from_command("pi --x"),
            config_values: vec![("model".into(), "a/b".into())],
            pending_prompt: Some("还没发".into()),
            agent_definition_id: Some("writer".into()),
            automation_id: Some("auto".into()),
            ..WorkspaceAcp::default()
        };
        let fork = forked_acp(&source, "acp-b".into());
        assert_eq!(fork.sid, "acp-b");
        assert_eq!(
            (fork.cwd, fork.agent, fork.profile_id, fork.launch),
            (
                source.cwd.clone(),
                source.agent.clone(),
                source.profile_id.clone(),
                source.launch.clone()
            )
        );
        assert_eq!(fork.config_values, source.config_values);
        assert_eq!(fork.agent_definition_id, source.agent_definition_id);
        assert_eq!(
            (
                fork.history_session_id,
                fork.pending_prompt,
                fork.automation_id
            ),
            (None, None, None)
        );
    }

    fn conversation_with(id: &str, acp: WorkspaceAcp) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(acp),
            ..WorkspaceConversation::default()
        }
    }

    #[test]
    fn resuming_finds_only_the_same_history_in_the_sidebar() {
        let claude = ConversationAgentKind::Claude;
        let resumed_acp = |sid: &str, history: &str| WorkspaceAcp {
            cwd: Some("/repo/".into()),
            sid: sid.into(),
            agent: claude.id().into(),
            history_session_id: Some(history.into()),
            ..WorkspaceAcp::default()
        };
        let mut archived = conversation_with("old", resumed_acp("acp-0", "h1"));
        archived.archived_at = Some(1);
        let directory = WorkspaceDirectory {
            conversations: vec![
                archived,
                conversation_with("other", resumed_acp("acp-1", "h2")),
                conversation_with("open", resumed_acp("acp-2", "h1")),
            ],
            ..WorkspaceDirectory::default()
        };
        let style = PathStyle::Posix;
        assert_eq!(
            resumed(style, &directory, claude, "/repo", "h1").as_deref(),
            Some("open")
        );
        assert_eq!(resumed(style, &directory, claude, "/repo", "h3"), None);
        assert_eq!(
            resumed(
                style,
                &directory,
                ConversationAgentKind::Codex,
                "/repo",
                "h1"
            ),
            None
        );
    }

    fn resume_args(cwd: &str, definition: Option<&str>) -> Resume {
        Resume {
            machine: RuntimeKey::Local,
            agent: ConversationAgentKind::Pi.id().into(),
            profile_id: None,
            cwd: cwd.into(),
            resume_id: "h1".into(),
            agent_definition_id: definition.map(str::to_string),
        }
    }

    #[test]
    fn resuming_keeps_the_agent_it_belonged_to_while_it_exists() {
        let settings = with_agent("pi", ("", ""));
        let acp = resumed_acp(
            &settings,
            &resume_args("/repo", Some("writer")),
            None,
            "s".into(),
        )
        .unwrap();
        assert_eq!(acp.history_session_id.as_deref(), Some("h1"));
        assert_eq!(acp.agent_definition_id.as_deref(), Some("writer"));

        let space = "/home/u/.smelt/agents/writer";
        let acp = resumed_acp(
            &settings,
            &resume_args(space, None),
            Some("/home/u/.smelt"),
            "s".into(),
        )
        .unwrap();
        assert_eq!(acp.agent_definition_id.as_deref(), Some("writer"));

        let acp = resumed_acp(
            &settings,
            &resume_args("/repo", Some("gone")),
            None,
            "s".into(),
        )
        .unwrap();
        assert_eq!(acp.agent_definition_id, None);
        assert!(acp.config_values.is_empty());

        let terminal_only = Resume {
            agent: HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity)
                .id()
                .into(),
            ..resume_args("/repo", None)
        };
        assert!(resumed_acp(&settings, &terminal_only, None, "s".into()).is_err());
    }

    #[test]
    fn a_handoff_carries_its_prompt_as_the_first_message() {
        let settings = AgentHostSettings::fresh();
        let args = Handoff {
            machine: RuntimeKey::Local,
            cwd: "/repo".into(),
            agent: ConversationAgentKind::Codex.id().into(),
            profile_id: None,
            prompt: "接着做".into(),
        };
        let acp = handoff_acp(&settings, &args, "acp-1".into()).unwrap();
        assert_eq!(acp.pending_prompt.as_deref(), Some("接着做"));
        assert_eq!(acp.history_session_id, None);
        assert_eq!(
            acp.launch,
            settings.acp_launch_for(ConversationAgentKind::Codex)
        );
    }

    #[test]
    fn a_new_session_binds_the_workset_its_directory_is_in() {
        let directory = WorkspaceDirectory {
            worksets: vec![WorkspaceWorkset {
                id: "w1".into(),
                project_root: "/repo".into(),
                task: "fix".into(),
                ..WorkspaceWorkset::default()
            }],
            ..WorkspaceDirectory::default()
        };
        let style = PathStyle::Posix;
        assert_eq!(
            workset_of(style, &directory, "/repo/worktrees/fix/app").as_deref(),
            Some("w1")
        );
        assert_eq!(workset_of(style, &directory, "/repo"), None);
        assert_eq!(workset_of(style, &directory, "/repo/worktrees/fixed"), None);
    }

    #[test]
    fn resuming_in_a_terminal_uses_the_machines_launch_entry() {
        let claude: HistorySourceKind = ConversationAgentKind::Claude.into();
        let style = PathStyle::Posix;
        let none = BTreeMap::new();
        let entries = vec![LaunchEntry {
            label: "Claude".into(),
            command: "CLAUDE_CONFIG_DIR=/x claude --allow-all".into(),
            provider: None,
        }];
        assert_eq!(
            cli_resume_line(style, &entries, claude, "a session's id", &none).as_deref(),
            Some("CLAUDE_CONFIG_DIR=/x claude --allow-all --resume 'a session'\\''s id'")
        );
        // 那台机器没配这家的启动项，用出厂的。
        let codex: HistorySourceKind = ConversationAgentKind::Codex.into();
        let line = cli_resume_line(style, &entries, codex, "h", &none).unwrap();
        assert!(line.ends_with("resume 'h'"), "{line}");
        assert!(line.starts_with("codex"), "{line}");

        let env = BTreeMap::from([
            ("A".to_string(), "~/.claude-quant".to_string()),
            ("B".to_string(), "x y".to_string()),
        ]);
        assert_eq!(
            cli_resume_line(style, &entries, claude, "h", &env).as_deref(),
            Some(
                "A=\"$HOME\"/'.claude-quant' B='x y' \
                 CLAUDE_CONFIG_DIR=/x claude --allow-all --resume 'h'"
            )
        );
    }

    #[test]
    fn a_terminal_only_agent_resumes_with_its_own_program() {
        let antigravity = HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity);
        let line =
            cli_resume_line(PathStyle::Posix, &[], antigravity, "c1", &BTreeMap::new()).unwrap();
        assert!(line.ends_with("--conversation 'c1'"), "{line}");
        assert!(command_matches_agent(PathStyle::Posix, "agy", antigravity));
        assert!(command_matches_agent(
            PathStyle::Posix,
            "env COPILOT_HOME=/tmp/c /usr/local/bin/copilot --allow-all",
            ConversationAgentKind::Copilot.into()
        ));
        assert!(!command_matches_agent(
            PathStyle::Posix,
            "claude",
            ConversationAgentKind::Codex.into()
        ));
    }
}
