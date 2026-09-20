use super::{PaneState, terminal_session_display_title};

#[test]
fn single_pane_title_keeps_the_surviving_pane_name() {
    assert_eq!(
        terminal_session_display_title(None, None, 1, Some("跑测试的终端"), "自动标题".into()),
        "跑测试的终端"
    );
    assert_eq!(
        terminal_session_display_title(
            None,
            Some("整个会话"),
            1,
            Some("跑测试的终端"),
            "自动标题".into(),
        ),
        "整个会话"
    );
    assert_eq!(
        terminal_session_display_title(None, None, 2, Some("左侧 pane"), "自动标题".into()),
        "自动标题",
        "分屏时 pane 名不能反向改掉会话名"
    );
}

/// 终端里跑的是可识别的 agent 对话时，名字属于那段对话：它必须盖过会话/pane
/// 上遗留的 pin，否则用户换一段对话，侧栏还顶着上一段的名字。
#[test]
fn conversation_name_outranks_stale_session_and_pane_pins() {
    assert_eq!(
        terminal_session_display_title(
            Some("记忆方案研究"),
            Some("上一段对话的旧名字"),
            1,
            Some("pane 旧名字"),
            "自动标题".into(),
        ),
        "记忆方案研究"
    );
    // 换到一段还没被命名的对话：回落到自动标题，而不是继续用旧 pin。
    assert_eq!(
        terminal_session_display_title(
            None,
            Some("  "),
            1,
            None,
            "Review Issue - GitHub Copilot".into(),
        ),
        "Review Issue - GitHub Copilot"
    );
}

/// pane 自定义名必须能跟着 Leaf 存下来、读回来（否则重开 GUI 就丢名字）。
#[test]
fn leaf_custom_title_roundtrips() {
    let leaf = PaneState::Leaf {
        cwd: Some("/tmp/x".into()),
        id: Some("sid-1".into()),
        custom_title: Some("跑测试的终端".into()),
        launch_label: Some("Claude Code".into()),
        launch_cmd: Some("claude --dangerously-skip-permissions".into()),
    };
    let json = serde_json::to_string(&leaf).unwrap();
    let back: PaneState = serde_json::from_str(&json).unwrap();
    match back {
        PaneState::Leaf {
            custom_title,
            launch_label,
            launch_cmd,
            id,
            cwd,
        } => {
            assert_eq!(custom_title.as_deref(), Some("跑测试的终端"));
            assert_eq!(launch_label.as_deref(), Some("Claude Code"));
            assert_eq!(
                launch_cmd.as_deref(),
                Some("claude --dangerously-skip-permissions")
            );
            assert_eq!(id.as_deref(), Some("sid-1"));
            assert_eq!(cwd.as_deref(), Some("/tmp/x"));
        }
        _ => panic!("应当反序列化成 Leaf"),
    }
}

#[test]
fn old_archive_without_custom_title_still_loads() {
    let old = r#"{"Leaf":{"cwd":"/tmp/x","id":"sid-1"}}"#;
    let back: PaneState = serde_json::from_str(old).unwrap();
    match back {
        PaneState::Leaf {
            custom_title,
            launch_label,
            launch_cmd,
            id,
            ..
        } => {
            assert!(custom_title.is_none(), "旧存档不该凭空冒出自定义名");
            assert!(launch_label.is_none(), "旧存档不该凭空冒出启动项名");
            assert!(launch_cmd.is_none(), "旧存档不该凭空冒出启动命令");
            assert_eq!(id.as_deref(), Some("sid-1"));
        }
        _ => panic!("应当反序列化成 Leaf"),
    }
}
