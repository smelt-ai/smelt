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

/// 活连接挡住 headless 自升级。不能用「最近鉴权过」这种会过期的时间戳：
/// ACP 对话连着看很久也不再走 desktop 鉴权 / 终端 open，过期就会把正在跑的
/// 回合当成没人看。
#[test]
fn holding_a_viewer_lease_means_someone_is_watching() {
    let _lease = ViewerLease::acquire();
    assert!(viewer_is_present(), "控制连接还在就必须当成有人在看");
}

#[test]
fn nested_viewer_leases_keep_presence_until_the_last_drop() {
    let outer = ViewerLease::acquire();
    let inner = ViewerLease::acquire();
    drop(inner);
    assert!(
        viewer_is_present(),
        "还有一条观看连接时不能放行 headless 自升级"
    );
    drop(outer);
}

#[test]
fn only_changes_to_the_pending_link_wake_the_headless_upgrade() {
    use notify::event::{CreateKind, EventKind, Flag, ModifyKind, RenameMode};
    let dir = std::path::PathBuf::from("/opt/smelt/bin");
    let renamed = notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
        .add_path(dir.join(".smeltd.pending.link-1-abc"))
        .add_path(dir.join(smelt_core::daemon_image::PENDING_NAME));
    assert!(touches_staged_successor(&renamed), "挂上新映像要叫醒");
    let unrelated = notify::Event::new(EventKind::Create(CreateKind::File))
        .add_path(dir.join("smeltd-0123abcd"));
    assert!(!touches_staged_successor(&unrelated), "发布映像本身还不算挂上");
    let rescan = notify::Event::new(EventKind::Other).set_flag(Flag::Rescan);
    assert!(touches_staged_successor(&rescan), "事件丢了要看一眼");
}

#[test]
fn failed_headless_upgrades_back_off_up_to_thirty_minutes() {
    assert_eq!(headless_upgrade_retry_delay(0), Duration::from_secs(60));
    assert_eq!(headless_upgrade_retry_delay(1), Duration::from_secs(120));
    assert_eq!(headless_upgrade_retry_delay(4), Duration::from_secs(16 * 60));
    assert_eq!(headless_upgrade_retry_delay(5), Duration::from_secs(30 * 60));
    assert_eq!(headless_upgrade_retry_delay(u32::MAX), Duration::from_secs(30 * 60));
}

#[test]
fn failed_pipe_accepts_back_off_up_to_five_seconds() {
    assert_eq!(pipe_accept_retry_delay(0), Duration::from_millis(50));
    assert_eq!(pipe_accept_retry_delay(3), Duration::from_millis(400));
    assert_eq!(pipe_accept_retry_delay(6), Duration::from_millis(3_200));
    assert_eq!(pipe_accept_retry_delay(7), Duration::from_secs(5));
    assert_eq!(pipe_accept_retry_delay(u32::MAX), Duration::from_secs(5));
}
