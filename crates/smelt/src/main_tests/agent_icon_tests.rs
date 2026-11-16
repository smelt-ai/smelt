use smelt_model::agent_kind::{ConversationAgentKind, TerminalAgentKind};

use super::resolve_terminal_provider;

#[test]
fn explicit_launch_provider_wins_and_bare_terminal_uses_hook_provider() {
    assert_eq!(
        resolve_terminal_provider(Some(TerminalAgentKind::Claude), Some("codex")),
        Some(TerminalAgentKind::Claude)
    );
    assert_eq!(
        resolve_terminal_provider(None, Some("codex")),
        Some(TerminalAgentKind::Codex)
    );
    assert_eq!(resolve_terminal_provider(None, Some("unknown")), None);
}

#[test]
fn every_agent_has_a_sidebar_icon() {
    for kind in TerminalAgentKind::ALL {
        let path = format!("smelt-icons/agent-{}.svg", kind.id());
        assert!(
            crate::agent_icon_svg(&path).is_some(),
            "终端 agent {} 没有登记侧栏图标：{path}",
            kind.id()
        );
    }
    // 通用 ACP 不在 ALL 里，但它的会话照样要有图标可画。
    for kind in ConversationAgentKind::ALL
        .into_iter()
        .chain([ConversationAgentKind::Custom])
    {
        let path = format!("smelt-icons/agent-{}.svg", kind.id());
        assert!(
            crate::agent_icon_svg(&path).is_some(),
            "ACP agent {} 没有登记侧栏图标：{path}",
            kind.id()
        );
    }
}

#[test]
fn oh_my_pi_sidebar_icon_is_registered() {
    let bytes = crate::agent_icon_svg("smelt-icons/agent-omp.svg")
        .expect("Oh My Pi 的侧栏图标必须登记");
    assert!(!bytes.is_empty());
}

#[test]
fn non_agent_paths_are_not_claimed() {
    assert!(crate::agent_icon_svg("smelt-icons/not-an-agent.svg").is_none());
    assert!(crate::agent_icon_svg("smelt-icons/git-branch.svg").is_none());
    assert!(crate::agent_icon_svg("smelt-icons/trash.svg").is_none());
    assert!(crate::agent_icon_svg("smelt-icons/agent-nope.svg").is_none());
}

#[test]
fn machine_os_icons_are_bundled_with_the_application() {
    use gpui::AssetSource as _;

    for path in [
        "smelt-icons/machine-linux.svg",
        "smelt-icons/machine-windows.svg",
    ] {
        let icon = crate::SmeltAssets
            .load(path)
            .unwrap()
            .expect("机器系统图标必须打包进应用资源");
        assert!(!icon.is_empty());
    }
}

#[test]
fn archive_sidebar_icon_is_bundled_with_the_application() {
    use gpui::AssetSource as _;

    let path = gpui_kit_assets::IconName::Archive.path();
    let icon = crate::SmeltAssets
        .load(path.as_ref())
        .unwrap()
        .expect("归档分组的图标必须打包进应用资源");
    assert!(!icon.is_empty());
}
