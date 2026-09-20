use super::*;

#[test]
fn pi_status_bridge_is_injected_process_locally() {
    let extension = std::path::Path::new("/tmp/Smelt Agent/pi-status.ts");

    assert_eq!(
        launch_with_terminal_status_bridge(
            smelt_core::agent_kind::TerminalAgentKind::Pi.quick_terminal_cmd(),
            extension,
        ),
        "pi --approve --extension '/tmp/Smelt Agent/pi-status.ts'"
    );
    assert_eq!(
        launch_with_terminal_status_bridge(
            "env PI_CODING_AGENT_DIR=/tmp pi --session abc",
            extension,
        ),
        "env PI_CODING_AGENT_DIR=/tmp pi --session abc --extension '/tmp/Smelt Agent/pi-status.ts'"
    );
    let already_injected = "pi --extension '/tmp/Smelt Agent/pi-status.ts'";
    assert_eq!(
        launch_with_terminal_status_bridge(already_injected, extension),
        already_injected
    );
}

#[test]
fn pi_status_bridge_stays_before_the_cli_argument_terminator() {
    let extension = std::path::Path::new("/tmp/pi-status.ts");

    assert_eq!(
        launch_with_terminal_status_bridge("pi -- --literal prompt", extension),
        "pi --extension '/tmp/pi-status.ts' -- --literal prompt"
    );
}

#[test]
fn terminal_status_bridge_does_not_touch_other_agents() {
    let extension = std::path::Path::new("/tmp/pi-status.ts");

    assert_eq!(
        launch_with_terminal_status_bridge(
            "codex --dangerously-bypass-approvals-and-sandbox",
            extension,
        ),
        "codex --dangerously-bypass-approvals-and-sandbox"
    );
    assert_eq!(launch_with_terminal_status_bridge("zsh", extension), "zsh");
}

#[test]
fn pi_status_bridge_shell_quotes_hostile_extension_paths() {
    let extension = std::path::Path::new("/tmp/Pi's status.ts");

    assert_eq!(
        launch_with_terminal_status_bridge("pi", extension),
        "pi --extension '/tmp/Pi'\"'\"'s status.ts'"
    );
}

#[test]
fn bundled_pi_status_extension_is_materialized_idempotently() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let dir = std::env::temp_dir().join(format!("smelt-pi-status-{nonce}"));
    let path = dir.join("pi-status-extension.ts");

    sync_pi_status_extension_at(&path).unwrap();
    sync_pi_status_extension_at(&path).unwrap();

    let source = std::fs::read_to_string(&path).unwrap();
    assert_eq!(source, PI_STATUS_EXTENSION_SOURCE);
    assert!(source.contains("pi.on(\"agent_start\""));
    assert!(source.contains("pi.on(\"agent_settled\""));
    assert!(source.contains("pi.on(\"tool_execution_start\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn opencode_mcp_config_is_process_local_and_uses_its_native_schema() {
    let config = opencode_mcp_config_json(
        "/tmp/smelt-agent-mcp",
        "session-1",
        "token-1",
        "/tmp/smeltd.sock",
    );

    assert_eq!(
        config,
        serde_json::json!({
            "mcp": {
                "smelt": {
                    "type": "local",
                    "command": ["/tmp/smelt-agent-mcp"],
                    "enabled": true,
                    "environment": {
                        "SMELT_SESSION_ID": "session-1",
                        "SMELT_AGENT_TOKEN": "token-1",
                        "SMELT_SOCK": "/tmp/smeltd.sock",
                    },
                    "timeout": 610_000,
                }
            }
        })
    );
}

#[test]
fn opencode_mcp_launch_uses_only_an_inline_environment_overlay() {
    let launch = launch_with_opencode_mcp(
        "opencode --auto",
        "/tmp/smelt-agent-mcp",
        "session-1",
        "token-1",
        "/tmp/smeltd.sock",
    );

    assert!(launch.starts_with("OPENCODE_CONFIG_CONTENT='{"));
    assert!(launch.ends_with("}' opencode --auto"));
    assert!(!launch.contains("opencode.json"));
    assert!(!launch.contains(" mcp add "));
}
