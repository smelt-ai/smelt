use super::*;

#[test]
fn owner_watch_returns_once_the_owner_exits() {
    #[cfg(windows)]
    let mut owner = std::process::Command::new("ping")
        .args(["-n", "31", "127.0.0.1"])
        .spawn()
        .unwrap();
    #[cfg(not(windows))]
    let mut owner = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = owner.id() as i32;
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        wait_for_test_owner_exit(pid);
        let _ = done_tx.send(());
    });

    assert!(
        done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "主人还活着，守护不能退"
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("主人退出后守护必须跟着退");
}
