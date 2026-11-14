use super::PaneState;

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
