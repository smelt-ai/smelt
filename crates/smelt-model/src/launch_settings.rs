//! 快捷启动项的数据和内置项规则。存取在运行时。

use serde::{Deserialize, Serialize};
use crate::agent_kind::TerminalAgentKind;
use crate::new_session::{LaunchEntry, default_launch_entries};

/// 启动项迁移版本。旧出厂命令与标签只在对应版本升级时改写；固定内置项则在
/// 每次加载时与注册表对齐。
pub const LAUNCH_CONFIG_VERSION: u32 = 14;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchSettings {
    pub version: u32,
    /// 除固定的「新建终端」「新建 Worktree…」外，下拉菜单里的启动项。
    pub entries: Vec<LaunchEntry>,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            version: LAUNCH_CONFIG_VERSION,
            entries: default_launch_entries(),
        }
    }
}

/// 内置启动项由 provider 注册信息识别；旧配置缺少 provider 时，只把 Smelt
/// 发布过的逐字默认命令视为内置，避免把用户自定义的 Agent 命令误锁定。
fn launch_entry_matches_builtin(entry: &LaunchEntry, agent: TerminalAgentKind) -> bool {
    entry.provider.as_deref() == Some(agent.id())
        || (entry.provider.is_none()
            && (entry.command.trim() == agent.quick_terminal_cmd()
                || agent
                    .upgrade_released_quick_terminal_command(&entry.command)
                    .is_some()))
}

pub fn launch_entry_is_builtin(entry: &LaunchEntry) -> bool {
    entry
        .provider
        .as_deref()
        .and_then(TerminalAgentKind::from_id)
        .is_some()
}

/// 固定内置启动项与注册表对齐，并给旧版无 provider 的逐字出厂项补上稳定标识。
/// 返回 true 表示配置发生变化，需要持久化。
pub fn ensure_builtin_launch_entries(config: &mut LaunchSettings) -> bool {
    let mut changed = false;
    for agent in TerminalAgentKind::ALL {
        if let Some(entry) = config
            .entries
            .iter_mut()
            .find(|entry| launch_entry_matches_builtin(entry, agent))
        {
            if entry.provider.is_none() {
                entry.provider = Some(agent.id().to_string());
                changed = true;
            }
            continue;
        }
        config.entries.push(LaunchEntry {
            label: agent.quick_terminal_label().to_string(),
            command: agent.quick_terminal_cmd().to_string(),
            provider: Some(agent.id().to_string()),
        });
        changed = true;
    }
    changed
}
