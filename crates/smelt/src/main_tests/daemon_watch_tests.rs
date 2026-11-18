//! 守护映像监听里不碰真文件的那部分。

use crate::event_touches_file;

fn event_at(path: &str) -> notify::Event {
    notify::Event::new(notify::EventKind::Any).add_path(std::path::PathBuf::from(path))
}

/// 目录里别的文件（cargo 产物、App 里的其它程序）变了不算，只认 smeltd。
#[test]
fn only_the_daemon_binary_counts() {
    let name = std::ffi::OsStr::new("smeltd");
    assert!(event_touches_file(
        &event_at("/repo/target/debug/smeltd"),
        name
    ));
    assert!(!event_touches_file(
        &event_at("/repo/target/debug/smelt"),
        name
    ));
    assert!(!event_touches_file(
        &event_at("/repo/target/debug/smeltd.d"),
        name
    ));
}
