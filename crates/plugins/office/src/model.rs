//! 从运行时快照投影办公室。状态可信度和分屏优先级沿用共享模型，不从标题猜活动。

use smelt_plugin_api::model::{
    agent_kind::{ConversationAgentKind, TerminalAgentKind},
    agent_status::AgentStatus,
    daemon_state::{DaemonPhase, DaemonSessionState},
    runtime_path::PathStyle,
    workspace_directory::{WorkspaceConversation, WorkspaceDirectory, project_root_of},
};
use smelt_plugin_api::{self as api, OpenView, RuntimeKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    Thinking,
    Executing,
    Approval,
    Input,
    Failed,
    Idle,
    Offline,
}

impl Activity {
    pub(crate) fn running(self) -> bool {
        matches!(self, Self::Thinking | Self::Executing)
    }

    pub(crate) fn needs_you(self) -> bool {
        matches!(self, Self::Approval | Self::Input | Self::Failed)
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::Thinking => rust_i18n::t!("status.thinking"),
            Self::Executing => rust_i18n::t!("status.executing"),
            Self::Approval => rust_i18n::t!("status.approval"),
            Self::Input => rust_i18n::t!("status.input"),
            Self::Failed => rust_i18n::t!("status.failed"),
            Self::Idle => rust_i18n::t!("status.idle"),
            Self::Offline => rust_i18n::t!("status.offline"),
        }
        .into_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Worker {
    pub(crate) conversation: String,
    pub(crate) title: String,
    pub(crate) provider: String,
    pub(crate) icon: Option<String>,
    pub(crate) activity: Activity,
    /// 只由权威终态触发完成动作；空闲/丢失运行时不能冒充成功。
    pub(crate) completed: bool,
    pub(crate) detail: Option<String>,
    pub(crate) look: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Room {
    pub(crate) machine: RuntimeKey,
    pub(crate) machine_name: String,
    pub(crate) root: String,
    pub(crate) name: String,
    pub(crate) unavailable: Option<String>,
    pub(crate) workers: Vec<Worker>,
}

/// 事件到达时调用；render 不取整份目录。
pub(crate) fn rooms_for_machine(
    machine: &api::Machine,
    online: bool,
    style: PathStyle,
    directory: &WorkspaceDirectory,
    states: &[DaemonSessionState],
) -> Vec<Room> {
    let mut rooms: Vec<Room> = Vec::new();
    for conversation in &directory.conversations {
        // 与会话视图可打开的名册一致：自动化的后台运行由自动化插件承载。
        if conversation.archived_at.is_some()
            || conversation.acp.as_ref().is_some_and(|acp| {
                acp.automation_id.is_some()
                    || api::model::session_control::is_background_acp_session_id(&acp.sid)
            })
        {
            continue;
        }
        let cwd = conversation
            .acp
            .as_ref()
            .and_then(|acp| acp.cwd.as_deref())
            .or_else(|| conversation.terminals.first()?.cwd.as_deref())
            .unwrap_or_default();
        let root = conversation
            .workset_id
            .as_ref()
            .and_then(|id| directory.worksets.iter().find(|workset| &workset.id == id))
            .map(|workset| workset.project_root.as_str())
            .or_else(|| project_root_of(style, &directory.projects, cwd))
            .unwrap_or(cwd);
        let room = match rooms.iter().position(|room| room.root == root) {
            Some(index) => &mut rooms[index],
            None => {
                rooms.push(Room {
                    machine: machine.key.clone(),
                    machine_name: machine.name.clone(),
                    root: root.into(),
                    name: style
                        .file_name(root)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(root)
                        .into(),
                    unavailable: (!online).then(|| {
                        machine
                            .unavailable
                            .clone()
                            .unwrap_or_else(|| rust_i18n::t!("status.offline").into_owned())
                    }),
                    workers: Vec::new(),
                });
                rooms.last_mut().expect("刚加入的房间")
            }
        };
        room.workers.push(worker(conversation, states, online));
    }
    rooms
}

fn worker(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
    online: bool,
) -> Worker {
    let lead = conversation
        .session_ids()
        .next()
        .and_then(|id| states.iter().find(|state| state.id == id));
    let status = conversation.status(states);
    let relevant = conversation
        .states_in(states)
        .filter(|state| AgentStatus::from_daemon_state(state) == Some(status))
        .min_by_key(|state| match state.effective_phase() {
            DaemonPhase::Failed => 0,
            DaemonPhase::AwaitingApproval => 1,
            DaemonPhase::WaitingForUser => 2,
            DaemonPhase::ExecutingTool => 3,
            _ => 4,
        });
    let activity = if !online {
        Activity::Offline
    } else {
        match relevant.map(DaemonSessionState::effective_phase) {
            Some(DaemonPhase::Failed) => Activity::Failed,
            Some(DaemonPhase::AwaitingApproval) => Activity::Approval,
            Some(DaemonPhase::WaitingForUser) => Activity::Input,
            Some(DaemonPhase::Thinking) => Activity::Thinking,
            Some(DaemonPhase::ExecutingTool) => Activity::Executing,
            _ => Activity::Idle,
        }
    };
    let title = [
        lead.and_then(|state| state.conversation_name.as_deref()),
        conversation.custom_title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|title| !title.is_empty())
    .map(str::to_owned)
    .unwrap_or_else(|| rust_i18n::t!("session.untitled").into_owned());
    let (provider, icon) = providers(conversation, states);
    Worker {
        conversation: conversation.id.clone(),
        title,
        provider,
        icon,
        activity,
        completed: activity == Activity::Idle
            && conversation
                .states_in(states)
                .filter(|state| state.has_runtime() && state.phase_is_authoritative())
                .max_by_key(|state| state.phase_since)
                .is_some_and(|state| state.effective_phase() == DaemonPhase::Succeeded),
        detail: online
            .then(|| relevant.and_then(DaemonSessionState::pending_text))
            .flatten(),
        look: identity(&conversation.id),
    }
}

fn provider_label(id: &str) -> (String, Option<String>) {
    if let Some(kind) = ConversationAgentKind::from_id(id) {
        return (kind.label().into(), Some(kind.icon_asset()));
    }
    if let Some(kind) = TerminalAgentKind::from_id(id) {
        return (kind.label().into(), Some(kind.icon_asset()));
    }
    // 新 provider / 自定义协议仍有角色，认不出的身份原样显示。
    (id.into(), None)
}

fn providers(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
) -> (String, Option<String>) {
    if let Some(acp) = &conversation.acp {
        return provider_label(&acp.agent);
    }
    let mut providers = Vec::new();
    for terminal in &conversation.terminals {
        let state = terminal
            .id
            .as_ref()
            .and_then(|id| states.iter().find(|state| &state.id == id));
        let id = state
            .and_then(|state| state.provider.as_deref())
            .filter(|id| !id.is_empty())
            .or_else(|| {
                terminal
                    .launch_cmd
                    .as_deref()
                    .and_then(TerminalAgentKind::from_command_prefix)
                    .map(TerminalAgentKind::id)
            });
        if let Some(id) = id {
            let provider = provider_label(id);
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
    }
    let icon = if providers.len() == 1 {
        providers[0].1.clone()
    } else {
        None
    };
    let label = if providers.is_empty() {
        rust_i18n::t!("session.terminal").into_owned()
    } else {
        providers
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
            .join(" / ")
    };
    (label, icon)
}

/// FNV-1a：新增、移除或重排其它会话不改变同一角色的外观。
fn identity(id: &str) -> u64 {
    id.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

/// 约定：smelt.sessions 的 SESSION / SessionArgs；权威在 sessions/src/contract.rs。
pub(crate) fn open_request(machine: &RuntimeKey, conversation: &str) -> OpenView {
    OpenView {
        view: "smelt.sessions/session".into(),
        args: api::serde_json::json!({
            "machine": api::machine_to_json(machine),
            "conversation_id": conversation,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::model::workspace_directory::{WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset};

    fn conversation(id: &str, cwd: &str, agent: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(WorkspaceAcp {
                sid: format!("s-{id}"),
                cwd: Some(cwd.into()),
                agent: agent.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn machine() -> api::Machine {
        api::Machine {
            key: RuntimeKey::Local,
            name: "Mac".into(),
            unavailable: None,
        }
    }

    fn state(id: &str, phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: format!("s-{id}"),
            phase,
            turn_events: true,
            ..Default::default()
        }
    }

    #[test]
    fn all_authoritative_phases_and_stale_states_are_mapped() {
        let conversation = conversation("c", "/repo", "codex");
        for (phase, expected) in [
            (DaemonPhase::Thinking, Activity::Thinking),
            (DaemonPhase::ExecutingTool, Activity::Executing),
            (DaemonPhase::AwaitingApproval, Activity::Approval),
            (DaemonPhase::WaitingForUser, Activity::Input),
            (DaemonPhase::Failed, Activity::Failed),
            (DaemonPhase::Succeeded, Activity::Idle),
            (DaemonPhase::Connecting, Activity::Idle),
            (DaemonPhase::Dead, Activity::Idle),
        ] {
            assert_eq!(
                worker(&conversation, &[state("c", phase)], true).activity,
                expected
            );
        }
        let mut stale = state("c", DaemonPhase::Thinking);
        stale.turn_events = false;
        assert_eq!(
            worker(&conversation, &[stale.clone()], true).activity,
            Activity::Idle
        );
        stale.turn_events = true;
        stale.runtime = false;
        assert_eq!(
            worker(&conversation, &[stale], true).activity,
            Activity::Idle
        );
        assert_eq!(worker(&conversation, &[], true).activity, Activity::Idle);
        assert_eq!(
            worker(&conversation, &[state("c", DaemonPhase::Thinking)], false).activity,
            Activity::Offline
        );
    }

    #[test]
    fn groups_projects_worktrees_and_unbound_sessions_without_archives_or_automation() {
        let mut archived = conversation("old", "/repo", "claude");
        archived.archived_at = Some(1);
        let mut automation = conversation("auto", "/repo", "pi");
        automation.acp.as_mut().unwrap().automation_id = Some("a".into());
        let mut worktree = conversation("wt", "/elsewhere", "codex");
        worktree.workset_id = Some("wt".into());
        let directory = WorkspaceDirectory {
            projects: vec!["/repo".into(), "/repo/nested".into()],
            worksets: vec![WorkspaceWorkset {
                id: "wt".into(),
                project_root: "/repo".into(),
                ..Default::default()
            }],
            conversations: vec![
                conversation("a", "/repo/src", "claude"),
                conversation("b", "/repo/nested/src", "codex"),
                worktree,
                conversation("loose", "/repo-other", "pi"),
                archived,
                automation,
            ],
            ..Default::default()
        };
        let rooms = rooms_for_machine(&machine(), true, PathStyle::Posix, &directory, &[]);
        assert_eq!(
            rooms
                .iter()
                .map(|room| room.root.as_str())
                .collect::<Vec<_>>(),
            ["/repo", "/repo/nested", "/repo-other"]
        );
        assert_eq!(
            rooms[0]
                .workers
                .iter()
                .map(|worker| worker.conversation.as_str())
                .collect::<Vec<_>>(),
            ["a", "wt"]
        );
        let offline = rooms_for_machine(&machine(), false, PathStyle::Posix, &directory, &[]);
        assert!(offline.iter().all(|room| room.unavailable.is_some()));
        assert!(
            offline
                .iter()
                .flat_map(|room| &room.workers)
                .all(|worker| worker.activity == Activity::Offline)
        );
    }

    #[test]
    fn split_terminals_aggregate_attention_and_use_runtime_names() {
        let conversation = WorkspaceConversation {
            id: "c".into(),
            custom_title: Some("custom".into()),
            terminals: vec![
                WorkspaceTerminal {
                    id: Some("s-a".into()),
                    ..Default::default()
                },
                WorkspaceTerminal {
                    id: Some("s-b".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut lead = state("a", DaemonPhase::Thinking);
        lead.conversation_name = Some("Runtime title".into());
        lead.provider = Some("codex".into());
        let waiting = state("b", DaemonPhase::AwaitingApproval);
        let worker = worker(&conversation, &[lead, waiting], true);
        assert_eq!(worker.activity, Activity::Approval);
        assert_eq!(worker.title, "Runtime title");
        assert_eq!(worker.provider, "Codex");
    }

    #[test]
    fn windows_paths_and_machine_identity_are_preserved_for_navigation() {
        let directory = WorkspaceDirectory {
            projects: vec![r"C:\repo".into()],
            conversations: vec![conversation("c", r"C:\repo\src", "claude")],
            ..Default::default()
        };
        let mut machine = machine();
        machine.key = RuntimeKey::Remote("windows".into());
        let rooms = rooms_for_machine(&machine, true, PathStyle::Windows, &directory, &[]);
        assert_eq!(rooms[0].name, "repo");
        for machine in [RuntimeKey::Local, machine.key] {
            let request = open_request(&machine, "c");
            assert_eq!(request.view, "smelt.sessions/session");
            assert_eq!(request.args["machine"], api::machine_to_json(&machine));
            assert_eq!(request.args["conversation_id"], "c");
            assert!(request.args.get("session_id").is_none());
        }
    }

    #[test]
    fn identity_survives_reordering_and_removed_sessions_disappear() {
        let mut directory = WorkspaceDirectory {
            conversations: vec![
                conversation("a", "/repo", "claude"),
                conversation("b", "/repo", "codex"),
            ],
            ..Default::default()
        };
        let before = rooms_for_machine(&machine(), true, PathStyle::Posix, &directory, &[])[0]
            .workers[1]
            .look;
        directory.conversations.remove(0);
        let rooms = rooms_for_machine(&machine(), true, PathStyle::Posix, &directory, &[]);
        assert_eq!(rooms[0].workers.len(), 1);
        assert_eq!(rooms[0].workers[0].look, before);
        directory.conversations.clear();
        assert!(rooms_for_machine(&machine(), true, PathStyle::Posix, &directory, &[]).is_empty());
    }

    #[test]
    fn covers_the_complete_agent_catalog_including_terminal_only_and_custom_agents() {
        for kind in ConversationAgentKind::ALL
            .into_iter()
            .chain([ConversationAgentKind::Custom])
        {
            let worker = worker(&conversation(kind.id(), "/repo", kind.id()), &[], true);
            assert_eq!(worker.provider, kind.label());
            assert_eq!(worker.icon.as_deref(), Some(kind.icon_asset().as_str()));
            assert!(api::ui::assets::load(worker.icon.as_ref().unwrap()).is_some());
        }
        for kind in TerminalAgentKind::ALL {
            let conversation = WorkspaceConversation {
                terminals: vec![WorkspaceTerminal {
                    id: Some("s".into()),
                    launch_cmd: Some(kind.quick_terminal_cmd().into()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            // 即使还没有 hook 事件，也从已知的启动类型正确显示身份；状态保持空闲。
            let worker = worker(&conversation, &[], true);
            assert_eq!(worker.provider, kind.label(), "{}", kind.id());
            assert_eq!(worker.activity, Activity::Idle);
            assert!(worker.icon.is_some());
        }
        let future = worker(&conversation("future", "/repo", "future-agent"), &[], true);
        assert_eq!(future.provider, "future-agent");
        assert!(future.icon.is_none());
    }

    #[test]
    fn split_terminal_providers_are_not_lost_behind_the_first_shell() {
        let conversation = WorkspaceConversation {
            terminals: vec![
                WorkspaceTerminal::default(),
                WorkspaceTerminal {
                    id: Some("s-b".into()),
                    ..Default::default()
                },
                WorkspaceTerminal {
                    launch_cmd: Some("crush".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut state = state("b", DaemonPhase::Thinking);
        state.provider = Some("codex".into());
        let worker = worker(&conversation, &[state], true);
        assert_eq!(worker.provider, "Codex / Crush");
        assert_eq!(worker.activity, Activity::Thinking);
    }

    #[test]
    fn only_a_live_authoritative_success_triggers_the_completed_cue() {
        let conversation = conversation("c", "/repo", "codex");
        let mut success = state("c", DaemonPhase::Succeeded);
        assert!(worker(&conversation, &[success.clone()], true).completed);
        success.runtime = false;
        assert!(!worker(&conversation, &[success.clone()], true).completed);
        success.runtime = true;
        success.turn_events = false;
        assert!(!worker(&conversation, &[success], true).completed);
        assert!(!worker(&conversation, &[], true).completed);
        assert!(!worker(&conversation, &[state("c", DaemonPhase::Idle)], true).completed);
        assert!(!worker(&conversation, &[state("c", DaemonPhase::Succeeded)], false).completed);
    }
}
