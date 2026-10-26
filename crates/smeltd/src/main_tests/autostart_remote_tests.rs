//! 远程访问的开关和自愈。不碰网络：真正的绑定由 `install_test_endpoint` 代替。

use super::*;
use smelt_core::remote_config::{RemoteAccessState, RemoteConfig};

fn config(enabled: bool, relay: &str) -> RemoteConfig {
    RemoteConfig {
        enabled,
        iroh_relay: relay.to_string(),
    }
}

#[test]
fn disabled_config_starts_nothing() {
    let state = new_remote_state();
    apply_remote_config(&state, &config(false, ""));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(remote_status(&state), RemoteAccessState::Off);
    assert!(state.lock().unwrap().endpoint.is_none());
}

#[test]
fn invalid_relay_is_reported_instead_of_falling_back() {
    let state = new_remote_state();
    apply_remote_config(&state, &config(true, "not a relay ::"));
    assert!(
        matches!(remote_status(&state), RemoteAccessState::Failed { .. }),
        "自建 relay 写错了要明说，不能悄悄改用公共 relay"
    );
}

#[test]
fn turning_remote_off_shuts_the_running_endpoint() {
    let _lock = lock_remote_tests();
    let state = new_remote_state();
    install_test_endpoint(&state, "endpoint-a");
    assert_eq!(
        remote_status(&state),
        RemoteAccessState::Running {
            endpoint_id: "endpoint-a".to_string(),
            relay: None,
        }
    );
    apply_remote_config(&state, &config(false, ""));
    assert_eq!(remote_status(&state), RemoteAccessState::Off);
}

#[test]
fn same_config_keeps_the_running_endpoint() {
    let _lock = lock_remote_tests();
    let state = new_remote_state();
    install_test_endpoint(&state, "endpoint-a");
    apply_remote_config(&state, &config(true, ""));
    assert!(
        matches!(remote_status(&state), RemoteAccessState::Running { .. }),
        "配置没变就不该重绑：已连着的设备会断"
    );
}

#[test]
fn stopping_blocks_any_further_start() {
    let state = new_remote_state();
    state.lock().unwrap().stopping = true;
    apply_remote_config(&state, &config(true, ""));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        remote_status(&state),
        RemoteAccessState::Off,
        "升级 exec 前停掉后，不能再拉起任何东西"
    );
}

/// `remote.status.wait`：状态和调用方手里的不一样就立刻回，一样就等到它变。
#[test]
fn status_wait_returns_once_the_state_differs() {
    let _lock = lock_remote_tests();
    let state = new_remote_state();
    assert_eq!(
        wait_remote_status_change(&state, &RemoteAccessState::Starting, Duration::ZERO),
        RemoteAccessState::Off,
        "已经不一样就不等"
    );
    let waiter = {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            wait_remote_status_change(&state, &RemoteAccessState::Off, Duration::from_secs(60))
        })
    };
    install_test_endpoint(&state, "endpoint-a");
    assert_eq!(
        waiter.join().unwrap(),
        RemoteAccessState::Running {
            endpoint_id: "endpoint-a".to_string(),
            relay: None,
        }
    );
    apply_remote_config(&state, &config(false, ""));
}
