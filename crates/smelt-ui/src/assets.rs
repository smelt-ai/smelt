//! smelt 自带的 svg 图标，资源路径带 `smelt-icons/` 前缀：gpui-kit 图标集里没有的那几枚
//! （智能体身份、git 分支、机器系统、停靠区开合等）。组件、插件按路径取用；应用的资源
//! 加载先问这里，再问 gpui-kit 的全套图标。
//!
//! 智能体身份图标按稳定的存档 id 起名（`agent-<id>.svg`），路径由
//! `smelt_model::agent_kind` 的 `icon_path` 拼，这里按同一个名字登记。第三方图标的出处
//! 见 `assets/icons/THIRD_PARTY_NOTICES.md`。

/// 这些图标的资源路径前缀。
pub const PREFIX: &str = "smelt-icons/";

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/icons/", $name)).as_slice())),*]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "agent-antigravity.svg",
    "agent-claude.svg",
    "agent-codex.svg",
    "agent-copilot.svg",
    "agent-crush.svg",
    "agent-cursor.svg",
    "agent-custom.svg",
    "agent-grok.svg",
    "agent-kiro.svg",
    "agent-omp.svg",
    "agent-opencode.svg",
    "agent-pi.svg",
    "git-branch.svg",
    "machine-linux.svg",
    "machine-windows.svg",
    "monitor.svg",
    // 停靠区开合按钮的展开态：分隔线外侧加实心色块。gpui-kit 的 panel-left / panel-right
    // 只有一根细线，18px 下开合两态分不清。
    "panel-left-filled.svg",
    "panel-right-filled.svg",
    "square-pen.svg",
    "trash.svg",
];

/// 资源路径对应的图标字节；不是这里的图标返回 `None`。
pub fn load(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix(PREFIX)?;
    ICONS
        .iter()
        .find(|(icon, _)| *icon == name)
        .map(|(_, bytes)| *bytes)
}

#[cfg(test)]
mod tests {
    use smelt_model::agent_kind::{ConversationAgentKind, TerminalAgentKind};

    use super::*;

    /// 图标按 id 拼路径，新增一家智能体忘了登记不会报错，只是图标那一格空着。
    #[test]
    fn every_agent_has_an_icon() {
        let terminal = TerminalAgentKind::ALL.iter().map(|kind| kind.icon_asset());
        // 通用 ACP 不在 ALL 里，但它的会话照样要有图标可画。
        let conversation = ConversationAgentKind::ALL
            .into_iter()
            .chain([ConversationAgentKind::Custom])
            .map(|kind| kind.icon_asset());
        for path in terminal.chain(conversation) {
            assert!(
                load(&path).is_some_and(|bytes| !bytes.is_empty()),
                "{path} 没有登记图标"
            );
        }
    }

    #[test]
    fn unknown_paths_are_not_claimed() {
        assert!(load("smelt-icons/agent-nope.svg").is_none());
        assert!(load("trash.svg").is_none());
        assert!(load("icons/trash.svg").is_none());
        assert!(load("smelt-icons/trash.svg").is_some());
    }
}
