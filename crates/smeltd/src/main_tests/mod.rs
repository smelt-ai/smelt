//! smeltd 守护进程单测：按主题拆成子模块，从 `main.rs` 迁出。

use super::*;

/// 从交接文件恢复的测试入口：`legacy_rehome` 传 `false`，只验证「认领 fd +
/// 重建会话表」这一段纯粹的恢复行为，不触发会杀进程/起宿主的遗留迁移。
/// 需要覆盖迁移决策的用例请直接测 `legacy_rehome_is_safe`。
fn resume_handoff(
    path: &str,
    event_hub: &EventHubHandle,
) -> Option<(UnixListener, Sessions, AcpSessions)> {
    resume_handoff_with_remote(path, event_hub, None, false)
}

mod acp_tests;
mod action_integration_tests;
mod action_tests;
mod agent_mcp_tests;
mod automation_command_tests;
mod autostart_remote_tests;
mod daemon_executable_tests;
mod ensure_remote_gateway_write_tests;
mod handoff_tests;
mod handoff_v2_tests;
mod history_rename_tests;
mod input_integration_tests;
mod input_payload_tests;
mod plugin_handoff_tests;
mod remote_catalog_recovery_tests;
mod remote_reattach_tests;
mod resize_bounds_tests;
mod resume_handoff_tests;
mod self_upgrade_tests;
mod session_directory_tests;
mod single_instance_tests;
#[cfg(target_os = "macos")]
mod sleep_assertion_leak_tests;
mod sleep_assertion_policy_tests;
mod snapshot_tests;
mod spawn_gate_sync_tests;
mod state_bus_tests;
mod state_listener_tests;
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
        pending_prompts: Mutex::new(VecDeque::new()),
        hosted_handle: Mutex::new(None),
        host_snapshot_revision: AtomicU64::new(0),
        handle: Mutex::new(None),
        unreaped_pid: Mutex::new(None),
        cwd: None,
        agent_needs_transcript_check: true,
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
        agent_session: Mutex::new(None),
        conversation_submit: Mutex::new(()),
        pending_agent_preset: Mutex::new(None),
    }
}

/// core 投影的四个只读能力。测试专用，且不碰任何 daemon 私有状态——只是把
/// `smelt_plugin_api` 的公开常量拼成集合，所以放在测试侧而不是 `event_hub`。
fn core_read_capabilities() -> std::collections::BTreeSet<smelt_plugin_api::Capability> {
    [
        smelt_plugin_api::CORE_CAPABILITY_SESSION_READ,
        smelt_plugin_api::CORE_CAPABILITY_REMOTE_SESSIONS_READ,
        smelt_plugin_api::CORE_CAPABILITY_WORKSPACE_READ,
        smelt_plugin_api::CORE_CAPABILITY_AUTOMATION_READ,
    ]
    .into_iter()
    .map(|capability| {
        smelt_plugin_api::Capability::new(capability).expect("core capability is valid")
    })
    .collect()
}
