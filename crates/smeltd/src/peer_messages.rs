//! Pi 对话互发消息：主 daemon 这一头（门卫）。设计见 `docs/pi-session-messaging-design.html`。
//!
//! 会话宿主把扩展的 `session_list` / `session_read` / `session_send` / `session_spawn`
//! 写成控制连接上的 `peer_request` 行。来源只认这条控制连接属于哪个注册表槽，请求里写什么都不算。
//! 门卫只在内存里检查，不落库：
//! - 只在 Pi 对话之间，不能发给自己，不碰自动化的会话；
//! - 「逐次审批」的对话不能给「完全访问」的对话发消息，否则就是借对方的权限做事；
//! - 对方还有 [`INBOX_LIMIT`] 条没送到就拒收；
//! - 每个对话记一个链预算，发一条减一，减到 0 拒绝；用户在这个对话里说话时复位。
//!   预算不从请求里取：模型能读到自己的环境，请求里的数都能伪造；
//! - 带 `idempotencyKey` 的，同一个来源、同一个 key 在对方最近的提交里只送一次。
//!
//! 通过后把 `DeliverPeerMessage` 交给对方的会话宿主，对方宿主排队、经扩展写进会话。
//! 挂起的对方先唤醒。送没送到看对方快照里的 `peer_inbox`。
//!
//! 起新对话（`session_spawn`）：照发起方的启动规格在同一个项目里建一个普通的侧栏对话，
//! 模型、推理强度、权限模式和发起方一样，再按 `task` 发第一条消息。每个对话同时最多
//! [`SPAWN_LIMIT`] 个；起出来的对话不能再起，防止一层层分下去。都只记在内存里。

use super::*;
use smelt_core::acp_session::{AcpUserAction, PeerDelivery, PeerMessageKind, PeerMessageStatus};

/// 用户说过话之后，一条消息链最多再转这么多跳。
const INITIAL_HOPS: u32 = 8;
/// 对方还有这么多条没送到就拒收。
const INBOX_LIMIT: usize = 8;
const TEXT_LIMIT_CHARS: usize = 32 * 1024;
const READ_LIMIT_MAX: usize = 5;
const READ_REPLY_CHARS: usize = 4_000;
/// 刚投出去、对方快照还没回来的那几条，最多算这么久。
const IN_FLIGHT_TTL: Duration = Duration::from_secs(30);
/// 一个对话同时开着的、它起的新对话最多这么多个。
const SPAWN_LIMIT: usize = 4;
const TITLE_LIMIT_CHARS: usize = 80;

/// 每个对话当前的链预算。没有记录就是 [`INITIAL_HOPS`]。
static CHAINS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);
/// 投给每个对话、它的快照里还没出现的消息。
static IN_FLIGHT: LazyLock<Mutex<HashMap<String, Vec<(String, Instant)>>>> =
    LazyLock::new(Default::default);
/// 每个对话起过、还在的新对话。值里出现过的对话不能再起。
static SPAWNED: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Default::default);

/// 用户在这个对话里说话了：别的对话发来的消息链到这里断开，预算复位。
pub(crate) fn reset_chain(session_id: &str) {
    CHAINS.lock().unwrap().remove(session_id);
}

/// 收这个宿主转上来的请求。每个请求单开线程：发送可能要等对方从挂起中醒来。
pub(crate) fn serve_host_requests(
    slot: Arc<AcpSlot<AcpSession>>,
    requests: smol::channel::Receiver<serde_json::Value>,
    session_id: String,
    acp_sessions: AcpSessions,
    subscribers: EventHubHandle,
) {
    let _ = thread::Builder::new()
        .name("smelt-peer-requests".into())
        .spawn(move || {
            while let Ok(line) = requests.recv_blocking() {
                let slot = Arc::clone(&slot);
                let session_id = session_id.clone();
                let acp_sessions = Arc::clone(&acp_sessions);
                let subscribers = subscribers.clone();
                let _ = thread::Builder::new()
                    .name("smelt-peer-request".into())
                    .spawn(move || {
                        answer_host_request(&slot, &session_id, &acp_sessions, &subscribers, &line);
                    });
            }
        });
}

fn answer_host_request(
    slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    line: &serde_json::Value,
) {
    let Some(request_id) = line.get("request_id").and_then(serde_json::Value::as_u64) else {
        return;
    };
    let op = line
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let body = line.get("body").cloned().unwrap_or_default();
    let reply = if !acp_sessions.is_current(source_id, slot) {
        rejected("not_found", "发出请求的对话已经换掉了")
    } else {
        let answered = match op {
            "session_list" => Ok(list(source_id, acp_sessions, &body)),
            "session_read" => read(source_id, acp_sessions, &body),
            "session_send" => send(slot, source_id, acp_sessions, subscribers, &body),
            "session_spawn" => spawn(slot, source_id, acp_sessions, subscribers, &body),
            _ => Err(rejected("invalid", format!("不认识的操作 {op}"))),
        };
        answered.unwrap_or_else(|rejection| rejection)
    };
    let host = slot.value.hosted_handle.lock().unwrap().clone();
    if let Some(host) = host {
        let _ = host.send_action(&AcpUserAction::PeerReply { request_id, reply });
    }
}

fn rejected(reason: &str, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"ok": false, "reason": reason, "error": error.into()})
}

/// 能和它互发消息：在跑或挂起着的 Pi 对话，不归自动化。
fn is_peer_session(id: &str, sess: &AcpSession) -> bool {
    if id.starts_with("acp-automation-")
        || matches!(
            *sess.conversation_binding.lock().unwrap(),
            Some(smelt_core::conversation::ConversationBinding::Automation { .. })
        )
    {
        return false;
    }
    let is_pi = sess
        .launch_spec
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(smelt_core::pi_rpc::is_smelt_pi_launch);
    is_pi && (acp_runtime_alive(sess) || sess.idle.is_suspended())
}

fn peer_target(
    acp_sessions: &AcpSessions,
    id: &str,
) -> Result<Arc<AcpSlot<AcpSession>>, serde_json::Value> {
    acp_sessions
        .get(id)
        .filter(|slot| is_peer_session(id, &slot.value))
        .ok_or_else(|| {
            rejected(
                "not_found",
                format!("没有这个 Pi 对话，或它没在运行：{id}。用 session_list 查可以发消息的对话"),
            )
        })
}

/// 侧栏上的名字：桌面菜单里的（含用户改过的），其次远程目录里的，再次会话自己的标题。
fn title_of(id: &str, sess: &AcpSession) -> String {
    if let Some(server) = SERVER.get() {
        let menu = server
            .workspace_menu
            .lock()
            .unwrap()
            .acp_session(id)
            .map(|session| session.title.trim().to_string())
            .filter(|title| !title.is_empty());
        if let Some(title) = menu {
            return title;
        }
        let catalog = server
            .remote_sessions
            .lock()
            .unwrap()
            .catalog()
            .ok()
            .and_then(|catalog| catalog.remote_acp(id))
            .map(|session| session.title.trim().to_string())
            .filter(|title| !title.is_empty());
        if let Some(title) = catalog {
            return title;
        }
    }
    if let Some(title) = sess.reduced.lock().unwrap().resolved_session_title() {
        return title;
    }
    sess.cwd
        .as_deref()
        .and_then(|cwd| std::path::Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名对话".to_string())
}

fn state_of(sess: &AcpSession) -> &'static str {
    if sess.idle.is_suspended() {
        return "suspended";
    }
    let phase = sess.reduced.lock().unwrap().phase;
    let busy = sess.prompt_in_flight.load(Ordering::SeqCst)
        || !matches!(
            phase,
            smelt_core::daemon_state::DaemonPhase::Idle
                | smelt_core::daemon_state::DaemonPhase::Succeeded
                | smelt_core::daemon_state::DaemonPhase::Failed
                | smelt_core::daemon_state::DaemonPhase::Dead
        );
    if busy { "busy" } else { "idle" }
}

/// 权限模式是「完全访问」。值来自驱动报的配置项，挂起的会话镜像里也还在。
fn bypasses_approval(sess: &AcpSession) -> bool {
    sess.reduced
        .lock()
        .unwrap()
        .config_options
        .iter()
        .find(|config| config.config_id == "mode")
        .and_then(crate::acp_idle::config_value)
        .as_deref()
        == Some("bypassPermissions")
}

fn list(
    source_id: &str,
    acp_sessions: &AcpSessions,
    body: &serde_json::Value,
) -> serde_json::Value {
    let query = body
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty());
    let mut sessions = Vec::new();
    for (id, slot) in acp_sessions.snapshot() {
        let sess = &slot.value;
        if id == source_id || !is_peer_session(&id, sess) {
            continue;
        }
        let title = title_of(&id, sess);
        if let Some(query) = &query
            && !title.to_lowercase().contains(query)
            && !id.to_lowercase().contains(query)
        {
            continue;
        }
        let sent: Vec<serde_json::Value> = sess
            .reduced
            .lock()
            .unwrap()
            .peer_inbox
            .iter()
            .filter(|message| message.source_session_id == source_id)
            .map(|message| {
                serde_json::json!({
                    "id": message.id,
                    "kind": message.kind.as_str(),
                    "status": match message.status {
                        PeerMessageStatus::Pending => "pending",
                        PeerMessageStatus::Delivered => "delivered",
                    },
                })
            })
            .collect();
        sessions.push(serde_json::json!({
            "id": id,
            "title": title,
            "project": sess.cwd,
            "state": state_of(sess),
            "sent": sent,
        }));
    }
    serde_json::json!({"ok": true, "sessions": sessions})
}

fn read(
    source_id: &str,
    acp_sessions: &AcpSessions,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let id = text_field(body, "sessionId").ok_or_else(|| rejected("invalid", "缺少 sessionId"))?;
    if id == source_id {
        return Err(rejected("self", "这是你自己的对话"));
    }
    let limit = body
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(1, |limit| (limit as usize).clamp(1, READ_LIMIT_MAX));
    let slot = peer_target(acp_sessions, &id)?;
    let sess = &slot.value;
    let (mut replies, total) = {
        let reduced = sess.reduced.lock().unwrap();
        (
            assistant_replies(reduced.entries(), limit),
            reduced.entries_total(),
        )
    };
    // 宿主模式下父镜像只留当前回合。不够就向还活着的宿主要一页；挂起的不唤醒，镜像里有多少给多少。
    if replies.len() < limit && !sess.idle.is_suspended() {
        let host = sess.hosted_handle.lock().unwrap().clone();
        if let Some(host) = host
            && let Ok(page) = host.request_history_page(total, 200)
        {
            replies = assistant_replies(&page.entries, limit);
        }
    }
    Ok(serde_json::json!({
        "ok": true,
        "sessionId": id,
        "title": title_of(&id, sess),
        "state": state_of(sess),
        "replies": replies,
    }))
}

/// 最近 `limit` 条助手正文（不含思考），按先后排，每条截到 [`READ_REPLY_CHARS`]。
fn assistant_replies(entries: &[smelt_core::acp_chat::AcpEntry], limit: usize) -> Vec<String> {
    let mut replies: Vec<String> = entries
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            smelt_core::acp_chat::AcpEntry::Assistant {
                text,
                thought: false,
            } if !text.trim().is_empty() => Some(truncate_chars(text.trim(), READ_REPLY_CHARS)),
            _ => None,
        })
        .take(limit)
        .collect();
    replies.reverse();
    replies
}

fn truncate_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

fn send(
    source_slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let to = text_field(body, "to").ok_or_else(|| rejected("invalid", "缺少 to"))?;
    let text = text_field(body, "text").ok_or_else(|| rejected("invalid", "消息是空的"))?;
    if text.chars().count() > TEXT_LIMIT_CHARS {
        return Err(rejected(
            "invalid",
            format!("消息太长，最多 {TEXT_LIMIT_CHARS} 个字符"),
        ));
    }
    if to == source_id {
        return Err(rejected("self", "不能给自己发消息"));
    }
    let kind = PeerMessageKind::parse(
        body.get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    let reply_to = text_field(body, "replyTo");
    let idempotency_key = text_field(body, "idempotencyKey");
    let target_slot = peer_target(acp_sessions, &to)?;
    let target = &target_slot.value;
    let source = &source_slot.value;

    if bypasses_approval(target) && !bypasses_approval(source) {
        return Err(rejected(
            "permission",
            "对方是「完全访问」，你是「逐次审批」，不能给它发消息",
        ));
    }
    let budget = chain_budget(source_id);
    if budget == 0 {
        return Err(rejected(
            "hops_exhausted",
            "这条消息链转发次数用完了，等用户在这个对话里说话后才能再发",
        ));
    }
    if target.idle.is_suspended() {
        crate::acp_idle::wake_suspended_acp_session(&to, &target_slot, acp_sessions, subscribers)
            .map_err(|error| rejected("target_unavailable", format!("拉不起对方的对话：{error}")))?;
    }

    let target_title = title_of(&to, target);
    let dedupe = idempotency_key
        .as_deref()
        .map(|key| format!("peer:{source_id}:{key}"));
    let mut submitted = target.conversation_submit.lock().unwrap();
    if let Some(dedupe) = &dedupe
        && submitted.contains(dedupe)
    {
        return Ok(serde_json::json!({
            "ok": true,
            "status": "duplicate",
            "to": {"id": to, "title": target_title},
        }));
    }
    if pending_count(&to, target) >= INBOX_LIMIT {
        return Err(rejected(
            "inbox_full",
            format!("对方还有 {INBOX_LIMIT} 条消息没收，等它处理完再发"),
        ));
    }
    let hops = budget - 1;
    let message = PeerDelivery {
        id: next_message_id(),
        source_session_id: source_id.to_string(),
        source_title: title_of(source_id, source),
        kind,
        hops,
        reply_to,
        idempotency_key: idempotency_key.unwrap_or_default(),
        text,
    };
    let id = message.id.clone();
    crate::acp_host::deliver_peer_message(target, message)
        .map_err(|error| rejected("target_unavailable", error))?;
    if let Some(dedupe) = dedupe {
        submitted.remember(dedupe);
    }
    drop(submitted);
    IN_FLIGHT
        .lock()
        .unwrap()
        .entry(to.clone())
        .or_default()
        .push((id.clone(), Instant::now()));
    {
        let mut chains = CHAINS.lock().unwrap();
        let current = chains.get(&to).copied().unwrap_or(INITIAL_HOPS);
        chains.insert(to.clone(), current.min(hops));
    }
    Ok(serde_json::json!({
        "ok": true,
        "status": "enqueued",
        "id": id,
        "to": {"id": to, "title": target_title},
        "hops": hops,
    }))
}

/// 起一个新的 Pi 对话，交给它一个任务。见模块说明。
fn spawn(
    source_slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    use smelt_core::agent_kind::ConversationAgentKind;

    let title = text_field(body, "title").ok_or_else(|| rejected("invalid", "缺少 title"))?;
    let title = truncate_chars(&title, TITLE_LIMIT_CHARS);
    let task = text_field(body, "task").ok_or_else(|| rejected("invalid", "任务是空的"))?;
    if task.chars().count() > TEXT_LIMIT_CHARS {
        return Err(rejected(
            "invalid",
            format!("任务太长，最多 {TEXT_LIMIT_CHARS} 个字符"),
        ));
    }
    let server = SERVER
        .get()
        .ok_or_else(|| rejected("target_unavailable", "这个进程里不能起新对话"))?;
    if was_spawned(source_id) {
        return Err(rejected(
            "spawn_limit",
            "别的对话起的对话不能再起新对话。要人手就用 session_send 交给已有的对话",
        ));
    }
    if chain_budget(source_id) == 0 {
        return Err(rejected(
            "hops_exhausted",
            "这条消息链转发次数用完了，等用户在这个对话里说话后才能再起新对话",
        ));
    }
    if live_spawned(source_id, acp_sessions) >= SPAWN_LIMIT {
        return Err(rejected(
            "spawn_limit",
            format!("你起的对话已经有 {SPAWN_LIMIT} 个还开着，先用 session_send 交给它们"),
        ));
    }

    let source = &source_slot.value;
    let launch = source
        .launch_spec
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| rejected("target_unavailable", "找不到这个对话的启动规格"))?;
    let cwd = source
        .cwd
        .clone()
        .ok_or_else(|| rejected("target_unavailable", "这个对话没有工作目录"))?;
    let ephemeral_env = source.ephemeral_env.lock().unwrap().clone();
    // 发起方在远程目录里就照抄它的选项；桌面开的对话不在目录里，按 Pi 记。
    let agent_option_id = server
        .remote_sessions
        .lock()
        .unwrap()
        .catalog()
        .ok()
        .and_then(|catalog| catalog.remote_acp(source_id))
        .map(|session| session.agent_option_id)
        .or_else(|| find_agent_option_for(ConversationAgentKind::Pi, None).map(|option| option.id))
        .ok_or_else(|| rejected("target_unavailable", "没有可用的 Pi 配置"))?;

    let id = format!("acp-{}", uuid::Uuid::new_v4().simple());
    let request = AcpOpenRequest {
        id: id.clone(),
        cwd: Some(cwd.clone()),
        launch: launch.clone(),
        ephemeral_env,
        resume_id: None,
        fork_id: None,
        fork_cut: None,
        conversation_binding: None,
        pending_agent_preset: None,
        omit_history: false,
    };
    let remote = smelt_core::session_control::RemoteAcpSession {
        id: id.clone(),
        cwd,
        title: title.clone(),
        agent_option_id,
        agent: ConversationAgentKind::Pi.id().to_string(),
        launch,
        resume_id: None,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0),
        lifecycle: Default::default(),
        hidden: false,
    };
    let slot = create_daemon_acp_session(
        &request,
        Some(remote),
        &server.sessions,
        acp_sessions,
        subscribers,
        &server.remote_sessions,
    )
    .map_err(|error| rejected("target_unavailable", format!("起不来新对话：{error}")))?;
    SPAWNED
        .lock()
        .unwrap()
        .entry(source_id.to_string())
        .or_default()
        .push(id.clone());
    crate::acp_idle::copy_session_config(source, &id, &slot.value, subscribers).map_err(
        |error| {
            rejected(
                "target_unavailable",
                format!("新对话 {id} 起来了，但没等到它就绪：{error}"),
            )
        },
    )?;
    let _ = apply_acp_user_action(
        &slot.value,
        AcpUserAction::SetSessionTitle {
            title: title.clone(),
        },
        subscribers,
    );
    let sent = send(
        source_slot,
        source_id,
        acp_sessions,
        subscribers,
        &serde_json::json!({"to": id, "text": task, "kind": "task"}),
    )?;
    Ok(serde_json::json!({
        "ok": true,
        "status": "spawned",
        "id": sent["id"],
        "to": {"id": id, "title": title},
        "hops": sent["hops"],
    }))
}

/// 这个对话是别的对话起的。
fn was_spawned(session_id: &str) -> bool {
    SPAWNED
        .lock()
        .unwrap()
        .values()
        .any(|children| children.iter().any(|child| child == session_id))
}

/// 这个对话起的、还开着的新对话。关掉、删掉的顺手清出去。
fn live_spawned(source_id: &str, acp_sessions: &AcpSessions) -> usize {
    let mut spawned = SPAWNED.lock().unwrap();
    let Some(children) = spawned.get_mut(source_id) else {
        return 0;
    };
    children.retain(|child| {
        acp_sessions
            .get(child)
            .is_some_and(|slot| is_peer_session(child, &slot.value))
    });
    children.len()
}

fn chain_budget(session_id: &str) -> u32 {
    CHAINS
        .lock()
        .unwrap()
        .get(session_id)
        .copied()
        .unwrap_or(INITIAL_HOPS)
}

/// 对方还没收的：快照里待送的，加刚投出、快照还没回来的。
fn pending_count(target_id: &str, target: &AcpSession) -> usize {
    let (pending, known): (usize, HashSet<String>) = {
        let reduced = target.reduced.lock().unwrap();
        (
            reduced
                .peer_inbox
                .iter()
                .filter(|message| message.status == PeerMessageStatus::Pending)
                .count(),
            reduced
                .peer_inbox
                .iter()
                .map(|message| message.id.clone())
                .collect(),
        )
    };
    let mut in_flight = IN_FLIGHT.lock().unwrap();
    let Some(sent) = in_flight.get_mut(target_id) else {
        return pending;
    };
    sent.retain(|(id, at)| !known.contains(id) && at.elapsed() < IN_FLIGHT_TTL);
    pending + sent.len()
}

fn next_message_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("pm-{}", &id[..12])
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::acp_chat::AcpEntry;

    #[test]
    fn read_returns_the_latest_assistant_replies_in_order_without_thoughts() {
        let entries = vec![
            AcpEntry::User("跑一下".into()),
            AcpEntry::Assistant {
                text: "第一条".into(),
                thought: false,
            },
            AcpEntry::Assistant {
                text: "想一想".into(),
                thought: true,
            },
            AcpEntry::Assistant {
                text: "  ".into(),
                thought: false,
            },
            AcpEntry::Assistant {
                text: "第二条".into(),
                thought: false,
            },
        ];
        assert_eq!(assistant_replies(&entries, 1), vec!["第二条".to_string()]);
        assert_eq!(
            assistant_replies(&entries, 5),
            vec!["第一条".to_string(), "第二条".to_string()]
        );
    }

    #[test]
    fn long_replies_are_cut_on_a_character_boundary() {
        assert_eq!(truncate_chars("一二三四", 2), "一二…");
        assert_eq!(truncate_chars("一二", 2), "一二");
    }

    /// 起出来的对话不能再起：防止一层层分下去。
    #[test]
    fn a_spawned_conversation_cannot_spawn_again() {
        SPAWNED
            .lock()
            .unwrap()
            .insert("acp-spawn-parent".into(), vec!["acp-spawn-child".into()]);
        assert!(was_spawned("acp-spawn-child"));
        assert!(!was_spawned("acp-spawn-parent"));
    }

    #[test]
    fn chain_budget_starts_full_and_user_input_restores_it() {
        let id = "acp-chain-test";
        assert_eq!(chain_budget(id), INITIAL_HOPS);
        CHAINS.lock().unwrap().insert(id.to_string(), 0);
        assert_eq!(chain_budget(id), 0);
        reset_chain(id);
        assert_eq!(chain_budget(id), INITIAL_HOPS);
    }
}
