use super::*;

fn config(enabled: bool) -> smelt_core::remote_config::RemoteConfig {
    smelt_core::remote_config::RemoteConfig {
        enabled,
        // 留空：测试不碰网络，只验证网关那半段和门闩。
        iroh_relay: String::new(),
        write_enabled: false,
    }
}

#[test]
fn disabled_config_starts_nothing() {
    let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let iroh: IrohState = Arc::new(Mutex::new(None));
    let iroh_conns = new_iroh_connections();
    assert!(!spawn_remote_autostart(
        config(false),
        Arc::clone(&remote),
        Arc::clone(&iroh),
        Arc::clone(&iroh_conns),
    ));
    thread::sleep(Duration::from_millis(200));
    assert!(
        remote.lock().unwrap().gateway.is_none(),
        "没开远程时守护绝不能自己把网关开起来"
    );
}

#[test]
fn enabled_config_brings_the_gateway_back() {
    let _lock = lock_remote_gateway_tests();
    let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let iroh: IrohState = Arc::new(Mutex::new(None));
    let iroh_conns = new_iroh_connections();
    assert!(spawn_remote_autostart(
        config(true),
        Arc::clone(&remote),
        Arc::clone(&iroh),
        Arc::clone(&iroh_conns),
    ));
    // 网关是本机回环 + 端口 0，起得很快；隧道因为没配 relay 会被跳过。
    let mut started = false;
    for _ in 0..50 {
        if remote.lock().unwrap().gateway.is_some() {
            started = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(started, "守护重启后必须按配置把远程网关拉回来");
    assert!(iroh.lock().unwrap().is_none(), "没配 relay 就不该起隧道");
    stop_remote_gateway(&remote);
}

#[test]
fn stopping_blocks_autostart_from_creating_a_gateway() {
    let _lock = lock_remote_gateway_tests();
    #[cfg(target_os = "macos")]
    let before = SystemSleepAssertion::count_owned();
    let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    remote.lock().unwrap().stopping = true;
    assert!(spawn_remote_autostart(
        config(true),
        Arc::clone(&remote),
        Arc::new(Mutex::new(None)),
        new_iroh_connections(),
    ));
    thread::sleep(Duration::from_millis(300));
    assert!(
        remote.lock().unwrap().gateway.is_none(),
        "upgrade cleanup 之后 autostart 绝不能再把网关拉起来"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        SystemSleepAssertion::count_owned(),
        before,
        "被取消的 autostart 不能留下电源断言"
    );
}

#[test]
fn stop_waits_for_the_inflight_iroh_start_boundary() {
    let start_guard = IROH_START_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
    let generation = iroh_autostart_generation(&remote);
    let iroh: IrohState = Arc::new(Mutex::new(None));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let remote_for_stop = Arc::clone(&remote);
    let iroh_for_stop = Arc::clone(&iroh);
    let worker = thread::spawn(move || {
        stop_iroh(&iroh_for_stop, &remote_for_stop);
        done_tx.send(()).unwrap();
    });

    for _ in 0..100 {
        if !iroh_autostart_is_current(&remote, generation) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !iroh_autostart_is_current(&remote, generation),
        "stop 必须先作废已经排队的 autostart"
    );
    assert!(
        done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "start 临界区退出前，stop 不能提前报告完成"
    );

    drop(start_guard);
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("start 临界区释放后 stop 应完成");
    worker.join().unwrap();
}

#[test]
fn old_disconnect_does_not_remove_the_new_connection_instance() {
    let connections = new_iroh_connections();
    let generation = begin_iroh_connection_generation(&connections);
    track_iroh_connection_event(
        &connections,
        generation,
        smelt_iroh::ConnectionEvent::Connected {
            connection_id: 10,
            remote_id: "device-a".into(),
            connected_at: 100,
        },
    );
    track_iroh_connection_event(
        &connections,
        generation,
        smelt_iroh::ConnectionEvent::Connected {
            connection_id: 11,
            remote_id: "device-a".into(),
            connected_at: 200,
        },
    );
    track_iroh_connection_event(
        &connections,
        generation,
        smelt_iroh::ConnectionEvent::Disconnected {
            connection_id: 10,
            remote_id: "device-a".into(),
        },
    );

    let snapshot = snapshot_iroh_connections(&connections, generation);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].remote_id, "device-a");
    assert_eq!(snapshot[0].connected_at, 200);

    let next_generation = begin_iroh_connection_generation(&connections);
    track_iroh_connection_event(
        &connections,
        generation,
        smelt_iroh::ConnectionEvent::Connected {
            connection_id: 12,
            remote_id: "stale-device".into(),
            connected_at: 300,
        },
    );
    assert!(snapshot_iroh_connections(&connections, next_generation).is_empty());
}
