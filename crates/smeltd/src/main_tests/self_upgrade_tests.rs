use super::*;

#[test]
fn self_upgrade_reply_ok_is_upgraded() {
    assert_eq!(
        classify_self_upgrade_reply("{\"ok\":true}"),
        SelfUpgradeOutcome::Upgraded
    );
}

#[test]
fn self_upgrade_reply_error_is_failed() {
    assert_eq!(
        classify_self_upgrade_reply("{\"ok\":false,\"err\":\"store 已被迁移\"}"),
        SelfUpgradeOutcome::Failed("store 已被迁移".to_string())
    );
    // 非 JSON 回包：失败而非升级，避免误判。
    assert!(matches!(
        classify_self_upgrade_reply("not-json"),
        SelfUpgradeOutcome::Failed(_)
    ));
}

/// env 指纹优先：桌面拉起时传入的就是本进程这份，不碰磁盘。
#[test]
fn pinned_fingerprint_prefers_env() {
    assert_eq!(
        pinned_daemon_fingerprint(Some("fp-from-env".to_string())),
        Some("fp-from-env".to_string())
    );
    // 空白 env 等同无：回退到磁盘哈希（不断言具体值，只断言不返回空白）。
    let fallback = pinned_daemon_fingerprint(Some("   ".to_string()));
    assert!(fallback.is_none_or(|fp| !fp.trim().is_empty()));
}
