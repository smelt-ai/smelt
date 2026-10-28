//! smeltd 守护进程单测：按主题拆成子模块，从 `main.rs` 迁出。

use super::*;
use crate::acp_registry::SessionLock;

mod acp_idle_tests;
#[cfg(unix)]
mod acp_tests;
#[cfg(unix)]
mod action_integration_tests;
mod action_tests;
mod automation_command_tests;
mod autostart_remote_tests;
#[cfg(unix)]
mod daemon_executable_tests;
#[cfg(unix)]
mod flow_control_tests;
mod history_rename_tests;
#[cfg(unix)]
mod input_integration_tests;
mod input_payload_tests;
mod remote_catalog_recovery_tests;
#[cfg(unix)]
mod remote_reattach_tests;
#[cfg(unix)]
mod resize_bounds_tests;
mod runtime_client_tests;
mod self_upgrade_tests;
mod session_directory_tests;
mod single_instance_tests;
#[cfg(target_os = "macos")]
mod sleep_assertion_leak_tests;
mod sleep_assertion_policy_tests;
mod snapshot_tests;
mod state_bus_tests;
#[cfg(unix)]
mod state_listener_tests;
mod test_owner_tests;
#[cfg(unix)]
mod watch_tests;
mod workspace_directory_tests;
mod workspace_menu_tests;

/// 测一个 ACP 会话值：默认 Direct 绑定、无 launch，供各子模块复用。
fn make_acp_session_value(
    id: &str,
    reduced: smelt_core::acp_session::AcpSessionState,
) -> AcpSession {
    let instance = next_session_instance();
    AcpSession {
        instance,
        reduced: Watched::new(reduced),
        snapshot_revision: AtomicU64::new(0),
        connection_generation: AtomicU64::new(0),
        turn_completion: SessionLock::new(()),
        prompt_in_flight: AtomicBool::new(false),
        automation_watch: SessionLock::new(None),
        pending_prompts: SessionLock::new(VecDeque::new()),
        next_pending_prompt_id: AtomicU64::new(0),
        immediate_delivery: SessionLock::new(None),
        unaccepted_prompt: SessionLock::new(None),
        provider_hold: AtomicBool::new(false),
        handle: SessionLock::new(None),
        unreaped_pid: SessionLock::new(None),
        cwd: None,
        state: Arc::new(SessionLock::new(SessionState {
            id: id.to_string(),
            instance,
            ..Default::default()
        })),
        output_gate: SessionLock::new(()),
        out: SessionLock::new(AcpOut {
            client: None,
            watchers: Vec::new(),
        }),
        launch_spec: SessionLock::new(None),
        restore_state: SessionLock::new(AcpRestoreState::Fresh),
        ephemeral_env: SessionLock::new(BTreeMap::new()),
        conversation_binding: SessionLock::new(Some(
            smelt_core::conversation::ConversationBinding::Direct,
        )),
        conversation_submit: SessionLock::new(crate::acp_host::RecentSubmissions::default()),
        pending_agent_preset: SessionLock::new(None),
        durable_history_session_id: SessionLock::new(None),
        idle: Default::default(),
    }
}

/// `Ctl.master` 用一根真管道（不是 /dev/null）：这样能从另一头读回真正写
/// 进去的字节。action 和 input 的集成测试共用这一份。
#[cfg(unix)]
fn make_pipe_session(rows: u16, cols: u16, phase: Phase) -> (Arc<Session>, std::fs::File) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };

    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i32;
    drop(child); // 退出状态交给下面 Session 的唯一 TerminalChild owner

    let state = Arc::new(Mutex::new(SessionState {
        phase,
        ..Default::default()
    }));
    let subscribers = new_event_hub();
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        subscribers,
        Arc::clone(&color_replies),
    );
    let sess = Arc::new(Session {
        instance: next_session_instance(),
        geometry_token: uuid::Uuid::new_v4().simple().to_string(),
        child: TerminalChild::start(pid).unwrap(),
        ctl: Mutex::new(Ctl {
            master,
            jolt: false,
            cols,
            rows,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
        }),
        input_gate: Mutex::new(()),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        output_gate: Mutex::new(()),
        color_replies,
        term: Mutex::new(new_daemon_term(rows, cols, listener)),
        state,
    });
    (sess, read_end)
}

/// core 投影的四个只读能力。测试专用，且不碰任何 daemon 私有状态——只是把
/// `smelt_event_protocol` 的公开常量拼成集合，所以放在测试侧而不是 `event_hub`。
fn core_read_capabilities() -> std::collections::BTreeSet<smelt_event_protocol::Capability> {
    [
        smelt_event_protocol::CORE_CAPABILITY_SESSION_READ,
        smelt_event_protocol::CORE_CAPABILITY_REMOTE_SESSIONS_READ,
        smelt_event_protocol::CORE_CAPABILITY_WORKSPACE_READ,
        smelt_event_protocol::CORE_CAPABILITY_AUTOMATION_READ,
    ]
    .into_iter()
    .map(|capability| {
        smelt_event_protocol::Capability::new(capability).expect("core capability is valid")
    })
    .collect()
}
