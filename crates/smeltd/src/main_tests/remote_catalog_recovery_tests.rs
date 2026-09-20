//! 远程会话目录在加载失败后能自己走出来。
//!
//! 现场教训：`~/.smelt/smelt.sqlite3` 旁边留了一个跟主库对不上的 `-shm`，
//! SQLite 每次 open 都报 `disk I/O error`。daemon 把启动时那一次失败记成终局，
//! 于是整个进程生命周期里远程目录都不可用——手机端只剩一句
//! `remote session catalog unavailable`，桌面也发布不了侧栏菜单，唯一出路是
//! 重启进程。故障是文件层面的、可以被修好的，加载就必须能重试。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// loader 是函数指针（生产实现就是一个自由函数），只能靠静态量记录调用情况，
/// 所以这几个用例必须串行。
static SERIAL: Mutex<()> = Mutex::new(());
static FAILURES_BEFORE_SUCCESS: AtomicUsize = AtomicUsize::new(0);
static LOAD_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

fn flaky_loader() -> Result<RemoteSessionCatalog, String> {
    LOAD_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    if FAILURES_BEFORE_SUCCESS.load(Ordering::SeqCst) > 0 {
        FAILURES_BEFORE_SUCCESS.fetch_sub(1, Ordering::SeqCst);
        return Err("invalid remote session catalog: disk I/O error".to_string());
    }
    Ok(RemoteSessionCatalog::in_memory())
}

fn always_failing_loader() -> Result<RemoteSessionCatalog, String> {
    LOAD_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    Err("invalid remote session catalog: disk I/O error".to_string())
}

#[test]
fn catalog_recovers_once_the_underlying_failure_clears() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    LOAD_ATTEMPTS.store(0, Ordering::SeqCst);
    // 启动时一次、之后的访问再一次：故障要持续到第一次访问之后，才能证明
    // 「访问路径上真的重试了」而不是构造函数碰巧成功。
    FAILURES_BEFORE_SUCCESS.store(2, Ordering::SeqCst);

    let mut state = RemoteCatalogState::with_loader(flaky_loader);
    let error = state
        .catalog()
        .expect_err("故障还在时必须继续 fail-closed，不能假装目录是空的");
    assert!(error.contains("disk I/O error"), "错误原文要透传给调用方");

    // 文件层面的故障被修好了（比如孤儿 -shm 被清掉）：下一次访问就该恢复，
    // 而不是等用户重启 daemon。
    assert!(state.catalog().is_ok(), "故障消失后必须自己恢复");
    assert!(
        state.catalog_mut().is_ok(),
        "恢复之后写入路径同样要放行，否则远程生命周期命令仍然被拒"
    );
}

#[test]
fn a_persistent_failure_stays_fail_closed_and_keeps_retrying() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    LOAD_ATTEMPTS.store(0, Ordering::SeqCst);

    let mut state = RemoteCatalogState::with_loader(always_failing_loader);
    for _ in 0..3 {
        assert!(
            state.catalog().is_err(),
            "目录读不出来时绝不能退化成空目录——那会让远程命令覆盖掉未知内容"
        );
    }
    // 构造 1 次 + 三次访问各 1 次：失败状态下每次访问都要真的再试，
    // 否则故障修好了也走不出来。
    assert_eq!(LOAD_ATTEMPTS.load(Ordering::SeqCst), 4);
}

#[test]
fn a_healthy_catalog_is_never_reloaded() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    LOAD_ATTEMPTS.store(0, Ordering::SeqCst);
    FAILURES_BEFORE_SUCCESS.store(0, Ordering::SeqCst);

    let mut state = RemoteCatalogState::with_loader(flaky_loader);
    for _ in 0..5 {
        assert!(state.catalog().is_ok());
    }
    assert_eq!(
        LOAD_ATTEMPTS.load(Ordering::SeqCst),
        1,
        "已经加载好的目录不能被重读：那会丢掉内存里尚未落盘的改动"
    );
}
