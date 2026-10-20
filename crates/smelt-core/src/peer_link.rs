//! 会话宿主向主 daemon 转交 Pi 会话互发的请求（`session_list` / `session_read` / `session_send` / `session_spawn`）。
//!
//! 一个会话宿主进程只托管一个对话，所以通道是进程级的：宿主启动时 [`install`] 一个
//! 往控制连接写行的函数，扩展的请求经 [`request`] 写成一行
//! `{"peer_request": {"request_id", "op", "body"}}`，主 daemon 用 `PeerReply` 动作回答，
//! 宿主的动作循环交给 [`complete`]。来源由主 daemon 按这条控制连接认定，请求里不带来源。
//!
//! 没装通道（不在独立会话宿主里跑，比如同进程的测试注册表）时，请求直接失败。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

type Uplink = Box<dyn Fn(serde_json::Value) -> Result<(), String> + Send + Sync>;

static UPLINK: OnceLock<Uplink> = OnceLock::new();
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);

fn waiters() -> &'static Mutex<HashMap<u64, mpsc::SyncSender<serde_json::Value>>> {
    static WAITERS: OnceLock<Mutex<HashMap<u64, mpsc::SyncSender<serde_json::Value>>>> =
        OnceLock::new();
    WAITERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 装上往主 daemon 写一行的函数。一个进程只装一次，再装不生效。
pub fn install(uplink: impl Fn(serde_json::Value) -> Result<(), String> + Send + Sync + 'static) {
    let _ = UPLINK.set(Box::new(uplink));
}

/// 把请求交给主 daemon，等它回答。回答原样返回：`{"ok": true, ...}` 或 `{"ok": false, "error"}`。
pub fn request(
    op: &str,
    body: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let uplink = UPLINK
        .get()
        .ok_or_else(|| "这个对话不在独立会话宿主里，不能和别的对话通信".to_string())?;
    let request_id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = mpsc::sync_channel(1);
    waiters().lock().unwrap().insert(request_id, tx);
    let line = serde_json::json!({
        "peer_request": {
            "request_id": request_id,
            "op": op,
            "body": body,
        }
    });
    if let Err(error) = uplink(line) {
        waiters().lock().unwrap().remove(&request_id);
        return Err(error);
    }
    match rx.recv_timeout(timeout) {
        Ok(reply) => Ok(reply),
        Err(_) => {
            waiters().lock().unwrap().remove(&request_id);
            Err("主 daemon 没有及时回答".to_string())
        }
    }
}

/// 主 daemon 的回答到了。没人等（已经超时）就丢掉。
pub fn complete(request_id: u64, reply: serde_json::Value) {
    if let Some(waiter) = waiters().lock().unwrap().remove(&request_id) {
        let _ = waiter.try_send(reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_reaches_the_waiting_request_and_late_replies_are_dropped() {
        let (sent_tx, sent_rx) = mpsc::channel::<serde_json::Value>();
        let sent_tx = Mutex::new(sent_tx);
        install(move |line| {
            sent_tx
                .lock()
                .unwrap()
                .send(line)
                .map_err(|e| e.to_string())
        });
        let answering = std::thread::spawn(move || {
            let line = sent_rx.recv().unwrap();
            let request = &line["peer_request"];
            assert_eq!(request["op"], "session_list");
            assert_eq!(request["body"]["query"], "e2e");
            let id = request["request_id"].as_u64().unwrap();
            complete(id, serde_json::json!({"ok": true, "sessions": []}));
            // 晚到的、没人等的回答不能留在等待表里。
            complete(id + 1000, serde_json::json!({"ok": true}));
        });
        let reply = request(
            "session_list",
            serde_json::json!({"query": "e2e"}),
            Duration::from_secs(5),
        )
        .unwrap();
        answering.join().unwrap();
        assert_eq!(reply["ok"], true);
        assert!(waiters().lock().unwrap().is_empty());
    }
}
