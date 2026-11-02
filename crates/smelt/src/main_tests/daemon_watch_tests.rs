//! 跨日检查和守护映像监听里不碰真时间、真文件的那部分。

use std::time::Duration;

use chrono::{FixedOffset, TimeZone, Utc};

use crate::{event_touches_file, until_next_midnight, until_turn_minute_changes};

/// 一次性计时器排到下一个本地零点，按所在时区算，不按 UTC。
#[test]
fn next_midnight_is_local() {
    let late = Utc.with_ymd_and_hms(2026, 10, 26, 23, 59, 30).unwrap();
    assert_eq!(until_next_midnight(&late), Duration::from_secs(30));

    let just_past = Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap();
    assert_eq!(
        until_next_midnight(&just_past),
        Duration::from_secs(24 * 3600)
    );

    // UTC 15:00 在东八区是 23:00，离那边的零点还有一小时。
    let shanghai = FixedOffset::east_opt(8 * 3600).unwrap();
    let local = Utc
        .with_ymd_and_hms(2026, 10, 26, 15, 0, 0)
        .unwrap()
        .with_timezone(&shanghai);
    assert_eq!(until_next_midnight(&local), Duration::from_secs(3600));
}

/// 侧栏「本轮已运行 N 分钟」的一次性计时器排到分钟数下一次变的时候；没在一轮里不排。
#[test]
fn turn_clock_wakes_when_the_minute_changes() {
    assert_eq!(until_turn_minute_changes(0, 1_000), None);
    assert_eq!(
        until_turn_minute_changes(1_000, 1_000),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        until_turn_minute_changes(1_000, 1_045),
        Some(Duration::from_secs(15))
    );
    assert_eq!(
        until_turn_minute_changes(1_000, 1_120),
        Some(Duration::from_secs(60))
    );
    // 起点比现在还晚（时钟回拨）：一分钟后再看，不出零或负数。
    assert_eq!(
        until_turn_minute_changes(2_000, 1_000),
        Some(Duration::from_secs(60))
    );
}

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
