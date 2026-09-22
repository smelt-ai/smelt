//! GUI（未来 web/mobile 也走这条）→ smeltd 的 ACP 会话客户端：连
//! `acp_open`，收 `ConversationSnapshot` 流，发 `AcpUserAction`。跟 `acp_conn.rs` 是
//! 同一层次的东西，但这边连的是 smeltd 的 unix socket，不是子进程 agent
//! 自己——smeltd 才持有真正的连接（见 acp_session 模块（`crates/smelt-core/src/acp_session/mod.rs`）文件头：`Permission`/
//! `Elicitation` 的 responder 绑在连接线程上，没法跨进程传，所以 GUI 这层
//! 只能是「发指令、收结果」的薄客户端）。
//!
//! 每次 `acp_open` 一条专用 OS 线程（连接 + 读循环）+ 一条专用 OS 线程（写，
//! 转发 `action_rx`），跟 `acp_conn::spawn_acp` 同一种「一个会话一条线程」的
//! 分工。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::acp_session::{AcpEndKind, AcpUserAction, ConversationSnapshot};
use crate::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
use crate::daemon_protocol::DaemonOperation;
use crate::daemon_state::DaemonPhase;

pub const ACP_INITIAL_TAIL_LIMIT: usize = 100;
pub const ACP_HISTORY_PAGE_LIMIT: usize = 100;
const ACP_ACTION_QUEUE_CAPACITY: usize = 256;

/// 短生命周期 ACP 控制请求的上限。调用方已经在后台执行，但 daemon 不响应时仍要
/// 及时释放 worker，不能把关闭、重启或翻历史的任务永久挂住。
const ACP_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

fn connect_acp_control() -> Result<UnixStream, String> {
    let stream = UnixStream::connect(crate::daemon_state::smeltd_sock_path())
        .map_err(|e| format!("连不上 smeltd：{e}"))?;
    stream
        .set_read_timeout(Some(ACP_CONTROL_TIMEOUT))
        .map_err(|e| format!("设置读取超时失败：{e}"))?;
    stream
        .set_write_timeout(Some(ACP_CONTROL_TIMEOUT))
        .map_err(|e| format!("设置写入超时失败：{e}"))?;
    Ok(stream)
}

/// 一次 `acp_open` 的启动参数。执行引擎仍以 `ConversationAgentKind::id()` 那串小写标识
/// 随请求发送，以兼容旧 smeltd
/// 的 handoff 数据，恢复历史本身只依赖 agent 的 `session/load`。
pub struct ConversationClientLaunch {
    pub id: String,
    pub cwd: Option<String>,
    pub launch: ConversationLaunchSpec,
    pub engine_kind: ConversationAgentKind,
    /// 仅本次 ACP 子进程可见的环境变量。controller 的远端配置可能含凭据，
    /// 因而这项不能并入可持久化的 `ConversationLaunchSpec`。
    pub ephemeral_env: BTreeMap<String, String>,
    /// 首次以「继续历史会话」打开时带上；已经连过一次之后 smeltd 自己记得
    /// agent 侧真实的 session id，这个字段只在“smeltd 也不认识这个 id”时
    /// （比如它刚重启过）才会被用上。
    pub resume_id: Option<String>,
    /// Pi 原生 `--fork` 的源 session id。新开会话，不占用源 session 的独占锁。
    pub fork_id: Option<String>,
    /// 与 `fork_id` 搭配的分叉切点：整拷打开后、重放前切到该用户消息之前。
    /// 见 `acp_conn::AcpForkCut`。
    pub fork_cut: Option<crate::acp_conn::AcpForkCut>,
    /// 建连前 GUI 已持有的连续历史末端（全局 offset，不是本地 Vec 长度）。若
    /// 第一份 daemon 快照到达前连接失败，终态快照用它作为 offset，避免把仍可
    /// 展示的分页历史误清空。
    pub retained_entries_end: usize,
    /// 新 daemon 会话的交互输入路由。已有会话 attach 时 daemon 自己的状态优先，
    /// 因此客户端存档不会覆盖一个热会话的当前 binding。
    pub conversation_binding: crate::conversation::ConversationBinding,
    /// 产品级智能体会话身份。它与执行 ACP 的 provider 分离；已有会话 attach
    /// 时同样以 daemon 的实时状态为准，只用于 daemon 丢失后的冷恢复。
    pub agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    /// 只在新 daemon 会话创建时登记。热 attach 不会用客户端的旧存档覆盖 daemon
    /// 已消费的状态。
    pub pending_agent_preset: Option<String>,
}

fn acp_open_request(launch: &ConversationClientLaunch) -> serde_json::Value {
    let mut req = serde_json::json!({
        "op": DaemonOperation::AcpOpen,
        "id": &launch.id,
        "cwd": &launch.cwd,
        "launch": &launch.launch,
        "agent": launch.engine_kind.id(),
        "ephemeral_env": &launch.ephemeral_env,
        "resume_id": &launch.resume_id,
        "fork_id": &launch.fork_id,
        "fork_cut": &launch.fork_cut,
        "tail_limit": ACP_INITIAL_TAIL_LIMIT,
        "conversation_binding": &launch.conversation_binding,
    });
    if let Some(agent_session) = &launch.agent_session {
        req["agent_session"] =
            serde_json::to_value(agent_session).expect("AgentSessionBinding must serialize");
    }
    if let Some(prompt) = &launch.pending_agent_preset {
        req["pending_agent_preset"] = serde_json::Value::String(prompt.clone());
    }
    req
}

pub struct ConversationClientHandle {
    pub action_tx: smol::channel::Sender<AcpUserAction>,
    pub snapshot_rx: smol::channel::Receiver<ConversationSnapshot>,
    /// 连接建立后由后台线程填进来（建连是异步的，构造 `ConversationClientHandle` 时
    /// 还没有 fd 可存）。Drop 时用它在后台主动 `shutdown()`：读/写两条线程各自
    /// `try_clone()` 了一份来跑，克隆的 fd 各自独立，只 drop 掉 channel 端
    /// 不会让底层 socket 真正关闭（POSIX `dup()` 语义）；`shutdown()` 才会让
    /// 底层 socket 立刻对所有克隆失效，两条线程的阻塞读写各自出错退出，
    /// smeltd 那边也会读到 EOF 摘掉 `out.client`。**不发送任何"结束会话"的
    /// 指令**——这正是这一整层要解决的问题：GUI 断开只是摘连接，会话在
    /// smeltd 里照样活着。
    ///
    /// 已知的小窗口：如果 `ConversationClientHandle`在后台线程完成连接**之前**就被
    /// drop（视图创建后立刻销毁，理论上可能但极罕见），这份 cell 还是空的，
    /// 主动 shutdown 就落空了；好在写线程会因为 `action_tx` 被 drop 而在
    /// `action_rx.recv()` 处自然退出，读线程仍会孤儿般地占着连接直到 smeltd
    /// 那边写超时/进程退出才收尾——代价可接受，不值得为这个窗口引入同步握手
    /// （那会让每次开 ACP 标签都卡一次 socket round-trip）。
    conn_cell: Arc<Mutex<Option<UnixStream>>>,
}

impl Drop for ConversationClientHandle {
    fn drop(&mut self) {
        // `Drop` 经常由 GPUI 主线程触发。获取 cell 的锁和 `shutdown` 都可能进入内核，
        // 所以把完整的收尾移到后台；视图本身可以立刻销毁，不必等 daemon 消费断开。
        let conn_cell = Arc::clone(&self.conn_cell);
        let close = std::thread::Builder::new()
            .name("smelt-acp-client-close".into())
            .spawn(move || {
                if let Some(c) = conn_cell.lock().ok().and_then(|mut g| g.take()) {
                    let _ = c.shutdown(std::net::Shutdown::Both);
                }
            });
        if close.is_err() {
            // 线程资源耗尽时不能把 socket 留给连接/读取线程；同步回退仍是有界的
            // （这里只拿进程内 Mutex 并调用 shutdown，不等待 daemon 回执）。
            if let Some(c) = self
                .conn_cell
                .lock()
                .ok()
                .and_then(|mut guard| guard.take())
            {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

fn fallback_snapshot(reason: &str, entries_offset: usize) -> ConversationSnapshot {
    ConversationSnapshot {
        // 断线只是 GUI 与 smeltd 的传输终止，不代表 daemon 中的会话历史消失。
        // 用已接收连续历史的全局末端作为增量偏移，AcpView 会保留现有 entries，
        // 只更新终态。
        entries_offset,
        entries_total: entries_offset,
        snapshot_revision: 0,
        session_title: None,
        replaying_history: false,
        entries: Vec::new(),
        tool_debug: Default::default(),
        runtime_debug: Default::default(),
        phase: DaemonPhase::Dead,
        end_reason: reason.to_string(),
        end_kind: AcpEndKind::TransportDisconnected,
        accepted_delivery_ids: Default::default(),
        active_delivery_id: None,
        completed_delivery_id: None,
        pending_permissions: Vec::new(),
        pending_elicitation: None,
        status_line: None,
        acp_session_id: None,
        history_session_id: None,
        supports_image: true,
        available_commands: Vec::new(),
        usage: None,
        usage_cached_read: None,
        usage_cost: None,
        usage_breakdown: None,
        plan: None,
        model: None,
        config_options: Vec::new(),
        conversation_state: None,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
        compacting: false,
        queued_steering: Vec::new(),
        queued_follow_up: Vec::new(),
        composer_restore_revision: 0,
        composer_restore_texts: Vec::new(),
        turn_started_at_ms: None,
        turn_timings: Vec::new(),
        completed_unread: false,
        turn_outcome: None,
        // 连接终态（连不上 smeltd / 握手失败 / 断线）值得存盘，跟旧版 Fatal
        // 事件一样不在"跳过持久化"的名单里。
        should_persist: true,
    }
}

fn snapshot_after_stream_disconnect(
    last_phase: Option<DaemonPhase>,
    entries_offset: usize,
    session_title: Option<String>,
) -> Option<ConversationSnapshot> {
    // daemon 已经给出失败/结束终态时，它携带的 provider 错误才是根因；紧随其后的
    // EOF 只是连接关闭的结果，不能再用通用“连接已断开”快照把根因覆盖掉。
    if matches!(last_phase, Some(DaemonPhase::Dead | DaemonPhase::Failed)) {
        return None;
    }

    let mut disconnected = fallback_snapshot("与 smeltd 的连接已断开", entries_offset);
    disconnected.session_title = session_title;
    Some(disconnected)
}

fn acp_snapshot_request(id: &str, before: usize, limit: usize) -> serde_json::Value {
    serde_json::json!({
        "op": DaemonOperation::AcpSnapshot,
        "id": id,
        "before": before,
        "limit": limit,
    })
}

/// Read one bounded page immediately before `before` without disturbing the long-lived
/// control connection. The caller runs this blocking helper off the GPUI thread.
pub fn load_acp_history(
    id: &str,
    before: usize,
    limit: usize,
) -> Result<ConversationSnapshot, String> {
    let mut stream = connect_acp_control()?;
    writeln!(stream, "{}", acp_snapshot_request(id, before, limit))
        .map_err(|e| format!("读取历史请求发送失败：{e}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| format!("读取历史失败：{e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|_| "历史快照解析失败".to_string())?;
    serde_json::from_value(
        value
            .get("snapshot")
            .cloned()
            .ok_or_else(|| "ACP 会话不存在".to_string())?,
    )
    .map_err(|_| "历史快照内容无效".to_string())
}

/// 连 smeltd 的 `acp_open`，起连接线程，立即返回（不阻塞调用方——旧版
/// `spawn_acp` 就是这个约定，「握手结果以事件回来」）。连不上 smeltd、握手
/// 失败都不 panic，而是塞一份 `Ended` 快照进 `snapshot_rx`，跟
/// `acp_conn::spawn_acp` 遇到起不来的情况一律走 `ConversationEvent::Fatal` 是同一个
/// 约定，调用方（GUI 视图）只需要处理"连不上"和"agent 本身连不上"两种一样
/// 的终态展示，不用分别处理。
pub fn spawn_acp_client(launch: ConversationClientLaunch) -> ConversationClientHandle {
    // UI 回调只做 `try_send`。daemon 停止读 action 时，既不能等待，也不能无限积压
    // 大型图片 prompt；队列满后视图保留草稿，连接恢复后用户可再次提交。
    let (action_tx, action_rx) = smol::channel::bounded::<AcpUserAction>(ACP_ACTION_QUEUE_CAPACITY);
    let (snapshot_tx, snapshot_rx) = smol::channel::unbounded::<ConversationSnapshot>();
    let conn_cell: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let conn_cell_for_thread = Arc::clone(&conn_cell);

    let thread_name = format!("smelt-acp-cli-{}", &launch.id[..launch.id.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let mut known_entries_end = launch.retained_entries_end;
            let mut known_session_title = None;
            let mut last_phase = None;
            let sock_path = crate::daemon_state::smeltd_sock_path();
            let conn = match UnixStream::connect(&sock_path) {
                Ok(c) => c,
                Err(e) => {
                    let _ = snapshot_tx.try_send(fallback_snapshot(
                        &format!("连不上 smeltd：{e}"),
                        known_entries_end,
                    ));
                    return;
                }
            };
            let Ok(mut writer) = conn.try_clone() else {
                let _ = snapshot_tx.try_send(fallback_snapshot(
                    "无法建立 smeltd 双向连接",
                    known_entries_end,
                ));
                return;
            };
            // UI 端只做 try_send，实际写在这条后台线程；仍给它一个上限，避免 daemon
            // 停止读 action 后无限占住线程并让动作队列持续增长。
            let _ = writer.set_write_timeout(Some(ACP_CONTROL_TIMEOUT));
            let req = acp_open_request(&launch);
            if writeln!(writer, "{req}").is_err() {
                let _ = snapshot_tx.try_send(fallback_snapshot(
                    "向 smeltd 发起会话失败",
                    known_entries_end,
                ));
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

            // 读循环：逐行 JSON 解出 `{"snapshot": ConversationSnapshot}`。
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
                let Ok(snap) = serde_json::from_value::<ConversationSnapshot>(snap_v.clone())
                else {
                    continue;
                };
                known_entries_end = snap.entries_offset.saturating_add(snap.entries.len());
                known_session_title = snap.session_title.clone();
                last_phase = Some(snap.phase);
                if snapshot_tx.try_send(snap).is_err() {
                    return; // 接收端（GUI 视图）没了
                }
            }
            if let Some(disconnected) =
                snapshot_after_stream_disconnect(last_phase, known_entries_end, known_session_title)
            {
                let _ = snapshot_tx.try_send(disconnected);
            }
        })
        .expect("spawn acp client thread");

    ConversationClientHandle {
        action_tx,
        snapshot_rx,
        conn_cell,
    }
}

/// 显式结束一个 smeltd 托管的 ACP 会话：杀子进程、摘表、踢掉所有连接
/// （`acp_kill` op）。跟 `ConversationClientHandle` 的 Drop **不是**同一件事——Drop
/// 只是"这个客户端不再关心这个会话了"，会话本身照样在 smeltd 里活着；这个
/// 函数才是真的把会话终结掉，只在用户明确要求"结束这段对话"（比如点 ×
/// 关掉标签）时调用，跟 GUI 退出/切标签这种"我先不看了"完全是两回事。
///
/// 阻塞：等守护回执再返回，跟 `terminal::kill_remote` 同一个理由——避免
/// 关闭动作和后续可能的 App 退出之间有个窗口，kill 命令还没送达就被中断。
pub fn kill_acp_session(id: &str) {
    let Ok(mut s) = connect_acp_control() else {
        return;
    };
    let _ = writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::AcpKill, "id": id })
    );
    let mut resp = String::new();
    let _ = BufReader::new(s).read_line(&mut resp);
}

/// 强制重启一个卡死的 ACP 会话：杀掉当前 agent 子进程（整个进程组），换一个
/// 新的接着跑，带 `resume_session_id` 走 `session/load` 接回同一份历史。跟
/// `kill_acp_session` 不是一回事——那个是终结会话（关标签），这个是会话本体
/// （标签、GUI 那条 `acp_open` 连接、entries 历史）原样保留，只是换掉失联的
/// 那个子进程，专治 `session/cancel` 打不断的死循环工具调用。
///
/// 阻塞等守护回执，理由同 `kill_acp_session`：调用方（GUI 点"强制重启"）想要
/// 一个确定的成功/失败结果，不想留下"到底发出去没有"的悬念。
pub fn restart_acp_session(id: &str) -> Result<(), String> {
    let mut s = connect_acp_control()?;
    writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::AcpRestart, "id": id })
    )
    .map_err(|e| format!("写请求失败：{e}"))?;
    let mut resp = String::new();
    BufReader::new(s)
        .read_line(&mut resp)
        .map_err(|e| format!("读回执失败：{e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(resp.trim()).map_err(|_| "回执解析失败".to_string())?;
    if v["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(v["error"].as_str().unwrap_or("重启失败").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACP_INITIAL_TAIL_LIMIT, ConversationClientLaunch, acp_open_request, acp_snapshot_request,
        snapshot_after_stream_disconnect,
    };
    use crate::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
    use std::collections::BTreeMap;

    #[test]
    fn acp_open_request_serializes_structured_launch() {
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-1".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("claude --print")
                .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
            engine_kind: ConversationAgentKind::Claude,
            ephemeral_env: [("PLUGIN_TOKEN".into(), "transient".into())]
                .into_iter()
                .collect(),
            resume_id: Some("resume-1".into()),
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: crate::conversation::ConversationBinding::Direct,
            agent_session: None,
            pending_agent_preset: None,
        });

        assert_eq!(req["op"], "acp_open");
        assert_eq!(req["resume_id"], "resume-1");
        assert!(req["fork_id"].is_null());
        assert_eq!(req["tail_limit"], ACP_INITIAL_TAIL_LIMIT);
        assert_eq!(req["launch"]["command"], "claude --print");
        assert_eq!(
            req["launch"]["env"]["CLAUDE_CONFIG_DIR"],
            "~/Claude Workspaces/quant"
        );
        assert_eq!(req["ephemeral_env"]["PLUGIN_TOKEN"], "transient");
        assert!(req.get("cmd").is_none(), "新协议不该再发旧 cmd 字段");
    }

    #[test]
    fn acp_open_request_does_not_emit_legacy_cmd() {
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-2".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("claude --print"),
            engine_kind: ConversationAgentKind::Claude,
            ephemeral_env: BTreeMap::new(),
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: crate::conversation::ConversationBinding::Direct,
            agent_session: None,
            pending_agent_preset: None,
        });

        assert_eq!(req["launch"]["command"], "claude --print");
        assert!(req.get("cmd").is_none(), "新协议不该再发旧 cmd 字段");
    }

    #[test]
    fn acp_open_request_serializes_a_generic_plugin_conversation_binding() {
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-plugin".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("codex"),
            engine_kind: ConversationAgentKind::Codex,
            ephemeral_env: BTreeMap::new(),
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: crate::conversation::ConversationBinding::Plugin {
                plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
                route: smelt_plugin_api::PluginInputRouteBinding {
                    contribution_id: smelt_plugin_api::ContributionId::new("thread-input").unwrap(),
                    context: serde_json::json!({"thread_id": "thread-1"}),
                },
            },
            agent_session: None,
            pending_agent_preset: None,
        });

        assert_eq!(req["conversation_binding"]["type"], "plugin");
        assert_eq!(req["conversation_binding"]["plugin_id"], "com.example.chat");
        assert_eq!(
            req["conversation_binding"]["route"]["context"]["thread_id"],
            "thread-1"
        );
    }

    #[test]
    fn acp_open_request_keeps_product_agent_separate_from_execution_provider() {
        let agent_session: smelt_plugin_api::AgentSessionBinding =
            serde_json::from_value(serde_json::json!({
                "agent": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-agent"
                },
                "controller": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-session"
                },
                "instance": {
                    "plugin_id": "com.example.quant",
                    "resource_type": "strategy",
                    "resource_id": "strategy-1"
                }
            }))
            .unwrap();
        let req = acp_open_request(&ConversationClientLaunch {
            id: "acp-quant".into(),
            cwd: Some("/repo".into()),
            launch: ConversationLaunchSpec::from_command("codex"),
            engine_kind: ConversationAgentKind::Codex,
            ephemeral_env: BTreeMap::new(),
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: crate::conversation::ConversationBinding::Direct,
            agent_session: Some(agent_session),
            pending_agent_preset: None,
        });

        assert_eq!(req["agent"], "codex");
        assert_eq!(
            req["agent_session"]["agent"]["plugin_id"],
            "com.example.quant"
        );
        assert_eq!(
            req["agent_session"]["agent"]["contribution_id"],
            "quant-agent"
        );
    }

    #[test]
    fn disconnect_snapshot_preserves_known_history() {
        let snapshot = snapshot_after_stream_disconnect(
            Some(crate::daemon_state::DaemonPhase::Idle),
            37,
            Some("修复启动失败".into()),
        )
        .expect("非终态断连仍应生成兜底快照");

        assert_eq!(snapshot.entries_offset, 37);
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.session_title.as_deref(), Some("修复启动失败"));
        assert!(matches!(
            snapshot.phase,
            crate::daemon_state::DaemonPhase::Dead
        ));
    }

    #[test]
    fn stream_eof_does_not_overwrite_a_provider_failure_snapshot() {
        assert!(
            snapshot_after_stream_disconnect(
                Some(crate::daemon_state::DaemonPhase::Dead),
                37,
                Some("修复启动失败".into()),
            )
            .is_none(),
            "daemon 已发送终态时，EOF 不能再覆盖成通用传输断连"
        );
    }

    #[test]
    fn history_request_is_bounded_before_the_loaded_prefix() {
        let req = acp_snapshot_request("acp-1", 900, 100);

        assert_eq!(req["op"], "acp_snapshot");
        assert_eq!(req["id"], "acp-1");
        assert_eq!(req["before"], 900);
        assert_eq!(req["limit"], 100);
    }
}
