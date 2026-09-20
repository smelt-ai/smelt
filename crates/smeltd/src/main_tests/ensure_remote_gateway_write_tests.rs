use super::*;

#[test]
fn starts_with_requested_write_when_down() {
    let _lock = lock_remote_gateway_tests();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let (token, _addr, write) = ensure_remote_gateway_with_write(&state, true).expect("start");
    assert!(write, "应烤进 write=true");
    assert!(!token.is_empty());
    #[cfg(target_os = "macos")]
    assert!(
        state
            .lock()
            .unwrap()
            .gateway
            .as_ref()
            .and_then(|gateway| gateway._sleep_assertion.as_ref())
            .is_some(),
        "远程网关运行时必须持有系统防休眠断言"
    );
    // 现状一致：再要一次可写必须复用同一 token，不能偷偷再起一个
    let (token2, _, write2) = ensure_remote_gateway_with_write(&state, true).expect("reuse");
    assert_eq!(token, token2);
    assert!(write2);
    stop_remote_gateway(&state);
}

#[test]
fn hot_updates_shared_write_permission_without_restarting_gateway() {
    let _lock = lock_remote_gateway_tests();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let (_, addr, _) = ensure_remote_gateway_with_write(&state, false).expect("start");
    set_remote_gateway_write(&state, true).expect("enable write");
    {
        let guard = state.lock().unwrap();
        assert_eq!(
            guard.gateway.as_ref().map(|gateway| gateway.addr),
            Some(addr)
        );
        assert!(guard.gateway.as_ref().is_some_and(|gateway| gateway.write));
        assert!(guard.write_enabled.load(Ordering::SeqCst));
    }
    set_remote_gateway_write(&state, false).expect("disable write");
    assert!(!state.lock().unwrap().write_enabled.load(Ordering::SeqCst));
    stop_remote_gateway(&state);
}

#[test]
fn restarts_and_reuses_token_when_write_changes() {
    let _lock = lock_remote_gateway_tests();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let (token_ro, _, write_ro) =
        ensure_remote_gateway_with_write(&state, false).expect("start ro");
    assert!(!write_ro);

    // 关键回归：幂等的 start_remote_gateway 在已开时会忽略传入 write=true，
    // ensure 必须先停再开，否则隧道路径会静默保持只读。
    let (token_rw, _, write_rw) =
        ensure_remote_gateway_with_write(&state, true).expect("upgrade to rw");
    assert!(write_rw, "write 切换后必须变成可写");
    assert_eq!(token_ro, token_rw, "写权限切换不应让已配对手机失效");

    let (token_ro2, _, write_ro2) =
        ensure_remote_gateway_with_write(&state, false).expect("downgrade to ro");
    assert!(!write_ro2);
    assert_eq!(token_rw, token_ro2);
    stop_remote_gateway(&state);
}

#[test]
fn token_persists_until_explicit_rotation() {
    let _lock = lock_remote_gateway_tests();
    let dir = std::env::temp_dir().join(format!(
        "smeltd-remote-token-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("remote-token");
    let first = load_or_create_remote_token_at(&path).expect("create token");
    let after_restart = load_or_create_remote_token_at(&path).expect("reload token");
    assert_eq!(first, after_restart);

    let state = new_remote_state(Some(first.clone()));
    let rotated = rotate_remote_token_at(&state, &path).expect("rotate token");
    assert_ne!(first, rotated);
    assert_eq!(
        load_or_create_remote_token_at(&path).expect("reload rotated token"),
        rotated
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        state.lock().unwrap().token.as_deref(),
        Some(rotated.as_str())
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plain_start_remote_gateway_is_still_idempotent_on_write() {
    // 对照：裸 start_remote_gateway 的旧语义还在——已开时忽略 write 参数。
    // ensure 才是"按 write 对齐"的入口；别把两个行为搞混。
    let _lock = lock_remote_gateway_tests();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let (t1, _, w1) = start_remote_gateway(&state, "127.0.0.1", 0, false).expect("ro");
    assert!(!w1);
    let (t2, _, w2) = start_remote_gateway(&state, "127.0.0.1", 0, true).expect("idempotent");
    assert_eq!(t1, t2);
    assert!(!w2, "幂等路径必须继续忽略传入的 write=true");
    stop_remote_gateway(&state);
}
