//! 应用的 svg 资源，两种路径：
//!
//! - `icons/<名字>.svg`：界面图标，见 [`IconName`]。插件（包括 JS 插件）按这个路径取，只有收进
//!   来的那些，没收的那一格空着。
//! - `smelt-icons/agent-<id>.svg`：智能体身份图标，路径由 `smelt_model::agent_kind` 的
//!   `icon_path` 拼，这里按同一个名字登记。都是第三方品牌资源，出处见
//!   `assets/icons/THIRD_PARTY_NOTICES.md`。
//!
//! 应用和离屏快照都把 [`Assets`] 交给 GPUI 当资源表。

use std::borrow::Cow;

use gpui::SharedString;

use crate::IconName;

/// 智能体图标的资源路径前缀。
pub const PREFIX: &str = "smelt-icons/";

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/icons/", $name)).as_slice())),*]
    };
}

const AGENT_ICONS: &[(&str, &[u8])] = icons![
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
];

/// 资源路径对应的字节；不是这里的资源返回 `None`。
pub fn load(path: &str) -> Option<&'static [u8]> {
    if let Some(icon) = IconName::from_asset_path(path) {
        return Some(icon.svg());
    }
    let name = path.strip_prefix(PREFIX)?;
    AGENT_ICONS
        .iter()
        .find(|(icon, _)| *icon == name)
        .map(|(_, bytes)| *bytes)
}

/// 所有资源路径。
fn paths() -> impl Iterator<Item = SharedString> {
    let icons = IconName::ALL
        .iter()
        .map(|icon| SharedString::new_static(icon.asset_path()));
    let agents = AGENT_ICONS
        .iter()
        .map(|(name, _)| SharedString::from(format!("{PREFIX}{name}")));
    icons.chain(agents)
}

/// 交给 GPUI 的资源表。
pub struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(load(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(paths().filter(|asset| asset.starts_with(path)).collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;
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
    fn every_listed_path_loads() {
        let listed = Assets.list("").unwrap();
        assert_eq!(listed.len(), IconName::ALL.len() + AGENT_ICONS.len());
        for path in &listed {
            assert!(
                Assets.load(path).unwrap().is_some(),
                "{path} 列出来了却取不到"
            );
        }
        assert!(Assets.list("icons/").unwrap().len() == IconName::ALL.len());
    }

    #[test]
    fn unknown_paths_are_not_claimed() {
        assert!(load("smelt-icons/agent-nope.svg").is_none());
        assert!(load("icons/nope.svg").is_none());
        assert!(load("trash.svg").is_none());
        assert!(load("smelt-icons/trash.svg").is_none());
        assert!(load("icons/trash.svg").is_some());
        assert!(load("smelt-icons/agent-claude.svg").is_some());
    }
}
