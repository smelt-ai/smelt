use super::*;

fn test_socket(name: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("smeltd-{name}-{}-{nonce}.sock", std::process::id()))
}

#[test]
fn concurrent_starters_leave_exactly_one_listener() {
    const STARTERS: usize = 8;
    let path = test_socket("single-instance");
    let barrier = Arc::new(std::sync::Barrier::new(STARTERS));
    let mut starters = Vec::new();

    for _ in 0..STARTERS {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        starters.push(thread::spawn(move || {
            let listener = bind_single_instance(&path, true).expect("并发启动不应 bind 失败");
            barrier.wait();
            listener.is_some()
        }));
    }

    let listeners = starters
        .into_iter()
        .map(|starter| starter.join().unwrap())
        .filter(|has_listener| *has_listener)
        .count();
    assert_eq!(listeners, 1, "并发启动只能有一个进程取得 listener");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

#[test]
fn stale_socket_path_is_replaced_while_holding_startup_lock() {
    let path = test_socket("stale");
    std::fs::write(&path, b"stale").unwrap();

    let listener = bind_single_instance(&path, true)
        .expect("僵尸 socket 应可恢复")
        .expect("没有活实例时应取得 listener");
    assert!(UnixStream::connect(&path).is_ok());

    drop(listener);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}
