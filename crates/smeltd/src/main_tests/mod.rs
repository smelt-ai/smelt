//! smeltd 守护进程单测：按主题拆成子模块，从 `main.rs` 迁出。

use super::*;

/// 从交接文件恢复的测试入口：`legacy_rehome` 传 `false`，只验证「认领 fd +
/// 重建会话表」这一段纯粹的恢复行为，不触发会杀进程/起宿主的遗留迁移。
/// 需要覆盖迁移决策的用例请直接测 `legacy_rehome_is_safe`。
#[cfg(unix)]
fn resume_handoff(
    path: &str,
    event_hub: &EventHubHandle,
) -> Option<(UnixListener, Sessions, AcpSessions)> {
    resume_handoff_with_remote(path, event_hub, None, false)
}

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
#[cfg(unix)]
mod handoff_tests;
#[cfg(unix)]
mod handoff_v2_tests;
mod history_rename_tests;
#[cfg(unix)]
mod input_integration_tests;
mod input_payload_tests;
mod remote_catalog_recovery_tests;
#[cfg(unix)]
mod remote_reattach_tests;
#[cfg(unix)]
mod resize_bounds_tests;
#[cfg(unix)]
mod resume_handoff_tests;
mod runtime_client_tests;
mod self_upgrade_tests;
mod session_directory_tests;
mod single_instance_tests;
#[cfg(target_os = "macos")]
mod sleep_assertion_leak_tests;
mod sleep_assertion_policy_tests;
mod snapshot_tests;
mod spawn_gate_sync_tests;
mod state_bus_tests;
#[cfg(unix)]
mod state_listener_tests;
mod test_owner_tests;
#[cfg(unix)]
mod watch_tests;
mod workspace_menu_tests;

/// 测一个 ACP 会话值：默认 Direct 绑定、无 launch，供各子模块复用。
/// 从 `acp_tests` 迁出——handoff drill 也要搭 hosted 会话，一份拷贝。
fn make_acp_session_value(
    id: &str,
    reduced: smelt_core::acp_session::AcpSessionState,
) -> AcpSession {
    let instance = next_session_instance();
    AcpSession {
        instance,
        reduced: Mutex::new(reduced),
        snapshot_revision: AtomicU64::new(0),
        connection_generation: AtomicU64::new(0),
        turn_completion: Mutex::new(()),
        prompt_in_flight: AtomicBool::new(false),
        automation_watch: Mutex::new(None),
        pending_prompts: Mutex::new(VecDeque::new()),
        next_pending_prompt_id: AtomicU64::new(0),
        immediate_delivery: Mutex::new(None),
        unaccepted_prompt: Mutex::new(None),
        provider_hold: AtomicBool::new(false),
        hosted_handle: Mutex::new(None),
        host_snapshot_revision: AtomicU64::new(0),
        handle: Mutex::new(None),
        unreaped_pid: Mutex::new(None),
        cwd: None,
        state: Arc::new(Mutex::new(SessionState {
            id: id.to_string(),
            instance,
            ..Default::default()
        })),
        output_gate: Mutex::new(()),
        out: Mutex::new(AcpOut {
            client: None,
            watchers: Vec::new(),
        }),
        launch_spec: Mutex::new(None),
        runtime_spec_fingerprint: Mutex::new(None),
        restore_state: Mutex::new(AcpRestoreState::Fresh),
        ephemeral_env: Mutex::new(BTreeMap::new()),
        conversation_binding: Mutex::new(Some(
            smelt_core::conversation::ConversationBinding::Direct,
        )),
        conversation_submit: Mutex::new(crate::acp_host::RecentSubmissions::default()),
        pending_agent_preset: Mutex::new(None),
        durable_history_session_id: Mutex::new(None),
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
            cwd: None,
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
