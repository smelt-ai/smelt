//! GUI（未来 web/mobile 也走这条）→ smeltd 的 ACP 会话客户端：连
//! `acp_open`，收 `AcpSnapshot` 流，发 `AcpUserAction`。跟 `acp_conn.rs` 是
//! 同一层次的东西，但这边连的是 smeltd 的 unix socket，不是子进程 agent
//! 自己——smeltd 才持有真正的连接（见 acp_session.rs 文件头：`Permission`/
//! `Elicitation` 的 responder 绑在连接线程上，没法跨进程传，所以 GUI 这层
//! 只能是「发指令、收结果」的薄客户端）。
//!
//! 每次 `acp_open` 一条专用 OS 线程（连接 + 读循环）+ 一条专用 OS 线程（写，
//! 转发 `action_rx`），跟 `acp_conn::spawn_acp` 同一种「一个会话一条线程」的
//! 分工。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use crate::acp_session::{AcpPhase, AcpSnapshot, AcpUserAction};
use crate::agent_kind::AcpLaunchSpec;

/// 一次 `acp_open` 的启动参数。`agent_id` 是 `AcpAgentKind::id()` 那串小写
/// 标识（"claude"/"codex"/"copilot"/"grok"），smeltd 只靠它判断
/// `resume_needs_transcript_check`（只有 claude 该为 true），不需要知道更多。
pub struct AcpClientLaunch {
    pub id: String,
    pub cwd: Option<String>,
    pub launch: AcpLaunchSpec,
    pub agent_id: String,
    /// 首次以「继续历史会话」打开时带上；已经连过一次之后 smeltd 自己记得
    /// agent 侧真实的 session id，这个字段只在“smeltd 也不认识这个 id”时
    /// （比如它刚重启过）才会被用上。
    pub resume_id: Option<String>,
}

fn legacy_cmd_fallback(launch: &AcpLaunchSpec) -> Option<String> {
    if launch.command.trim().is_empty() {
        return None;
    }
    if launch.env.is_empty() {
        return Some(launch.command.clone());
    }
    if launch.env.iter().any(|(name, value)| {
        name.contains(char::is_whitespace) || value.contains(char::is_whitespace)
    }) {
        return None;
    }
    let prefix = launch
        .env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("{prefix} {}", launch.command))
}

fn acp_open_request(launch: &AcpClientLaunch) -> serde_json::Value {
    let mut req = serde_json::json!({
        "op": "acp_open",
        "id": &launch.id,
        "cwd": &launch.cwd,
        "launch": &launch.launch,
        "agent": &launch.agent_id,
        "resume_id": &launch.resume_id,
    });
    if let Some(cmd) = legacy_cmd_fallback(&launch.launch) {
        req["cmd"] = serde_json::Value::String(cmd);
    }
    req
}

pub struct AcpClientHandle {
    pub action_tx: smol::channel::Sender<AcpUserAction>,
    pub snapshot_rx: smol::channel::Receiver<AcpSnapshot>,
    /// 连接建立后由后台线程填进来（建连是异步的，构造 `AcpClientHandle` 时
    /// 还没有 fd 可存）。Drop 时用它主动 `shutdown()`：读/写两条线程各自
    /// `try_clone()` 了一份来跑，克隆的 fd 各自独立，只 drop 掉 channel 端
    /// 不会让底层 socket 真正关闭（POSIX `dup()` 语义）；`shutdown()` 才会让
    /// 底层 socket 立刻对所有克隆失效，两条线程的阻塞读写各自出错退出，
    /// smeltd 那边也会读到 EOF 摘掉 `out.client`。**不发送任何"结束会话"的
    /// 指令**——这正是这一整层要解决的问题：GUI 断开只是摘连接，会话在
    /// smeltd 里照样活着。
    ///
    /// 已知的小窗口：如果 `AcpClientHandle`在后台线程完成连接**之前**就被
    /// drop（视图创建后立刻销毁，理论上可能但极罕见），这份 cell 还是空的，
    /// 主动 shutdown 就落空了；好在写线程会因为 `action_tx` 被 drop 而在
    /// `action_rx.recv()` 处自然退出，读线程仍会孤儿般地占着连接直到 smeltd
    /// 那边写超时/进程退出才收尾——代价可接受，不值得为这个窗口引入同步握手
    /// （那会让每次开 ACP 标签都卡一次 socket round-trip）。
    conn_cell: Arc<Mutex<Option<UnixStream>>>,
}

impl Drop for AcpClientHandle {
    fn drop(&mut self) {
        if let Some(c) = self.conn_cell.lock().ok().and_then(|mut g| g.take()) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn fallback_snapshot(reason: &str) -> AcpSnapshot {
    AcpSnapshot {
        entries: Vec::new(),
        phase: AcpPhase::Ended(reason.to_string()),
        pending_permission: None,
        pending_elicitation: None,
        status_line: None,
        acp_session_id: None,
        supports_image: true,
        available_commands: Vec::new(),
        usage: None,
        plan: None,
        model: None,
        completed_unread: false,
        // 连接终态（连不上 smeltd / 握手失败 / 断线）值得存盘，跟旧版 Fatal
        // 事件一样不在"跳过持久化"的名单里。
        should_persist: true,
    }
}

/// 连 smeltd 的 `acp_open`，起连接线程，立即返回（不阻塞调用方——旧版
/// `spawn_acp` 就是这个约定，「握手结果以事件回来」）。连不上 smeltd、握手
/// 失败都不 panic，而是塞一份 `Ended` 快照进 `snapshot_rx`，跟
/// `acp_conn::spawn_acp` 遇到起不来的情况一律走 `AcpEvent::Fatal` 是同一个
/// 约定，调用方（GUI 视图）只需要处理"连不上"和"agent 本身连不上"两种一样
/// 的终态展示，不用分别处理。
pub fn spawn_acp_client(launch: AcpClientLaunch) -> AcpClientHandle {
    let (action_tx, action_rx) = smol::channel::unbounded::<AcpUserAction>();
    let (snapshot_tx, snapshot_rx) = smol::channel::unbounded::<AcpSnapshot>();
    let conn_cell: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let conn_cell_for_thread = Arc::clone(&conn_cell);

    let thread_name = format!("smelt-acp-cli-{}", &launch.id[..launch.id.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let sock_path = crate::daemon_state::smeltd_sock_path();
            let conn = match UnixStream::connect(&sock_path) {
                Ok(c) => c,
                Err(e) => {
                    let _ = snapshot_tx.try_send(fallback_snapshot(&format!("连不上 smeltd：{e}")));
                    return;
                }
            };
            let Ok(mut writer) = conn.try_clone() else {
                return;
            };
            let req = acp_open_request(&launch);
            if writeln!(writer, "{req}").is_err() {
                let _ = snapshot_tx.try_send(fallback_snapshot("向 smeltd 发起会话失败"));
                return;
            }
            // 握手请求已经发出去才把 conn 交给 Drop 兜底——期间若 handle 已经
            // 被 drop，这里存进去的 fd 会在下一次 Drop 检查前一直占着，直到本
            // 函数走完自然退出（读循环会因为对端没人理而超时/断开），不是
            // 永久泄漏。
            if let Ok(mut cell) = conn_cell_for_thread.lock() {
                *cell = conn.try_clone().ok();
            }

            // 写线程：把 action_rx 里的动作逐条转发成 JSON 行。写端独立于读端的
            // socket 克隆，两个方向互不阻塞（同一 fd 的读写本来就是独立的）。
            std::thread::spawn(move || {
                smol::block_on(async move {
                    while let Ok(action) = action_rx.recv().await {
                        let Ok(line) = serde_json::to_string(&action) else {
                            continue;
                        };
                        if writeln!(writer, "{line}").is_err() {
                            return;
                        }
                    }
                });
            });

            // 读循环：逐行 JSON 解出 `{"snapshot": AcpSnapshot}`。
            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                let Some(snap_v) = v.get("snapshot") else {
                    continue;
                };
                let Ok(snap) = serde_json::from_value::<AcpSnapshot>(snap_v.clone()) else {
                    continue;
                };
                if snapshot_tx.try_send(snap).is_err() {
                    return; // 接收端（GUI 视图）没了
                }
            }
            let _ = snapshot_tx.try_send(fallback_snapshot("与 smeltd 的连接已断开"));
        })
        .expect("spawn acp client thread");

    AcpClientHandle {
        action_tx,
        snapshot_rx,
        conn_cell,
    }
}

/// 显式结束一个 smeltd 托管的 ACP 会话：杀子进程、摘表、踢掉所有连接
/// （`acp_kill` op）。跟 `AcpClientHandle` 的 Drop **不是**同一件事——Drop
/// 只是"这个客户端不再关心这个会话了"，会话本身照样在 smeltd 里活着；这个
/// 函数才是真的把会话终结掉，只在用户明确要求"结束这段对话"（比如点 ×
/// 关掉标签）时调用，跟 GUI 退出/切标签这种"我先不看了"完全是两回事。
///
/// 阻塞：等守护回执再返回，跟 `terminal::kill_remote` 同一个理由——避免
/// 关闭动作和后续可能的 App 退出之间有个窗口，kill 命令还没送达就被中断。
pub fn kill_acp_session(id: &str) {
    let Ok(mut s) = UnixStream::connect(crate::daemon_state::smeltd_sock_path()) else {
        return;
    };
    let _ = writeln!(s, "{}", serde_json::json!({ "op": "acp_kill", "id": id }));
    let mut resp = String::new();
    let _ = BufReader::new(s).read_line(&mut resp);
}

#[cfg(test)]
mod tests {
    use super::{AcpClientLaunch, acp_open_request};
    use crate::agent_kind::AcpLaunchSpec;

    #[test]
    fn acp_open_request_serializes_structured_launch() {
        let req = acp_open_request(&AcpClientLaunch {
            id: "acp-1".into(),
            cwd: Some("/repo".into()),
            launch: AcpLaunchSpec::from_command("claude --print")
                .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
            agent_id: "claude".into(),
            resume_id: Some("resume-1".into()),
        });

        assert_eq!(req["op"], "acp_open");
        assert_eq!(req["launch"]["command"], "claude --print");
        assert_eq!(
            req["launch"]["env"]["CLAUDE_CONFIG_DIR"],
            "~/Claude Workspaces/quant"
        );
        assert!(req.get("cmd").is_none(), "新协议不该再发旧 cmd 字段");
    }

    #[test]
    fn acp_open_request_keeps_plain_cmd_fallback_for_old_daemons() {
        let req = acp_open_request(&AcpClientLaunch {
            id: "acp-2".into(),
            cwd: Some("/repo".into()),
            launch: AcpLaunchSpec::from_command("claude --print"),
            agent_id: "claude".into(),
            resume_id: None,
        });

        assert_eq!(req["launch"]["command"], "claude --print");
        assert_eq!(req["cmd"], "claude --print");
    }
}
