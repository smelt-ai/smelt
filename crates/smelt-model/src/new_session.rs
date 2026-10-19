//! 新建菜单：可选的动作、分组规则、常用项偏好的数据。读配置和偏好在运行时。

use crate::acp_conn::AcpRuntimeDiagnostics;
use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use crate::session_control::AcpAgentOption;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    BlankTerminal,
}

/// 新建选择器里的一行。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
}

/// 三个分组。常用不额外占一层，和终端 / 对话同级。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PinParams {
    pub key: String,
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
