use super::*;
use std::time::Duration;

#[test]
fn self_upgrade_reply_ok_is_upgraded() {
    assert_eq!(
        classify_self_upgrade_reply("{\"ok\":true}"),
        SelfUpgradeOutcome::Upgraded
    );
}

#[test]
fn self_upgrade_reply_busy_stays_for_retry() {
    assert_eq!(
        classify_self_upgrade_reply(
            "{\"ok\":false,\"busy\":true,\"err\":\"ACP 会话仍有未完成请求\"}"
        ),
        SelfUpgradeOutcome::Busy("ACP 会话仍有未完成请求".to_string())
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

/// env 指纹优先：handoff successor / GUI 拉起传入的值即本进程映像，
/// 不碰磁盘。
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

/// predecessor 已退：EOF 立刻返回，不等满超时。
#[test]
fn predecessor_exit_returns_on_eof() {
    let (a, b) = UnixStream::pair().unwrap();
    drop(b);
    let start = std::time::Instant::now();
    wait_for_predecessor_exit(&a, Duration::from_secs(10));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "EOF 应立刻返回，不应等满超时"
    );
}

/// predecessor hung 住：超时兜底返回，插件不能无限等。
#[test]
fn predecessor_exit_times_out() {
    let (a, _b) = UnixStream::pair().unwrap();
    let start = std::time::Instant::now();
    wait_for_predecessor_exit(&a, Duration::from_millis(100));
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_secs(5),
        "应约 100ms 超时返回，实际 {elapsed:?}"
    );
}
