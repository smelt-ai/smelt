use super::*;

#[test]
fn remote_stop_reaps_exec_orphans_so_idle_sleep_can_return() {
    let _lock = lock_remote_gateway_tests();
    let before = SystemSleepAssertion::count_owned();
    let leaked = SystemSleepAssertion::acquire().expect("leak one assertion");
    std::mem::forget(leaked);
    assert_eq!(
        SystemSleepAssertion::count_owned(),
        before + 1,
        "forget 必须留下一条没有 Drop 的断言，模拟 exec 泄漏"
    );

    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    start_remote_gateway(&state, "127.0.0.1", 0, false).expect("start");
    assert_eq!(
        SystemSleepAssertion::count_owned(),
        before + 2,
        "活着的网关一条 + exec 泄漏一条"
    );

    stop_remote_gateway(&state);
    assert_eq!(
        SystemSleepAssertion::count_owned(),
        before,
        "关掉远程必须把 exec 泄漏的断言一起清掉，否则空闲睡眠关不干净"
    );
}

#[test]
fn process_start_reaps_survivors_before_autostart() {
    let _lock = lock_remote_gateway_tests();
    let before = SystemSleepAssertion::count_owned();
    std::mem::forget(SystemSleepAssertion::acquire().expect("orphan a"));
    std::mem::forget(SystemSleepAssertion::acquire().expect("orphan b"));
    assert_eq!(SystemSleepAssertion::count_owned(), before + 2);

    SystemSleepAssertion::release_orphans();
    assert_eq!(
        SystemSleepAssertion::count_owned(),
        before,
        "新进程启动时必须先清掉 exec 活下来的同名断言"
    );
}
