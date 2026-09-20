//! 「新建会话」的启动动作目录：桌面弹层与移动端选择器共用这一份。
//!
//! 目录里只有具体动作：常用、终端、对话。「终端 / 对话」是动作的属性，不是
//! 一层模式切换；Pin（常用）绑定到动作本身，所以同一个 Agent 的对话和终端可以
//! 分别置顶。两端都读同一份 SQLite 偏好与同一套 key，桌面 Pin 完手机立刻同序。
//!
//! 这里只负责「有哪些动作、叫什么、怎么分组」。真正怎么把动作跑起来由各端自己
//! 决定：桌面走 GUI 的会话工作区，移动端走网关的 createSession。

use crate::acp_conn::AcpRuntimeDiagnostics;
use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use crate::session_control::AcpAgentOption;

const NEW_SESSION_SQLITE_NAMESPACE: &str = "preferences";
const NEW_SESSION_SQLITE_KEY: &str = "new_session_pins";

/// 分组标题。顺序与 [`NewSessionSections::into_array`] 一致。
pub const SECTION_LABELS: [&str; 3] = ["常用", "终端", "对话"];

/// 项目行「+」下拉菜单里的一条可配置启动项：显示名 + shell 启动命令。
/// `provider` 标识这条启动项属于哪个 agent 协议族（如 "claude"/"codex"），
/// 供插件任务按 agent provider 匹配执行通道；用户自定义项可为空。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct LaunchEntry {
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// 出厂默认启动项：从 `TerminalAgentKind::ALL` 派生。终端能力与 ACP 能力分开
/// 维护，避免 Antigravity 这类只有 TUI/hooks 的客户端误出现在原生对话菜单里。
pub fn default_launch_entries() -> Vec<LaunchEntry> {
    TerminalAgentKind::ALL
        .into_iter()
        .map(|agent| LaunchEntry {
            label: agent.quick_terminal_label().to_string(),
            command: agent.quick_terminal_cmd().to_string(),
            provider: Some(agent.id().to_string()),
        })
        .collect()
}

/// 落盘的启动项（无 GUI 进程也能读）。库里没有这份快照时退回出厂默认，
/// 否则守护侧会在用户第一次改设置之前把终端分组渲染成空的。
pub fn stored_launch_entries() -> Vec<LaunchEntry> {
    let snapshot = crate::sqlite_state::default_sqlite_store()
        .ok()
        .and_then(|store| store.get_launch_snapshot().ok().flatten());
    let Some(snapshot) = snapshot else {
        return default_launch_entries();
    };
    snapshot
        .entries
        .into_iter()
        .map(|entry| LaunchEntry {
            label: entry.label,
            command: entry.command,
            provider: entry.provider,
        })
        .collect()
}

/// 终端 CLI 探测：诊断快照里没有记录就当成不可用，别把注册遗漏伪装成已安装。
pub fn terminal_agent_detected(
    kind: TerminalAgentKind,
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> bool {
    match diagnostics {
        Some(diag) => diag
            .for_terminal(kind)
            .is_some_and(crate::acp_conn::RuntimeExecutable::is_available),
        None => true,
    }
}

/// ACP agent 探测。`None`（还没探测完）暂时放行，避免首帧空列表。
pub fn acp_agent_detected(
    kind: ConversationAgentKind,
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> bool {
    match diagnostics {
        Some(diag) => diag
            .for_agent(kind)
            .is_some_and(crate::acp_conn::RuntimeExecutable::is_available),
        None => true,
    }
}

/// 内置 agent 启动项跟探测结果挂钩；对不上任何内置 CLI 的自定义命令始终显示。
pub fn launch_entry_detected(command: &str, diagnostics: Option<&AcpRuntimeDiagnostics>) -> bool {
    match TerminalAgentKind::from_command_prefix(command) {
        Some(kind) => terminal_agent_detected(kind, diagnostics),
        None => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NewSessionActionKind {
    Terminal,
    Conversation,
}

impl NewSessionActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "对话",
            Self::Terminal => "终端",
        }
    }
}

/// 动作要启动的东西。移动端只认 `agentOptionId` / `key`，命令行永远在这台机器上
/// 拼；`command` 只是给桌面直接复用，手机不会把它回传当参数用。
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "target", rename_all = "camelCase")]
pub enum NewSessionTarget {
    #[serde(rename_all = "camelCase")]
    Conversation {
        agent_option_id: String,
        /// 引擎 id（`claude`/`pi`…），供两端画图标。
        agent_kind: String,
    },
    Terminal {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    BlankTerminal,
}

/// 新建选择器里的一行。
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionAction {
    /// Pin 与跨端引用的稳定标识。终端项用 provider + 命令（改名后 Pin 仍有效）。
    pub key: String,
    /// 不含类型后缀的名称；渲染时统一显示为「名称 · 对话/终端」。
    pub label: String,
    pub kind: NewSessionActionKind,
    pub pinned: bool,
    #[serde(flatten)]
    pub target: NewSessionTarget,
}

impl NewSessionAction {
    pub fn agent_option_id(&self) -> Option<&str> {
        match &self.target {
            NewSessionTarget::Conversation {
                agent_option_id, ..
            } => Some(agent_option_id),
            _ => None,
        }
    }

    /// 终端动作要先跑的命令。空白终端返回 None（就是一个干净的 shell）。
    pub fn terminal_command(&self) -> Option<&str> {
        match &self.target {
            NewSessionTarget::Terminal { command, .. } => Some(command),
            _ => None,
        }
    }
}

/// 三个分组。常用不额外占一层，和终端 / 对话同级。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewSessionSections {
    pub common: Vec<NewSessionAction>,
    pub terminal: Vec<NewSessionAction>,
    pub conversation: Vec<NewSessionAction>,
}

impl NewSessionSections {
    pub fn into_array(self) -> [Vec<NewSessionAction>; 3] {
        [self.common, self.terminal, self.conversation]
    }

    pub fn is_empty(&self) -> bool {
        self.common.is_empty() && self.terminal.is_empty() && self.conversation.is_empty()
    }
}

/// 新建菜单的用户偏好单独存档，直接写入 SQLite；不读取旧 JSON，也不做旧格式迁移。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSessionPreferences {
    pub pinned: Vec<String>,
}

impl NewSessionPreferences {
    pub fn toggle(&mut self, key: &str) {
        if let Some(index) = self.pinned.iter().position(|pinned| pinned == key) {
            self.pinned.remove(index);
        } else {
            self.pinned.push(key.to_string());
        }
    }
}

pub fn load_preferences() -> NewSessionPreferences {
    match crate::sqlite_state::load_sqlite_kv::<NewSessionPreferences>(
        NEW_SESSION_SQLITE_NAMESPACE,
        NEW_SESSION_SQLITE_KEY,
    ) {
        Ok(Some(preferences)) => preferences,
        Ok(None) => NewSessionPreferences::default(),
        Err(error) => {
            eprintln!("[new-session] 读取 SQLite Pin 偏好失败，使用默认值: {error}");
            NewSessionPreferences::default()
        }
    }
}

pub fn save_preferences(preferences: &NewSessionPreferences) {
    if let Err(error) = crate::sqlite_state::save_sqlite_kv(
        NEW_SESSION_SQLITE_NAMESPACE,
        NEW_SESSION_SQLITE_KEY,
        preferences,
    ) {
        eprintln!("[new-session] 保存 SQLite Pin 偏好失败: {error}");
    }
}

/// 对话动作的 Pin key。产品智能体按定义 id，workspace profile 按 profile id，
/// 其余按引擎 id——同一个引擎的不同入口必须是不同的 key。
pub fn conversation_key(option: &AcpAgentOption) -> String {
    if let Some(id) = option.agent_definition_id() {
        return format!("conversation:agent:{id}");
    }
    match option.profile_id() {
        Some(profile_id) => format!("conversation:{}:profile:{profile_id}", option.kind),
        None => format!("conversation:{}", option.kind),
    }
}

/// 终端动作的 Pin key。provider + command 不依赖显示名，用户改名后 Pin 仍然有效；
/// 命令本身是自定义启动项目前唯一能跨版本稳定识别的字段。
pub fn terminal_key(entry: &LaunchEntry) -> String {
    format!(
        "terminal:{}:{}",
        entry.provider.as_deref().unwrap_or_default(),
        entry.command.trim()
    )
}

/// 空白终端也是一个明确的启动预设，可以和其它终端一样 Pin。
pub const BLANK_TERMINAL_KEY: &str = "terminal:blank";
pub const BLANK_TERMINAL_LABEL: &str = "空白终端";

fn entry_kind(entry: &LaunchEntry) -> Option<TerminalAgentKind> {
    entry
        .provider
        .as_deref()
        .and_then(TerminalAgentKind::from_id)
        .or_else(|| TerminalAgentKind::from_command_prefix(&entry.command))
}

fn entry_order(entry: &LaunchEntry) -> usize {
    entry_kind(entry)
        .and_then(|kind| TerminalAgentKind::ALL.iter().position(|item| *item == kind))
        .unwrap_or(usize::MAX)
}

/// 本机当前可用的全部启动动作，顺序即展示顺序：
/// 终端按注册表顺序（自定义项垫底）+ 空白终端；对话是产品智能体 → 裸引擎 → profile。
///
/// `conversation_options` 由调用方给出（桌面用 GUI 配置产出的那份，守护用
/// [`stored_conversation_options`]），这样「有哪些对话入口」只有一处定义。
pub fn new_session_actions(
    conversation_options: &[AcpAgentOption],
    launch_entries: &[LaunchEntry],
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> Vec<NewSessionAction> {
    let mut actions = Vec::new();

    let mut entries: Vec<LaunchEntry> = launch_entries
        .iter()
        .filter(|entry| !entry.label.trim().is_empty() && !entry.command.trim().is_empty())
        .filter(|entry| launch_entry_detected(&entry.command, diagnostics))
        .cloned()
        .collect();
    entries.sort_by_key(entry_order);
    for entry in entries {
        actions.push(NewSessionAction {
            key: terminal_key(&entry),
            label: entry.label.clone(),
            kind: NewSessionActionKind::Terminal,
            pinned: false,
            target: NewSessionTarget::Terminal {
                command: entry.command,
                provider: entry.provider,
            },
        });
    }
    actions.push(NewSessionAction {
        key: BLANK_TERMINAL_KEY.to_string(),
        label: BLANK_TERMINAL_LABEL.to_string(),
        kind: NewSessionActionKind::Terminal,
        pinned: false,
        target: NewSessionTarget::BlankTerminal,
    });

    for option in conversation_options {
        let Some(kind) = ConversationAgentKind::from_id(&option.kind) else {
            continue;
        };
        if !acp_agent_detected(kind, diagnostics) {
            continue;
        }
        actions.push(NewSessionAction {
            key: conversation_key(option),
            label: option.label.clone(),
            kind: NewSessionActionKind::Conversation,
            pinned: false,
            target: NewSessionTarget::Conversation {
                agent_option_id: option.id.clone(),
                agent_kind: option.kind.clone(),
            },
        });
    }

    actions
}

/// 无 GUI 进程时的对话入口清单：产品智能体在前，裸引擎与 profile 在后——与桌面
/// 新建菜单同序，别让同一份清单在两端读出不同的推荐顺序。
pub fn stored_conversation_options() -> Vec<AcpAgentOption> {
    let mut options = crate::session_control::agent_definition_options();
    options.extend(crate::session_control::agent_options());
    options
}

/// 守护/移动端侧的完整目录：读同一份落盘配置与同一份 Pin 偏好。
pub fn stored_new_session_sections(
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> NewSessionSections {
    let actions = new_session_actions(
        &stored_conversation_options(),
        &stored_launch_entries(),
        diagnostics,
    );
    build_sections(&actions, &load_preferences())
}

/// 把动作分成「常用 / 终端 / 对话」。常用动作从后两个分组移出，避免同一动作
/// 出现两次；偏好里的暂时不可用动作 key 仍保留，等对应启动项回来后自动出现。
pub fn build_sections(
    actions: &[NewSessionAction],
    preferences: &NewSessionPreferences,
) -> NewSessionSections {
    let mut sections = NewSessionSections::default();

    // 先按用户 Pin 的顺序排常用项，顺序本身也是用户可感知的偏好。
    for key in &preferences.pinned {
        let Some(source) = actions.iter().find(|action| &action.key == key) else {
            continue;
        };
        if sections.common.iter().any(|item| item.key == source.key) {
            continue;
        }
        let mut action = source.clone();
        action.pinned = true;
        sections.common.push(action);
    }

    for source in actions {
        if preferences.pinned.iter().any(|key| key == &source.key) {
            continue;
        }
        match source.kind {
            NewSessionActionKind::Terminal => sections.terminal.push(source.clone()),
            NewSessionActionKind::Conversation => sections.conversation.push(source.clone()),
        }
    }

    sections
}

/// 按 key 找回动作。移动端只回传 key，命令在本机解析——手机永远不决定跑什么。
pub fn find_action<'a>(actions: &'a [NewSessionAction], key: &str) -> Option<&'a NewSessionAction> {
    actions.iter().find(|action| action.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::ConversationLaunchSpec;

    fn option(id: &str, kind: ConversationAgentKind, label: &str) -> AcpAgentOption {
        AcpAgentOption {
            id: id.into(),
            kind: kind.id().into(),
            label: label.into(),
            profile: id.starts_with("profile:"),
            agent_definition_id: id
                .strip_prefix("agent:")
                .map(str::to_string)
                .filter(|id| !id.is_empty()),
            launch: ConversationLaunchSpec::from_command(kind.default_cmd()),
            history_dir: None,
        }
    }

    fn entry(label: &str, command: &str, provider: &str) -> LaunchEntry {
        LaunchEntry {
            label: label.into(),
            command: command.into(),
            provider: Some(provider.into()),
        }
    }

    fn labels(actions: &[NewSessionAction]) -> Vec<String> {
        actions
            .iter()
            .map(|action| format!("{} · {}", action.label, action.kind.label()))
            .collect()
    }

    #[test]
    fn conversation_and_terminal_are_separate_actions_for_the_same_agent() {
        let actions = new_session_actions(
            &[option(
                "claude",
                ConversationAgentKind::Claude,
                "Claude Code",
            )],
            &[entry(
                "Claude Code",
                "claude --dangerously-skip-permissions",
                "claude",
            )],
            None,
        );
        assert_eq!(
            labels(&actions),
            vec![
                "Claude Code · 终端".to_string(),
                "空白终端 · 终端".to_string(),
                "Claude Code · 对话".to_string(),
            ]
        );
        assert_ne!(actions[0].key, actions[2].key);
    }

    #[test]
    fn custom_launch_entries_sort_after_the_registry_order() {
        let actions = new_session_actions(
            &[],
            &[
                entry("我的脚本", "./run.sh", ""),
                entry(
                    "Antigravity",
                    "agy --dangerously-skip-permissions",
                    "antigravity",
                ),
                entry(
                    "Claude Code",
                    "claude --dangerously-skip-permissions",
                    "claude",
                ),
            ],
            None,
        );
        assert_eq!(
            actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Claude Code", "Antigravity", "我的脚本", "空白终端"]
        );
    }

    #[test]
    fn blank_and_empty_entries_never_collapse_into_each_other() {
        let actions = new_session_actions(
            &[],
            &[
                entry("", "claude", "claude"),
                entry("没有命令", "  ", "claude"),
            ],
            None,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, BLANK_TERMINAL_KEY);
    }

    #[test]
    fn user_agents_with_the_same_engine_stay_distinct() {
        let actions = new_session_actions(
            &[
                option("agent:quant", ConversationAgentKind::Pi, "量化智能体"),
                option("agent:lark", ConversationAgentKind::Pi, "飞书助理"),
                option("pi", ConversationAgentKind::Pi, "Pi"),
                option("profile:work", ConversationAgentKind::Pi, "工作区 Pi"),
            ],
            &[],
            None,
        );
        let keys: Vec<_> = actions
            .iter()
            .filter(|action| action.kind == NewSessionActionKind::Conversation)
            .map(|action| action.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "conversation:agent:quant",
                "conversation:agent:lark",
                "conversation:pi",
                "conversation:pi:profile:work",
            ]
        );
    }

    #[test]
    fn pinned_actions_move_out_of_their_kind_section_and_keep_pin_order() {
        let actions = new_session_actions(
            &[
                option("claude", ConversationAgentKind::Claude, "Claude Code"),
                option("grok", ConversationAgentKind::Grok, "Grok"),
            ],
            &[entry("Claude Code", "claude", "claude")],
            None,
        );
        let preferences = NewSessionPreferences {
            pinned: vec![
                "missing".into(),
                "conversation:grok".into(),
                BLANK_TERMINAL_KEY.into(),
            ],
        };
        let sections = build_sections(&actions, &preferences);
        assert_eq!(
            sections
                .common
                .iter()
                .map(|action| action.key.as_str())
                .collect::<Vec<_>>(),
            vec!["conversation:grok", BLANK_TERMINAL_KEY]
        );
        assert!(sections.common.iter().all(|action| action.pinned));
        assert!(
            sections
                .terminal
                .iter()
                .all(|action| action.key != BLANK_TERMINAL_KEY)
        );
        assert!(
            sections
                .conversation
                .iter()
                .all(|action| action.key != "conversation:grok")
        );
    }

    #[test]
    fn undetected_agents_and_clis_drop_out_of_the_catalog() {
        use crate::acp_conn::RuntimeExecutable;
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/usr/bin/claude".into()),
                ..Default::default()
            },
        );
        diagnostics.record_terminal(TerminalAgentKind::Grok, RuntimeExecutable::default());
        let actions = new_session_actions(
            &[
                option("claude", ConversationAgentKind::Claude, "Claude Code"),
                option("grok", ConversationAgentKind::Grok, "Grok"),
            ],
            &[
                entry("Claude Code", "claude", "claude"),
                entry("Grok", "grok", "grok"),
                entry("我的脚本", "./run.sh", ""),
            ],
            Some(&diagnostics),
        );
        assert_eq!(
            actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Claude Code", "我的脚本", "空白终端", "Claude Code"]
        );
    }

    #[test]
    fn preferences_do_not_accept_the_legacy_versioned_shape() {
        let legacy = serde_json::json!({
            "version": 1,
            "pinned": ["conversation:claude"]
        });
        assert!(serde_json::from_value::<NewSessionPreferences>(legacy).is_err());
    }

    #[test]
    fn actions_serialize_with_a_flat_target_for_the_phone() {
        let actions = new_session_actions(
            &[option(
                "agent:quant",
                ConversationAgentKind::Pi,
                "量化智能体",
            )],
            &[entry("Claude Code", "claude", "claude")],
            None,
        );
        let json = serde_json::to_value(&actions).unwrap();
        assert_eq!(json[0]["target"], "terminal");
        assert_eq!(json[0]["kind"], "terminal");
        assert_eq!(json[0]["command"], "claude");
        assert_eq!(json[1]["target"], "blankTerminal");
        assert_eq!(json[2]["target"], "conversation");
        assert_eq!(json[2]["agentOptionId"], "agent:quant");
        assert_eq!(json[2]["agentKind"], "pi");
    }
}

#[cfg(test)]
mod runtime_detection_tests {
    use super::{acp_agent_detected, launch_entry_detected, terminal_agent_detected};
    use crate::acp_conn::{AcpRuntimeDiagnostics, RuntimeExecutable};
    use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};

    fn available(program: &str) -> RuntimeExecutable {
        RuntimeExecutable {
            program: program.to_string(),
            path: Some(format!("/usr/bin/{program}")),
            version: Some("1.0.0".into()),
            error: None,
        }
    }

    #[test]
    fn unprobed_runtime_shows_every_builtin_agent() {
        for kind in TerminalAgentKind::ALL {
            assert!(
                terminal_agent_detected(kind, None),
                "{} 在探测完成前应先显示",
                kind.id()
            );
        }
        for kind in ConversationAgentKind::ALL {
            assert!(acp_agent_detected(kind, None));
        }
        assert!(launch_entry_detected(
            "claude --dangerously-skip-permissions",
            None
        ));
    }

    /// dsh 没有 CLI，但有一个确定的入口文件——探测结果必须能说"没搭好"。
    ///
    /// 这里曾经无条件返回 true：设置页显示就绪，启动却停在"启动中"不动，
    /// 真正的原因（运行时目录不存在）一个字也到不了用户面前。
    #[test]
    fn dsh_reports_missing_managed_runtime() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        assert!(
            !acp_agent_detected(ConversationAgentKind::Dsh, Some(&diagnostics)),
            "受管运行时缺失时 dsh 必须显示成未就绪"
        );

        diagnostics.record_agent(ConversationAgentKind::Dsh, available("dsh-acp-rich"));
        assert!(
            acp_agent_detected(ConversationAgentKind::Dsh, Some(&diagnostics)),
            "运行时就位后 dsh 必须显示成已就绪"
        );
    }

    /// dsh 的就绪与否只由它自己的探测决定，不受别家 CLI 装没装影响。
    #[test]
    fn dsh_detection_is_independent_of_terminal_clis() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(TerminalAgentKind::Claude, available("claude"));
        diagnostics.record_terminal(TerminalAgentKind::Codex, available("codex"));
        assert!(!acp_agent_detected(
            ConversationAgentKind::Dsh,
            Some(&diagnostics)
        ));
    }

    #[test]
    fn built_in_pi_dialog_runtime_is_independent_of_pi_terminal_cli() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_agent(ConversationAgentKind::Pi, available("smelt-pi-agent"));

        assert!(acp_agent_detected(
            ConversationAgentKind::Pi,
            Some(&diagnostics)
        ));
        assert!(
            !terminal_agent_detected(TerminalAgentKind::Pi, Some(&diagnostics)),
            "内置 Pi 对话 runtime 不应伪装成用户已安装 Pi 终端 CLI"
        );
    }

    #[test]
    fn probed_runtime_only_shows_installed_clis() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(TerminalAgentKind::Grok, available("grok"));
        diagnostics.record_terminal(TerminalAgentKind::Cursor, available("cursor-agent"));

        assert!(acp_agent_detected(
            ConversationAgentKind::Grok,
            Some(&diagnostics)
        ));
        assert!(acp_agent_detected(
            ConversationAgentKind::Cursor,
            Some(&diagnostics)
        ));
        assert!(!acp_agent_detected(
            ConversationAgentKind::Claude,
            Some(&diagnostics)
        ));
        assert!(!terminal_agent_detected(
            TerminalAgentKind::Antigravity,
            Some(&diagnostics)
        ));
        assert!(launch_entry_detected(
            "grok --always-approve",
            Some(&diagnostics)
        ));
        assert!(!launch_entry_detected(
            "claude --dangerously-skip-permissions",
            Some(&diagnostics)
        ));
        assert!(
            launch_entry_detected("zsh -l", Some(&diagnostics)),
            "对不上内置 agent 的自定义命令应始终显示"
        );
    }
}
