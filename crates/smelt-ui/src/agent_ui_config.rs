//! 审批通知 / ACP 命令等 agent UI 偏好（`~/.smelt/agent_ui.json`）。数据模型 +
//! 持久化住在这（需要 `gpui::Global`，所以不能挪进不许引 GPUI 的 smelt-core），
//! 设置页怎么渲染这些字段仍在主 crate 的 settings.rs——UI 和数据分层。

use gpui::{App, Global};

use smelt_core::agent_kind::{
    AcpAgentKind, AcpLaunchSpec, AcpProfile, default_acp_cmd, default_acp_codex_cmd,
    default_acp_copilot_cmd, default_acp_grok_cmd,
};

fn default_true() -> bool {
    true
}

const LEGACY_CODEX_ACP_CMD: &str = "bunx --bun @zed-industries/codex-acp@0.16.0";
const PREVIOUS_CODEX_ACP_CMD: &str = "bunx --bun @agentclientprotocol/codex-acp@1.1.7";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentUiConfig {
    #[serde(default = "default_true", alias = "notify_awaiting")]
    pub notify_approval: bool,
    #[serde(default = "default_true")]
    pub notify_input: bool,
    #[serde(default = "default_true")]
    pub notify_success: bool,
    #[serde(default = "default_true")]
    pub notify_failure: bool,
    #[serde(default = "default_true")]
    pub notify_terminal_bell: bool,
    /// Claude ACP 会话的 agent 启动命令（空白分词）。默认 Claude 官方适配器；权限门
    /// 保留——结构化审批正是这条通道的卖点，别在这里加 bypass 类参数。
    ///
    /// 字段名没跟着 `AcpAgentKind` 改成 `acp_claude_cmd`：老配置文件里就叫这个，
    /// 改名等于把用户自定义过的命令悄悄重置回默认。
    #[serde(default = "default_acp_cmd")]
    pub acp_cmd: String,
    /// GitHub Copilot ACP 会话的启动命令。
    #[serde(default = "default_acp_copilot_cmd")]
    pub acp_copilot_cmd: String,
    /// Codex 原生 app-server 的启动命令。字段名为兼容旧配置保留。
    #[serde(default = "default_acp_codex_cmd")]
    pub acp_codex_cmd: String,
    /// Grok ACP 会话的启动命令。
    #[serde(default = "default_acp_grok_cmd")]
    pub acp_grok_cmd: String,
    /// 手动添加的 workspace（同一家 agent 可以有好几个，比如 Claude 的默认
    /// `.claude` 和自定义的 `.claude-quant` 并存）。四个基础 agent 槽位不变、
    /// 走各自默认路径；这里只装"额外"的。
    #[serde(default)]
    pub profiles: Vec<AcpProfile>,
}

impl AgentUiConfig {
    /// 某个 agent 种类当前生效的启动命令。
    pub fn acp_cmd_for(&self, agent: AcpAgentKind) -> String {
        match agent {
            AcpAgentKind::Claude => self.acp_cmd.clone(),
            AcpAgentKind::Copilot => self.acp_copilot_cmd.clone(),
            AcpAgentKind::Codex => self.acp_codex_cmd.clone(),
            AcpAgentKind::Grok => self.acp_grok_cmd.clone(),
        }
    }

    /// 改某个 agent 的启动命令（设置页三条输入框共用）。
    pub fn set_acp_cmd_for(&mut self, agent: AcpAgentKind, cmd: String) {
        match agent {
            AcpAgentKind::Claude => self.acp_cmd = cmd,
            AcpAgentKind::Copilot => self.acp_copilot_cmd = cmd,
            AcpAgentKind::Codex => self.acp_codex_cmd = cmd,
            AcpAgentKind::Grok => self.acp_grok_cmd = cmd,
        }
    }

    pub fn find_profile(&self, id: &str) -> Option<&AcpProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn profile_launch_spec(&self, profile: &AcpProfile) -> AcpLaunchSpec {
        let mut launch = profile.launch_spec();
        launch.command = self.acp_cmd_for(profile.kind());
        launch
    }
}

impl Default for AgentUiConfig {
    fn default() -> Self {
        Self {
            notify_approval: true,
            notify_input: true,
            notify_success: true,
            notify_failure: true,
            notify_terminal_bell: true,
            acp_cmd: default_acp_cmd(),
            acp_copilot_cmd: default_acp_copilot_cmd(),
            acp_codex_cmd: default_acp_codex_cmd(),
            acp_grok_cmd: default_acp_grok_cmd(),
            profiles: Vec::new(),
        }
    }
}

impl Global for AgentUiConfig {}

fn agent_ui_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("agent_ui.json"))
}

pub fn load_agent_ui_config() -> AgentUiConfig {
    let path = agent_ui_path();
    let legacy_value = path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let mut config: AgentUiConfig = smelt_core::json_store::load_json(path);
    let mut migrated = legacy_value
        .as_ref()
        .is_some_and(|value| migrate_legacy_notification_setting(&mut config, value));
    // 只迁移之前随应用发出的默认值；用户自定义命令不动。
    if migrate_legacy_codex_adapter(&mut config) {
        migrated = true;
    }
    if migrated {
        save_agent_ui_config(&config);
    }
    config
}

fn migrate_legacy_notification_setting(
    config: &mut AgentUiConfig,
    value: &serde_json::Value,
) -> bool {
    let Some(enabled) = value.get("notify_awaiting").and_then(|v| v.as_bool()) else {
        return false;
    };
    config.notify_approval = enabled;
    config.notify_input = enabled;
    true
}

fn migrate_legacy_codex_adapter(config: &mut AgentUiConfig) -> bool {
    if config.acp_codex_cmd != LEGACY_CODEX_ACP_CMD
        && config.acp_codex_cmd != PREVIOUS_CODEX_ACP_CMD
    {
        return false;
    }
    config.acp_codex_cmd = default_acp_codex_cmd();
    true
}

fn save_agent_ui_config(c: &AgentUiConfig) {
    smelt_core::json_store::save_json(agent_ui_path(), c);
}

pub fn apply_agent_ui(f: impl FnOnce(&mut AgentUiConfig), cx: &mut App) {
    let mut c = cx.global::<AgentUiConfig>().clone();
    f(&mut c);
    save_agent_ui_config(&c);
    cx.set_global(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_codex_default_is_replaced_by_current_adapter() {
        let mut config = AgentUiConfig::default();
        config.acp_codex_cmd = LEGACY_CODEX_ACP_CMD.into();

        assert!(migrate_legacy_codex_adapter(&mut config));

        assert_eq!(config.acp_codex_cmd, default_acp_codex_cmd());
    }

    #[test]
    fn previous_official_codex_adapter_is_replaced_by_app_server() {
        let mut config = AgentUiConfig::default();
        config.acp_codex_cmd = PREVIOUS_CODEX_ACP_CMD.into();
        assert!(migrate_legacy_codex_adapter(&mut config));
        assert_eq!(config.acp_codex_cmd, "codex app-server");
    }

    #[test]
    fn custom_codex_adapter_is_not_migrated() {
        let mut config = AgentUiConfig::default();
        config.acp_codex_cmd = "codex-acp --custom".into();

        assert!(!migrate_legacy_codex_adapter(&mut config));
        assert_eq!(config.acp_codex_cmd, "codex-acp --custom");
    }

    #[test]
    fn legacy_disabled_awaiting_setting_disables_both_wait_notifications() {
        let mut config = AgentUiConfig::default();
        let old = serde_json::json!({ "notify_awaiting": false });

        assert!(migrate_legacy_notification_setting(&mut config, &old));
        assert!(!config.notify_approval);
        assert!(!config.notify_input);
        assert!(config.notify_success);
        assert!(config.notify_failure);
    }

    #[test]
    fn profile_launch_uses_configured_agent_command_and_workspace_env() {
        let config = AgentUiConfig {
            acp_cmd: "claude-custom --acp".into(),
            ..AgentUiConfig::default()
        };
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: "~/Claude Workspaces/quant".into(),
        };

        let launch = config.profile_launch_spec(&profile);

        assert_eq!(launch.command, "claude-custom --acp");
        assert_eq!(
            launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }
}
