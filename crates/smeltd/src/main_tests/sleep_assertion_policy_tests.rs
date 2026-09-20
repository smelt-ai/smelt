use super::*;

#[test]
fn battery_never_blocks_sleep_ac_keeps_remote_available() {
    assert!(
        sleep_assertion_desired(PowerKind::Ac),
        "插电时远程开着应阻止空闲睡眠，手机才能随时连"
    );
    assert!(
        !sleep_assertion_desired(PowerKind::Battery),
        "电池供电绝不能阻止休眠，远程只在电脑醒着时能连"
    );
}

#[cfg(target_os = "macos")]
fn assertion_held(state: &RemoteState) -> bool {
    state
        .lock()
        .unwrap()
        .gateway
        .as_ref()
        .and_then(|gateway| gateway._sleep_assertion.as_ref())
        .is_some()
}

#[cfg(target_os = "macos")]
#[test]
fn battery_gateway_does_not_hold_idle_sleep() {
    let _lock = lock_remote_gateway_tests();
    set_power_kind_override(Some(PowerKind::Battery));
    let before = SystemSleepAssertion::count_owned();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    start_remote_gateway(&state, "127.0.0.1", 0, false).expect("start");
    assert!(!assertion_held(&state), "电池上开远程也不能握防休眠断言");
    assert_eq!(SystemSleepAssertion::count_owned(), before);
    stop_remote_gateway(&state);
    assert_eq!(SystemSleepAssertion::count_owned(), before);
}

#[cfg(target_os = "macos")]
#[test]
fn unplugging_releases_assertion_even_if_a_session_is_active() {
    let _lock = lock_remote_gateway_tests();
    let before = SystemSleepAssertion::count_owned();
    let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    start_remote_gateway(&state, "127.0.0.1", 0, false).expect("start on ac");
    assert!(assertion_held(&state), "测试锁默认插电，开远程就应握断言");
    assert_eq!(SystemSleepAssertion::count_owned(), before + 1);

    set_power_kind_override(Some(PowerKind::Battery));
    sync_remote_sleep_assertion(&state);
    assert!(
        !assertion_held(&state),
        "拔掉电源必须立刻放下断言，不能因为远程还开着就继续挡休眠"
    );
    assert_eq!(SystemSleepAssertion::count_owned(), before);

    set_power_kind_override(Some(PowerKind::Ac));
    sync_remote_sleep_assertion(&state);
    assert!(assertion_held(&state), "插回电源后恢复旧行为，远程随时可连");
    assert_eq!(SystemSleepAssertion::count_owned(), before + 1);
    stop_remote_gateway(&state);
    assert_eq!(SystemSleepAssertion::count_owned(), before);
}
