//! 本机运行时：`$SMELT_HOME/smeltd.sock` 上的 smeltd。桌面、命令行连本机运行时
//! 都从这里拿客户端；多运行时之前只有这一个。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::{LocalConnector, RuntimeClient};

/// smeltd 的 unix socket 路径（`~/.smelt/smeltd.sock`），用时顺手建好父目录。
///
/// 只给本机运行时的生命周期管理用（拉起、清理残留 socket、交给 agent 子进程）。
/// 连运行时一律走 [`local_runtime`]。
pub fn smeltd_sock_path() -> PathBuf {
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    let _ = std::fs::create_dir_all(&dir);
    dir.join("smeltd.sock")
}

/// 本机运行时的协议客户端。
pub fn local_runtime() -> RuntimeClient {
    RuntimeClient::new(LocalConnector::new(smeltd_sock_path()))
}

const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// 向本机 smeltd 发一个短请求并等待一行 JSON 回包。适合所有 daemon-owned 小型领域
/// 命令；长连接流式协议（terminal / ACP / subscribe）各自管理 socket 生命周期。
pub fn daemon_request(request: Value) -> Result<Value, String> {
    daemon_request_with_timeout(request, DAEMON_REQUEST_TIMEOUT)
}

pub fn daemon_request_with_timeout(request: Value, timeout: Duration) -> Result<Value, String> {
    local_runtime().request_ok(&request, timeout)
}

/// 不看 `ok` 字段，原样返回回包。
pub fn daemon_response_with_timeout(request: Value, timeout: Duration) -> Result<Value, String> {
    local_runtime()
        .request(&request, timeout)
        .map_err(|error| error.to_string())
}
