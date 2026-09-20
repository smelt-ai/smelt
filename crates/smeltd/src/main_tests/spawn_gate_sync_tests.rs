use super::{SPAWN_GATE, acquire_upgrade_spawn_gate, new_test_acp_sessions};
use std::sync::{Arc, RwLock, mpsc};
use std::time::Duration;

#[test]
fn daemon_write_guard_blocks_acp_spawn_permit() {
    let acp_sessions = new_test_acp_sessions();
    let acp_gate = acp_sessions.spawn_gate();
    let write_guard = SPAWN_GATE.write().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();

    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let _permit = acp_gate.read().unwrap();
        entered_tx.send(()).unwrap();
    });

    ready_rx.recv().unwrap();
    let entered_while_locked = entered_rx.recv_timeout(Duration::from_millis(50)).is_ok();
    drop(write_guard);
    if !entered_while_locked {
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("ACP spawn permit should proceed after upgrade releases the gate");
    }
    worker.join().unwrap();
    assert!(
        !entered_while_locked,
        "terminal upgrade gate and ACP registry must share the same lock"
    );
}

#[test]
fn upgrade_snapshot_waits_for_preexisting_spawn_readers() {
    let gate = Arc::new(RwLock::new(()));
    let spawn_permit = gate.read().unwrap();
    let gate_for_upgrade = Arc::clone(&gate);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();

    let upgrade = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let _write_guard = acquire_upgrade_spawn_gate(&gate_for_upgrade);
        snapshot_tx.send(()).unwrap();
    });

    ready_rx.recv().unwrap();
    let snapshot_while_reader_held = snapshot_rx.recv_timeout(Duration::from_millis(50)).is_ok();
    drop(spawn_permit);
    if !snapshot_while_reader_held {
        snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("snapshot collection should start after existing readers finish");
    }
    upgrade.join().unwrap();
    assert!(
        !snapshot_while_reader_held,
        "upgrade must not collect snapshots until existing spawns publish their metadata"
    );
}
