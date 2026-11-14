use super::*;
use api::model::agent_kind::TerminalAgentKind;
use api::model::daemon_state::DaemonPhase;
use api::model::workspace_directory::{WorkspaceAcp, WorkspaceTerminal};

fn state(id: &str, phase: DaemonPhase) -> DaemonSessionState {
    DaemonSessionState {
        id: id.to_string(),
        phase,
        structured_events: true,
        turn_events: true,
        ..Default::default()
    }
}

fn states(list: &[(&str, DaemonPhase)]) -> HashMap<String, DaemonSessionState> {
    list.iter()
        .map(|(id, phase)| (id.to_string(), state(id, *phase)))
        .collect()
}

fn terminal(id: Option<&str>) -> WorkspaceTerminal {
    WorkspaceTerminal {
        id: id.map(str::to_string),
        ..Default::default()
    }
}

fn terminals(ids: &[Option<&str>]) -> WorkspaceConversation {
    WorkspaceConversation {
        terminals: ids.iter().map(|id| terminal(*id)).collect(),
        ..Default::default()
    }
}

fn acp(sid: &str, agent: &str) -> WorkspaceConversation {
    WorkspaceConversation {
        acp: Some(WorkspaceAcp {
            sid: sid.to_string(),
            agent: agent.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn row(ids: &[&str], statuses: &[AgentStatus], title: &str) -> Row {
    Row {
        ids: ids.iter().map(|id| id.to_string()).collect(),
        statuses: statuses.to_vec(),
        title: title.to_string(),
        agent: None,
    }
}

/// 两张表都要覆盖：一家 agent 可能只走 ACP 或只走终端（antigravity）。
#[test]
fn every_agent_has_a_menu_logo() {
    for id in TerminalAgentKind::ALL
        .iter()
        .map(|kind| kind.id())
        .chain(ConversationAgentKind::ALL.iter().map(|kind| kind.id()))
    {
        assert_ne!(logo(Some(id)), TERMINAL_PNG, "agent {id} 没有菜单栏母图");
    }
    assert_eq!(logo(None), TERMINAL_PNG);
    assert_eq!(logo(Some("自定义")), TERMINAL_PNG);
}

#[test]
fn title_matches_the_compact_attention_and_running_format() {
    assert_eq!(status_title(0, 0), "");
    assert_eq!(status_title(5, 0), "5@");
    assert_eq!(status_title(0, 660), "660");
    assert_eq!(status_title(5, 660), "5@·660");
    assert_eq!(status_tooltip(0, 0), "smelt");
    assert_eq!(status_tooltip(5, 660), "5 个会话需要关注，660 个会话运行中");
}

#[test]
fn terminals_never_started_are_not_sessions_yet() {
    assert_eq!(
        conversation_row(&terminals(&[None]), &HashMap::new(), None),
        None
    );
    let row = conversation_row(&terminals(&[None, Some("b")]), &HashMap::new(), None).unwrap();
    assert_eq!(row.ids, vec!["b".to_string()]);
}

#[test]
fn terminal_status_name_and_agent() {
    let mut conversation = terminals(&[Some("a"), Some("b")]);
    conversation.custom_title = Some("整段的名字".into());
    conversation.terminals[0].launch_cmd = Some("claude".into());
    let row = conversation_row(
        &conversation,
        &states(&[("a", DaemonPhase::Idle), ("b", DaemonPhase::Thinking)]),
        Some("远程机"),
    )
    .unwrap();
    assert_eq!(row.status(), AgentStatus::Running);
    assert_eq!(row.title, "整段的名字 · 远程机");
    assert!(agent_id_from_launch("claude").is_some());
    assert_eq!(row.agent.as_deref(), agent_id_from_launch("claude"));

    // 和侧栏同一条规则：对话的用户命名 > 会话名 > 只有一格时的格子名。
    let mut single = terminals(&[Some("a")]);
    single.terminals[0].custom_title = Some("格子名".into());
    let mut reported = states(&[("a", DaemonPhase::Idle)]);
    reported.get_mut("a").unwrap().provider = Some("codex".into());
    let row = conversation_row(&single, &reported, None).unwrap();
    assert_eq!(row.title, "格子名");
    // 没有启动命令就认 hook 报的。
    assert_eq!(row.agent.as_deref(), Some("codex"));
    single.custom_title = Some("会话名".into());
    assert_eq!(
        conversation_row(&single, &reported, None).unwrap().title,
        "会话名"
    );
    reported.get_mut("a").unwrap().custom_title = Some("对话名".into());
    assert_eq!(
        conversation_row(&single, &reported, None).unwrap().title,
        "对话名"
    );
}

#[test]
fn conversation_name_falls_back_to_the_agent() {
    let mut conversation = acp("s", "claude");
    let row = conversation_row(&conversation, &HashMap::new(), None).unwrap();
    assert_eq!(row.ids, vec!["s".to_string()]);
    assert_eq!(row.agent.as_deref(), Some("claude"));
    assert!(row.title.ends_with("对话"), "{}", row.title);

    conversation.acp.as_mut().unwrap().session_title = Some("修 bug".into());
    let row = conversation_row(&conversation, &HashMap::new(), None).unwrap();
    assert_eq!(row.title, "修 bug");

    let mut live = states(&[("s", DaemonPhase::AwaitingApproval)]);
    live.get_mut("s").unwrap().title = Some("运行时报的名字".into());
    let row = conversation_row(&conversation, &live, None).unwrap();
    assert_eq!(row.title, "运行时报的名字");
    assert_eq!(row.status(), AgentStatus::NeedsYou);
}

#[test]
fn menu_lists_active_and_raised_sessions_needing_you_first() {
    let rows = vec![
        row(&["idle"], &[AgentStatus::Idle], "空闲"),
        row(&["run"], &[AgentStatus::Running], "在跑"),
        row(&["unread"], &[AgentStatus::Idle], "没看"),
        row(&["ask"], &[AgentStatus::Running], "等批准"),
    ];
    let attention = HashMap::from([
        ("unread".to_string(), false),
        ("ask".to_string(), true),
        ("closed".to_string(), false),
    ]);
    let menu = menu(rows, &attention);
    let labels: Vec<_> = menu
        .entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["等批准 — 需要你", "在跑 — 运行中", "没看 — 未读"]
    );
    assert_eq!(
        menu.entries[0].color,
        agent_status_u32(AgentStatus::NeedsYou)
    );
    // 已经关掉的对话的提醒也算在数里。
    assert_eq!(menu.title, "3@·2");
}

#[test]
fn split_terminal_jumps_to_the_pane_that_raised_attention_then_the_busy_one() {
    let statuses = [AgentStatus::Idle, AgentStatus::Running];
    let busy = menu(
        vec![row(&["left", "right"], &statuses, "分屏")],
        &HashMap::new(),
    );
    assert_eq!(busy.entries[0].session_id, "right");

    let raised = menu(
        vec![row(&["left", "right"], &statuses, "分屏")],
        &HashMap::from([("left".to_string(), false)]),
    );
    assert_eq!(raised.entries[0].session_id, "left");
}

#[test]
fn status_menu_ends_with_opening_the_main_window() {
    let empty = menu(Vec::new(), &HashMap::new()).status_menu();
    assert_eq!(empty.title, "");
    assert!(matches!(
        empty.items.as_slice(),
        [StatusMenuItem::Action { command, .. }] if command == api::SHOW_MAIN_WINDOW.id()
    ));

    let listed = menu(
        vec![row(&["run"], &[AgentStatus::Running], "在跑")],
        &HashMap::new(),
    )
    .status_menu();
    assert_eq!(listed.items.len(), 3);
    let StatusMenuItem::Action {
        command,
        args,
        icon,
        ..
    } = &listed.items[0]
    else {
        panic!("第一项是会话");
    };
    assert_eq!(command, sessions::ACTIVATE.id());
    assert_eq!(args["session_id"], "run");
    assert_eq!(icon.as_ref().unwrap().png, TERMINAL_PNG);
    assert_eq!(listed.items[1], StatusMenuItem::Separator);
}
