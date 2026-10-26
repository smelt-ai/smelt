//! 停掉本机 smeltd 并等它真的退出。设置页的「重启守护进程」和替换 `.app` 前的停机共用这一份。

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use smelt_sys::process::TreeSignal;

/// 优雅停机先收割子进程、checkpoint 状态库，最后才回执。停机第二步只为进行中的自动化
/// 向宿主要全文，第三步并发收割，宽限几秒，这个上限盖得住。
const SHUTDOWN_REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// 回执之后进程退出、或强杀之后被内核收走，各等这么久。
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// 请本机守护优雅停机，等它退出；不退就强杀它的进程树。返回守护是否已经不在了。
///
/// 守护太旧、`version` 不报 pid 时，只能看 socket 还连不连得上。
pub fn stop_local_daemon() -> bool {
    let runtime = crate::daemon_state::local_runtime();
    let pid = daemon_pid(&runtime);
    if let Ok(mut stream) = runtime.connect_with_timeout(Duration::from_secs(2)) {
        let _ = stream.set_read_timeout(Some(SHUTDOWN_REPLY_TIMEOUT));
        let request = serde_json::json!({ "op": DaemonOperation::Shutdown });
        if writeln!(stream, "{request}").is_ok() {
            let mut reply = String::new();
            let _ = BufReader::new(stream).read_line(&mut reply);
        }
    }
    let Some(pid) = pid else {
        return runtime
            .connect_with_timeout(Duration::from_millis(200))
            .is_err();
    };
    let pid_i32 = pid as i32;
    if smelt_sys::process_wait::wait_until_exit(pid_i32, EXIT_GRACE) {
        return true;
    }
    let _ = smelt_sys::process::signal_tree(pid, TreeSignal::Kill);
    let _ = smelt_sys::process::terminate_pid(pid);
    smelt_sys::process_wait::wait_until_exit(pid_i32, EXIT_GRACE)
}

fn daemon_pid(runtime: &RuntimeClient) -> Option<u32> {
    let mut stream = runtime
        .connect_with_timeout(Duration::from_millis(500))
        .ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = serde_json::json!({ "op": DaemonOperation::Version });
    writeln!(stream, "{request}").ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    value["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok())
}
