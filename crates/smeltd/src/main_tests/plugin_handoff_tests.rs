use crate::handoff_v2::predecessor::successor_env;

#[test]
fn successor_env_carries_sock_fd_and_candidate_daemon_fingerprint() {
    let root = std::env::temp_dir().join(format!(
        "smeltd-plugin-handoff-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("smeltd.next");
    std::fs::write(&executable, b"candidate-daemon").unwrap();
    let expected = smelt_plugin_host::executable_fingerprint(&executable).unwrap();

    // 交接 v2 不再经 SMELTD_HANDOFF 传文件路径：fd 号即 import 模式信号。
    let environment = successor_env(17, &expected)
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(
        environment.get("SMELTD_HANDOFF_SOCK"),
        Some(&"17".to_string())
    );
    assert_eq!(
        environment.get("SMELTD_PLUGIN_DAEMON_FINGERPRINT"),
        Some(&expected)
    );
    std::fs::remove_dir_all(root).unwrap();
}
