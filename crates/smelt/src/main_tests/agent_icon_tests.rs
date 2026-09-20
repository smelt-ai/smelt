use smelt_core::agent_kind::{ConversationAgentKind, TerminalAgentKind};

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
    for kind in ConversationAgentKind::ALL {
        let path = format!("smelt-icons/agent-{}.svg", kind.id());
        assert!(
            crate::agent_icon_svg(&path).is_some(),
            "ACP agent {} 没有登记侧栏图标：{path}",
            kind.id()
        );
    }
}

#[test]
fn non_agent_paths_are_not_claimed() {
    assert!(crate::agent_icon_svg("smelt-icons/not-an-agent.svg").is_none());
    assert!(crate::agent_icon_svg("smelt-icons/git-branch.svg").is_none());
    assert!(crate::agent_icon_svg("smelt-icons/agent-nope.svg").is_none());
}
