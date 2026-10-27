//! Pi 对话互发消息：主 daemon 这一头（门卫 + 发件队列）。设计见 `docs/pi-session-messaging-design.html`。
//!
//! 扩展的 `session_list` / `session_read` / `session_send` / `session_spawn` 由起会话时
//! 绑上的应答函数直接回答。来源是绑定时记下的会话 id，请求体里的字段不算。
//!
//! 先收下，再处理：门卫只做同步检查，通过就把消息记进内存里的发件队列、回 `accepted`。
//! 拉起对方、投递在后台：每个目标一条投递线程，按收下的先后经
//! [`crate::acp_host::ensure_conversation_runtime`] 拉起对方，把消息交给它的驱动。
//! 事件归约里的 `peer_inbox` 由 [`observe_inbox`] 对账：送到了就结束；
//! 那一代 runtime 已经退出、消息还没进会话就重投。投不了给发送方退信。
//!
//! 消息 id、起出来的对话 id 都由（来源，Pi 的 `toolCallId`）算出：同一次调用重放落到同一条。
//!
//! 门卫规则，都只在内存里，不落库：
//! - 只在 Pi 对话之间，不能发给自己，不碰自动化的会话；
//! - 「逐次审批」的对话不能给「完全访问」的对话发消息，否则就是借对方的权限做事；
//! - 发给对方、还没送到的到 [`INBOX_LIMIT`] 封就拒收；
//! - 每个对话记一个链预算，发一条减一，减到 0 拒绝；用户在这个对话里说话时复位。
//!
//! 起新对话（`session_spawn`）：照发起方的启动规格在同一个项目里建一个普通的侧栏对话，
//! 模型、推理强度、权限模式和发起方一样，再把任务作为第一封信投过去。哪一步失败就撤掉
//! 建了一半的对话，给发起方退信。每个对话同时最多 [`SPAWN_LIMIT`] 个；起出来的对话不能再起。

use super::*;
use smelt_core::acp_session::{
    AcpUserAction, PeerDelivery, PeerMessageKind, PeerMessageStatus, PeerMessageView,
};

/// 用户说过话之后，一条消息链最多再转这么多跳。
const INITIAL_HOPS: u32 = 8;
/// 发给一个对话、还没送到的到这么多封就拒收。
const INBOX_LIMIT: usize = 8;
const TEXT_LIMIT_CHARS: usize = 32 * 1024;
const READ_LIMIT_MAX: usize = 5;
const READ_REPLY_CHARS: usize = 4_000;
/// 一个对话同时开着的、它起的新对话最多这么多个。
const SPAWN_LIMIT: usize = 4;
const TITLE_LIMIT_CHARS: usize = 80;
/// 每个来源留最近这么多封已经结束（送到或失败）的信，`session_list` 据此回答送到没有。
const KEEP_SETTLED: usize = 64;
/// 退信里附的原文开头。
const BOUNCE_EXCERPT_CHARS: usize = 200;

/// 每个对话当前的链预算。没有记录就是 [`INITIAL_HOPS`]。
static CHAINS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);
/// 每个对话起过、还在（或正在建）的新对话。值里出现过的对话不能再起。
static SPAWNED: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Default::default);
static OUTBOX: LazyLock<Mutex<Outbox>> = LazyLock::new(Default::default);

fn lock_outbox() -> std::sync::MutexGuard<'static, Outbox> {
    OUTBOX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_chains() -> std::sync::MutexGuard<'static, HashMap<String, u32>> {
    CHAINS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_spawned() -> std::sync::MutexGuard<'static, HashMap<String, Vec<String>>> {
    SPAWNED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
struct Outbox {
    /// 按收下的先后。
    letters: Vec<Letter>,
    /// 有投递线程在跑的目标。
    draining: HashSet<String>,
    /// 正在建的新对话：建好之前不投。
    held: HashSet<String>,
}

struct Letter {
    delivery: PeerDelivery,
    to: String,
    to_title: String,
    state: LetterState,
    /// 收下时发送方是不是「完全访问」。对方没打开、收下时不知道档位的，投之前再比。
    source_bypasses: bool,
    /// 交代给新对话的任务。退信说的是「没建成」。
    spawned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LetterState {
    Queued,
    /// 交给了对方这一代宿主，还没进会话记录。
    HandedOff {
        generation: u64,
    },
    Delivered,
    Failed(String),
}

impl Letter {
    fn settled(&self) -> bool {
        matches!(self.state, LetterState::Delivered | LetterState::Failed(_))
    }

    fn undelivered_to(&self, target: &str) -> bool {
        self.to == target && !self.settled()
    }
}

enum Accepted {
    New,
    Replay,
    InboxFull,
}

impl Outbox {
    fn find(&self, id: &str) -> Option<&Letter> {
        self.letters.iter().find(|letter| letter.delivery.id == id)
    }

    fn undelivered(&self, target: &str) -> usize {
        self.letters
            .iter()
            .filter(|letter| letter.undelivered_to(target))
            .count()
    }

    /// 收下一封。同一个 id 已经在就是重放；`limit` 是对方积压的上限，退信不受限。
    fn accept(&mut self, letter: Letter, limit: Option<usize>) -> Accepted {
        if self.find(&letter.delivery.id).is_some() {
            return Accepted::Replay;
        }
        if limit.is_some_and(|limit| self.undelivered(&letter.to) >= limit) {
            return Accepted::InboxFull;
        }
        self.letters.push(letter);
        Accepted::New
    }

    fn settle(&mut self, id: &str, state: LetterState) -> Option<Letter> {
        let index = self
            .letters
            .iter()
            .position(|letter| letter.delivery.id == id)?;
        self.letters[index].state = state;
        let failed = matches!(self.letters[index].state, LetterState::Failed(_));
        let bounce = failed.then(|| bounce_of(&self.letters[index])).flatten();
        let source = self.letters[index].delivery.source_session_id.clone();
        self.prune(&source);
        bounce
    }

    /// 每个来源只留最近 [`KEEP_SETTLED`] 封已经结束的。
    fn prune(&mut self, source: &str) {
        let settled = self
            .letters
            .iter()
            .filter(|letter| letter.settled() && letter.delivery.source_session_id == source)
            .count();
        let mut excess = settled.saturating_sub(KEEP_SETTLED);
        self.letters.retain(|letter| {
            if excess > 0 && letter.settled() && letter.delivery.source_session_id == source {
                excess -= 1;
                return false;
            }
            true
        });
    }

    /// 这个目标没有投递线程、也没在建，就该起一条。
    fn claim_drain(&mut self, target: &str) -> bool {
        if self.held.contains(target) || self.draining.contains(target) {
            return false;
        }
        self.draining.insert(target.to_string())
    }

    /// 投递线程要投的下一封。没有了就把线程登记去掉，和 [`Outbox::claim_drain`] 在同一把锁里。
    fn next_for(&mut self, target: &str) -> Option<(PeerDelivery, bool)> {
        let next = self
            .letters
            .iter()
            .find(|letter| letter.to == target && letter.state == LetterState::Queued)
            .map(|letter| (letter.delivery.clone(), letter.source_bypasses));
        if next.is_none() {
            self.draining.remove(target);
        }
        next
    }

    /// [`LetterState::HandedOff`] 只是中间态：结算交给 [`observe_inbox`]。
    fn mark_handed_off(&mut self, id: &str, generation: u64) {
        if let Some(letter) = self
            .letters
            .iter_mut()
            .find(|letter| letter.delivery.id == id)
        {
            letter.state = LetterState::HandedOff { generation };
        }
    }

    /// 见 [`observe_inbox`]。有信退回排队就返回 true。
    fn reconcile(&mut self, target: &str, generation: u64, inbox: &[PeerMessageView]) -> bool {
        let mut delivered = Vec::new();
        let mut requeued = false;
        for letter in &mut self.letters {
            let LetterState::HandedOff {
                generation: handed_to,
            } = letter.state
            else {
                continue;
            };
            if letter.to != target {
                continue;
            }
            let seen = inbox
                .iter()
                .find(|message| message.id == letter.delivery.id);
            if seen.is_some_and(|message| message.status == PeerMessageStatus::Delivered) {
                delivered.push(letter.delivery.id.clone());
            } else if handed_to < generation {
                letter.state = LetterState::Queued;
                requeued = true;
            }
        }
        for id in delivered {
            self.settle(&id, LetterState::Delivered);
        }
        requeued
    }
}

/// 失败的信退给发送方。退信自己失败了不再退。
fn bounce_of(letter: &Letter) -> Option<Letter> {
    let LetterState::Failed(reason) = &letter.state else {
        return None;
    };
    if letter.delivery.kind == PeerMessageKind::Bounce {
        eprintln!(
            "[peer] 退信 {} 也没送到 {}：{reason}",
            letter.delivery.id, letter.to
        );
        return None;
    }
    let title = if letter.to_title.is_empty() {
        letter.to.as_str()
    } else {
        letter.to_title.as_str()
    };
    let head = if letter.spawned {
        format!("新对话〈{title}〉没建成，交给它的任务没送出去：{reason}")
    } else {
        format!(
            "你发给〈{title}〉的{}（{}）没送到：{reason}",
            letter.delivery.kind.label(),
            letter.delivery.id
        )
    };
    Some(Letter {
        delivery: PeerDelivery {
            id: format!("{}-bounce", letter.delivery.id),
            source_session_id: letter.to.clone(),
            source_title: letter.to_title.clone(),
            kind: PeerMessageKind::Bounce,
            hops: 0,
            reply_to: Some(letter.delivery.id.clone()),
            text: format!(
                "{head}\n\n原文开头：{}",
                truncate_chars(&letter.delivery.text, BOUNCE_EXCERPT_CHARS)
            ),
        },
        to: letter.delivery.source_session_id.clone(),
        to_title: letter.delivery.source_title.clone(),
        state: LetterState::Queued,
        source_bypasses: true,
        spawned: false,
    })
}

/// 投递线程要用的注册表和事件出口。
#[derive(Clone)]
struct Courier {
    acp_sessions: AcpSessions,
    subscribers: EventHubHandle,
}

impl Courier {
    /// 收下一封并按需起投递线程。
    fn post(&self, letter: Letter, limit: Option<usize>) -> Accepted {
        let target = letter.to.clone();
        let mut outbox = lock_outbox();
        let accepted = outbox.accept(letter, limit);
        if matches!(accepted, Accepted::New) && outbox.claim_drain(&target) {
            drop(outbox);
            self.drain(target);
        }
        accepted
    }

    fn post_bounce(&self, bounce: Option<Letter>) {
        if let Some(bounce) = bounce {
            self.post(bounce, None);
        }
    }

    /// 起一条投递线程，按先后把发给 `target` 的信一封封投完。
    fn drain(&self, target: String) {
        let courier = self.clone();
        let _ = thread::Builder::new()
            .name("smelt-peer-courier".into())
            .spawn(move || {
                loop {
                    let Some((delivery, source_bypasses)) = lock_outbox().next_for(&target) else {
                        return;
                    };
                    let id = delivery.id.clone();
                    if let Err(reason) = courier.hand_off(&target, delivery, source_bypasses) {
                        let bounce = lock_outbox().settle(&id, LetterState::Failed(reason));
                        courier.post_bounce(bounce);
                    }
                }
            });
    }

    /// 拉起对方，交给它的宿主。
    fn hand_off(
        &self,
        target: &str,
        delivery: PeerDelivery,
        source_bypasses: bool,
    ) -> Result<(), String> {
        let slot = crate::acp_host::ensure_conversation_runtime(
            target,
            &self.acp_sessions,
            &workspace_menu(),
            &self.subscribers,
        )?;
        let sess = &slot.value;
        if delivery.kind != PeerMessageKind::Bounce && !source_bypasses && bypasses_approval(sess) {
            return Err(
                "对方是「完全访问」，你是「逐次审批」，不能给它发消息（permission）".to_string(),
            );
        }
        // 先记下交给哪一代 runtime 再写：快照可能比这边改状态先回来。中途换了 runtime
        // 也不要紧，记的代数旧了，新的第一份快照会让它重投，驱动按 id 去重。
        let generation = sess.connection_generation.load(Ordering::SeqCst);
        lock_outbox().mark_handed_off(&delivery.id, generation);
        crate::acp_host::deliver_peer_message(sess, delivery).map_err(ToOwned::to_owned)
    }
}

/// 这一代 runtime 的 `peer_inbox` 对一遍交出去的信。送到的结束；交给更早一代、
/// 还没送到的，那一代已经不在了，退回排队重投。
pub(crate) fn observe_inbox(
    target: &str,
    generation: u64,
    inbox: &[PeerMessageView],
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let mut outbox = lock_outbox();
    if outbox.reconcile(target, generation, inbox) && outbox.claim_drain(target) {
        drop(outbox);
        Courier {
            acp_sessions: Arc::clone(acp_sessions),
            subscribers: subscribers.clone(),
        }
        .drain(target.to_string());
    }
}

/// 有交给这个对话、还没结算的信。归约时先问一句，没有就不必拷 `peer_inbox`。
pub(crate) fn awaiting_inbox(target: &str) -> bool {
    lock_outbox()
        .letters
        .iter()
        .any(|letter| letter.to == target && matches!(letter.state, LetterState::HandedOff { .. }))
}

/// 还有发给这个对话、没送到的信：它不能挂起，挂起会让交出去的那几封跟着进程一起没了。
pub(crate) fn has_undelivered(target: &str) -> bool {
    lock_outbox().undelivered(target) > 0
}

/// 用户在这个对话里说话了：别的对话发来的消息链到这里断开，预算复位。
pub(crate) fn reset_chain(session_id: &str) {
    lock_chains().remove(session_id);
}

/// 起这个 Pi 会话时绑上的应答。来源是 `session_id`，请求体里的字段不算。
///
/// `source_slot` 是绑定时的那一代槽位。请求到达时再 `get` 会拿到当前一代，
/// 紧接着 `is_current` 恒为真，换代之后的旧 runtime 就拦不住了。
pub(crate) fn bind_peer_handler(
    session_id: String,
    source_slot: std::sync::Weak<AcpSlot<AcpSession>>,
    acp_sessions: AcpSessions,
    subscribers: EventHubHandle,
) -> smelt_core::background_tasks::PeerRequestHandler {
    Arc::new(move |op, body| {
        let courier = Courier {
            acp_sessions: Arc::clone(&acp_sessions),
            subscribers: subscribers.clone(),
        };
        let Some(slot) = source_slot.upgrade() else {
            return rejected("not_found", "发出请求的对话已经换掉了");
        };
        if !acp_sessions.is_current(&session_id, &slot) {
            return rejected("not_found", "发出请求的对话已经换掉了");
        }
        match op {
            "session_list" => list(&session_id, &acp_sessions, body),
            "session_read" => read(&session_id, &acp_sessions, body).unwrap_or_else(|error| error),
            "session_send" => {
                send(&slot, &session_id, &courier, body).unwrap_or_else(|error| error)
            }
            "session_spawn" => {
                spawn(&slot, &session_id, &courier, body).unwrap_or_else(|error| error)
            }
            _ => rejected("invalid", format!("不认识的操作 {op}")),
        }
    })
}

fn rejected(reason: &str, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"ok": false, "reason": reason, "error": error.into()})
}

/// 找到了对方才被拒：带上对方是谁，界面写它的标题，不写 id。
fn with_target(mut reply: serde_json::Value, id: &str, title: &str) -> serde_json::Value {
    reply["to"] = serde_json::json!({"id": id, "title": title});
    reply
}

fn workspace_menu() -> WorkspaceMenuStore {
    SERVER
        .get()
        .map(|server| Arc::clone(&server.workspace_menu))
        .unwrap_or_default()
}

/// 对话目录里的一项。
struct Peer {
    id: String,
    title: String,
    project: Option<String>,
    state: &'static str,
    /// 权限档位是不是「完全访问」。没打开的对话不知道，拉起之后再看。
    bypasses: Option<bool>,
}

/// 注册表里能互发消息的：在跑或挂起着的 Pi 对话，不归自动化。
fn is_live_peer(id: &str, sess: &AcpSession) -> bool {
    if is_automation(id)
        || matches!(
            *sess.conversation_binding.lock(),
            Some(smelt_core::conversation::ConversationBinding::Automation { .. })
        )
    {
        return false;
    }
    let is_pi = sess
        .launch_spec
        .lock()
        .as_ref()
        .is_some_and(smelt_core::pi_rpc::is_smelt_pi_launch);
    is_pi && (acp_runtime_alive(sess) || sess.idle.is_suspended())
}

fn is_automation(id: &str) -> bool {
    id.starts_with("acp-automation-")
}

fn live_peer(id: &str, sess: &AcpSession) -> Peer {
    Peer {
        id: id.to_string(),
        title: title_of(id, sess),
        project: sess.cwd.clone(),
        state: state_of(sess),
        bypasses: Some(bypasses_approval(sess)),
    }
}

/// 侧栏菜单里、这次 daemon 还没打开过的 Pi 对话。
fn dormant_peers(acp_sessions: &AcpSessions) -> Vec<Peer> {
    let pi = smelt_core::agent_kind::ConversationAgentKind::Pi.id();
    let menu = workspace_menu();
    let menu = menu.lock().unwrap();
    menu.sessions
        .iter()
        .filter(|session| {
            session.kind == smelt_core::workspace_menu::WorkspaceMenuSessionKind::Acp
                && session.agent.as_deref() == Some(pi)
                && !is_automation(&session.id)
                && acp_sessions.get(&session.id).is_none()
        })
        .map(|session| Peer {
            id: session.id.clone(),
            title: session.title.trim().to_string(),
            project: session.cwd.clone().or_else(|| session.project_root.clone()),
            state: "dormant",
            bypasses: None,
        })
        .collect()
}

/// 能发给谁：注册表里的 Pi 对话，加侧栏上还没打开的。不含自己。
fn directory(source_id: &str, acp_sessions: &AcpSessions) -> Vec<Peer> {
    let mut peers: Vec<Peer> = acp_sessions
        .snapshot()
        .into_iter()
        .filter(|(id, slot)| id != source_id && is_live_peer(id, &slot.value))
        .map(|(id, slot)| live_peer(&id, &slot.value))
        .collect();
    peers.extend(
        dormant_peers(acp_sessions)
            .into_iter()
            .filter(|peer| peer.id != source_id),
    );
    peers
}

fn lookup_peer(id: &str, acp_sessions: &AcpSessions) -> Option<Peer> {
    match acp_sessions.get(id) {
        Some(slot) => is_live_peer(id, &slot.value).then(|| live_peer(id, &slot.value)),
        None => dormant_peers(acp_sessions)
            .into_iter()
            .find(|peer| peer.id == id),
    }
}

fn not_found(id: &str) -> serde_json::Value {
    rejected(
        "not_found",
        format!("没有这个 Pi 对话，或它已经停了：{id}。用 session_list 查可以发消息的对话"),
    )
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
    if let Some(title) = sess.reduced.lock().resolved_session_title() {
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
    let phase = sess.reduced.lock().phase;
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
        .config_options
        .iter()
        .find(|config| config.config_id == "mode")
        .and_then(crate::acp_idle::config_value)
        .as_deref()
        == Some("bypassPermissions")
}

/// `source` 发给 `target` 的信，发件队列还记得的。退信不算发送方发的。
fn sent_to(source: &str, target: &str) -> Vec<serde_json::Value> {
    lock_outbox()
        .letters
        .iter()
        .filter(|letter| {
            letter.to == target
                && letter.delivery.source_session_id == source
                && letter.delivery.kind != PeerMessageKind::Bounce
        })
        .map(|letter| {
            let mut sent = serde_json::json!({
                "id": letter.delivery.id,
                "kind": letter.delivery.kind.as_str(),
                "status": match &letter.state {
                    LetterState::Queued => "queued",
                    LetterState::HandedOff { .. } => "pending",
                    LetterState::Delivered => "delivered",
                    LetterState::Failed(_) => "failed",
                },
            });
            if let LetterState::Failed(reason) = &letter.state {
                sent["error"] = reason.clone().into();
            }
            sent
        })
        .collect()
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
    let sessions: Vec<serde_json::Value> = directory(source_id, acp_sessions)
        .into_iter()
        .filter(|peer| {
            query.as_ref().is_none_or(|query| {
                peer.title.to_lowercase().contains(query) || peer.id.to_lowercase().contains(query)
            })
        })
        .map(|peer| {
            serde_json::json!({
                "id": peer.id,
                "title": peer.title,
                "project": peer.project,
                "state": peer.state,
                "sent": sent_to(source_id, &peer.id),
            })
        })
        .collect();
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
    let Some(slot) = acp_sessions
        .get(&id)
        .filter(|slot| is_live_peer(&id, &slot.value))
    else {
        if lookup_peer(&id, acp_sessions).is_some() {
            return Err(rejected(
                "not_running",
                "这段对话还没打开，读不到它的回复。要它干活就用 session_send，发消息会把它打开",
            ));
        }
        return Err(not_found(&id));
    };
    let sess = &slot.value;
    // 挂起后内存里只剩当前回合，有多少给多少，不为此把对方拉起来。
    let replies = {
        let reduced = sess.reduced.lock();
        assistant_replies(reduced.entries(), limit)
    };
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

fn accepted_reply(id: &str, to: &str, to_title: &str, hops: u32) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "status": "accepted",
        "id": id,
        "to": {"id": to, "title": to_title},
        "hops": hops,
    })
}

/// 同一次工具调用又来了：回第一次的结果。
fn replay_reply(id: &str) -> Option<serde_json::Value> {
    let outbox = lock_outbox();
    let letter = outbox.find(id)?;
    Some(accepted_reply(
        id,
        &letter.to,
        &letter.to_title,
        letter.delivery.hops,
    ))
}

fn send(
    source_slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    courier: &Courier,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let call =
        text_field(body, "toolCallId").ok_or_else(|| rejected("invalid", "缺少 toolCallId"))?;
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
    let id = message_id(source_id, &call);
    if let Some(reply) = replay_reply(&id) {
        return Ok(reply);
    }
    let kind = PeerMessageKind::from_sender(
        body.get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    let target = lookup_peer(&to, &courier.acp_sessions).ok_or_else(|| not_found(&to))?;
    let source = &source_slot.value;
    let source_bypasses = bypasses_approval(source);
    if target.bypasses == Some(true) && !source_bypasses {
        return Err(with_target(
            rejected(
                "permission",
                "对方是「完全访问」，你是「逐次审批」，不能给它发消息",
            ),
            &to,
            &target.title,
        ));
    }
    let budget = chain_budget(source_id);
    if budget == 0 {
        return Err(with_target(
            rejected(
                "hops_exhausted",
                "这条消息链转发次数用完了：用户在这个对话里说话之前，发给哪个对话都会被拒，不要重试。要对方知道的事告诉用户",
            ),
            &to,
            &target.title,
        ));
    }
    let hops = budget - 1;
    let letter = Letter {
        delivery: PeerDelivery {
            id: id.clone(),
            source_session_id: source_id.to_string(),
            source_title: title_of(source_id, source),
            kind,
            hops,
            reply_to: text_field(body, "replyTo"),
            text,
        },
        to: to.clone(),
        to_title: target.title.clone(),
        state: LetterState::Queued,
        source_bypasses,
        spawned: false,
    };
    match courier.post(letter, Some(INBOX_LIMIT)) {
        Accepted::New => {}
        Accepted::Replay => {
            return replay_reply(&id).ok_or_else(|| rejected("invalid", "这条消息刚被清掉"));
        }
        Accepted::InboxFull => {
            return Err(with_target(
                rejected(
                    "inbox_full",
                    format!("对方还有 {INBOX_LIMIT} 条消息没收，等它处理完再发"),
                ),
                &to,
                &target.title,
            ));
        }
    }
    lower_chain(&to, hops);
    Ok(accepted_reply(&id, &to, &target.title, hops))
}

/// 起一个新的 Pi 对话，交给它一个任务。同步只做检查、记账，建对话在后台。见模块说明。
fn spawn(
    source_slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    courier: &Courier,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    use smelt_core::agent_kind::ConversationAgentKind;

    let call =
        text_field(body, "toolCallId").ok_or_else(|| rejected("invalid", "缺少 toolCallId"))?;
    let title = text_field(body, "title").ok_or_else(|| rejected("invalid", "缺少 title"))?;
    let title = truncate_chars(&title, TITLE_LIMIT_CHARS);
    let task = text_field(body, "task").ok_or_else(|| rejected("invalid", "任务是空的"))?;
    if task.chars().count() > TEXT_LIMIT_CHARS {
        return Err(rejected(
            "invalid",
            format!("任务太长，最多 {TEXT_LIMIT_CHARS} 个字符"),
        ));
    }
    let id = spawned_id(source_id, &call);
    let message = message_id(source_id, &call);
    if let Some(reply) = replay_reply(&message) {
        return Ok(reply);
    }
    if courier.acp_sessions.get(&id).is_some() {
        // 发件队列随 daemon 重启没了，对话还在：仍然是那一次，不再建。
        return Ok(accepted_reply(
            &message,
            &id,
            &title,
            chain_budget(source_id).saturating_sub(1),
        ));
    }
    let server = SERVER
        .get()
        .ok_or_else(|| rejected("invalid", "这个进程里不能起新对话"))?;
    if was_spawned(source_id) {
        return Err(rejected(
            "spawn_limit",
            "别的对话起的对话不能再起新对话。要人手就用 session_send 交给已有的对话",
        ));
    }
    let budget = chain_budget(source_id);
    if budget == 0 {
        return Err(rejected(
            "hops_exhausted",
            "这条消息链转发次数用完了：用户在这个对话里说话之前，不能再起新对话，不要重试。要做的事告诉用户",
        ));
    }

    let source = &source_slot.value;
    let launch = source
        .launch_spec
        .lock()
        .clone()
        .ok_or_else(|| rejected("invalid", "找不到这个对话的启动规格"))?;
    let cwd = source
        .cwd
        .clone()
        .ok_or_else(|| rejected("invalid", "这个对话没有工作目录"))?;
    let ephemeral_env = source.ephemeral_env.lock().clone();
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
        .ok_or_else(|| rejected("invalid", "没有可用的 Pi 配置"))?;

    {
        let mut spawned = lock_spawned();
        let children = spawned.entry(source_id.to_string()).or_default();
        if children.contains(&id) {
            // 同一次调用并发重放：另一个请求已经在建了。
            return Ok(accepted_reply(&message, &id, &title, budget - 1));
        }
        retain_live_children(children, &courier.acp_sessions);
        if children.len() >= SPAWN_LIMIT {
            return Err(rejected(
                "spawn_limit",
                format!("你起的对话已经有 {SPAWN_LIMIT} 个还开着，先用 session_send 交给它们"),
            ));
        }
        children.push(id.clone());
        // 和上面的计数在同一把锁里登记「正在建」：别的请求清理时不会把它当成已经关掉的。
        lock_outbox().held.insert(id.clone());
    }
    let hops = budget - 1;
    let letter = Letter {
        delivery: PeerDelivery {
            id: message.clone(),
            source_session_id: source_id.to_string(),
            source_title: title_of(source_id, source),
            kind: PeerMessageKind::Task,
            hops,
            reply_to: None,
            text: task,
        },
        to: id.clone(),
        to_title: title.clone(),
        state: LetterState::Queued,
        source_bypasses: bypasses_approval(source),
        spawned: true,
    };
    {
        let mut outbox = lock_outbox();
        // 同一个 id 已经在 `SPAWNED` 里挡住了，这里只会是新的。
        outbox.accept(letter, None);
    }
    lower_chain(&id, hops);

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
    let reply = accepted_reply(&message, &id, &title, hops);
    let source_slot = Arc::clone(source_slot);
    let courier = courier.clone();
    let _ = thread::Builder::new()
        .name("smelt-peer-spawn".into())
        .spawn(move || {
            let built = build_spawned(
                &request,
                remote,
                &title,
                &source_slot.value,
                server,
                &courier,
            );
            let mut outbox = lock_outbox();
            outbox.held.remove(&request.id);
            match built {
                Ok(()) => {
                    if outbox.claim_drain(&request.id) {
                        drop(outbox);
                        courier.drain(request.id.clone());
                    }
                }
                Err(reason) => {
                    eprintln!("[peer] 新对话 {} 没建成：{reason}", request.id);
                    let bounce = outbox.settle(&message, LetterState::Failed(reason));
                    drop(outbox);
                    forget_child(&request.id);
                    courier.post_bounce(bounce);
                }
            }
        });
    Ok(reply)
}

/// 建新对话、写好配置和标题。哪一步失败就把建了一半的对话撤掉，侧栏上不留空对话。
fn build_spawned(
    request: &AcpOpenRequest,
    remote: smelt_core::session_control::RemoteAcpSession,
    title: &str,
    source: &AcpSession,
    server: &crate::protocol::ServerShared,
    courier: &Courier,
) -> Result<(), String> {
    let slot = create_daemon_acp_session(
        request,
        Some(remote),
        &server.sessions,
        &courier.acp_sessions,
        &courier.subscribers,
        &server.remote_sessions,
    )
    .map_err(|error| format!("起不来新对话：{error}"))?;
    let configured = crate::acp_idle::copy_session_config(
        source,
        &request.id,
        &slot.value,
        &courier.subscribers,
    )
    .map_err(|error| format!("没等到它就绪：{error}"))
    .and_then(|()| {
        apply_acp_user_action(
            &slot.value,
            AcpUserAction::SetSessionTitle {
                title: title.to_string(),
            },
            &courier.subscribers,
        )
        .map_err(|error| format!("改不了标题：{error}"))
    });
    if let Err(reason) = configured {
        if let Err(error) = kill_acp_session(
            &request.id,
            &courier.acp_sessions,
            &server.remote_sessions,
            &courier.subscribers,
        ) {
            eprintln!("[peer] 撤掉没建成的新对话 {} 失败：{error}", request.id);
        }
        return Err(reason);
    }
    Ok(())
}

/// 这个对话是别的对话起的。
fn was_spawned(session_id: &str) -> bool {
    SPAWNED
        .lock()
        .unwrap()
        .values()
        .any(|children| children.iter().any(|child| child == session_id))
}

/// 起出来的对话里，关掉、删掉的清出去。正在建的（还在发件队列里等）留着。
fn retain_live_children(children: &mut Vec<String>, acp_sessions: &AcpSessions) {
    let building = lock_outbox().held.clone();
    children.retain(|child| {
        building.contains(child)
            || acp_sessions
                .get(child)
                .is_some_and(|slot| is_live_peer(child, &slot.value))
    });
}

fn forget_child(child: &str) {
    for children in lock_spawned().values_mut() {
        children.retain(|id| id != child);
    }
}

fn chain_budget(session_id: &str) -> u32 {
    CHAINS
        .lock()
        .unwrap()
        .get(session_id)
        .copied()
        .unwrap_or(INITIAL_HOPS)
}

/// 收到一条还剩 `hops` 跳的消息，对方往下转最多也就这么多。
fn lower_chain(session_id: &str, hops: u32) {
    let mut chains = lock_chains();
    let current = chains.get(session_id).copied().unwrap_or(INITIAL_HOPS);
    chains.insert(session_id.to_string(), current.min(hops));
}

/// SHA-256（各段以 0 分隔）的十六进制。
fn derived_hex(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 这次工具调用发出的消息的 id。
fn message_id(source_id: &str, tool_call_id: &str) -> String {
    format!("pm-{}", &derived_hex(&[source_id, tool_call_id])[..12])
}

/// 这次工具调用起的新对话的 id，和 `session.create` 的 id 同一个形状。
fn spawned_id(source_id: &str, tool_call_id: &str) -> String {
    format!(
        "acp-{}",
        &derived_hex(&[source_id, tool_call_id, "spawn"])[..32]
    )
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
        lower_chain(id, 0);
        assert_eq!(chain_budget(id), 0);
        reset_chain(id);
        assert_eq!(chain_budget(id), INITIAL_HOPS);
    }

    /// 同一次工具调用重放落到同一条消息、同一个新对话；换一次调用就是新的。
    #[test]
    fn ids_follow_the_source_and_the_tool_call() {
        assert_eq!(
            message_id("acp-a", "toolu_1"),
            message_id("acp-a", "toolu_1")
        );
        assert_ne!(
            message_id("acp-a", "toolu_1"),
            message_id("acp-a", "toolu_2")
        );
        assert_ne!(
            message_id("acp-a", "toolu_1"),
            message_id("acp-b", "toolu_1")
        );
        let message = message_id("acp-a", "toolu_1");
        assert!(
            message.starts_with("pm-") && message.len() == 15,
            "{message}"
        );
        let spawned = spawned_id("acp-a", "toolu_1");
        assert!(
            spawned.starts_with("acp-") && spawned.len() == 36,
            "{spawned}"
        );
        assert_eq!(spawned, spawned_id("acp-a", "toolu_1"));
    }

    fn letter(id: &str, from: &str, to: &str) -> Letter {
        Letter {
            delivery: PeerDelivery {
                id: id.into(),
                source_session_id: from.into(),
                source_title: "修登录".into(),
                kind: PeerMessageKind::Task,
                hops: 7,
                reply_to: None,
                text: "重跑 flaky e2e".into(),
            },
            to: to.into(),
            to_title: "测试".into(),
            state: LetterState::Queued,
            source_bypasses: false,
            spawned: false,
        }
    }

    /// 积压只数还没送到的；同一个 id 再来是重放，不占位置。
    #[test]
    fn the_outbox_counts_undelivered_letters_and_ignores_replays() {
        let mut outbox = Outbox::default();
        assert!(matches!(
            outbox.accept(letter("pm-1", "acp-a", "acp-b"), Some(2)),
            Accepted::New
        ));
        assert!(matches!(
            outbox.accept(letter("pm-1", "acp-a", "acp-b"), Some(2)),
            Accepted::Replay
        ));
        assert!(matches!(
            outbox.accept(letter("pm-2", "acp-a", "acp-b"), Some(2)),
            Accepted::New
        ));
        assert!(matches!(
            outbox.accept(letter("pm-3", "acp-a", "acp-b"), Some(2)),
            Accepted::InboxFull
        ));
        outbox.settle("pm-1", LetterState::Delivered);
        assert_eq!(outbox.undelivered("acp-b"), 1);
        assert!(matches!(
            outbox.accept(letter("pm-3", "acp-a", "acp-b"), Some(2)),
            Accepted::New
        ));
    }

    /// 投递线程按收下的先后取；没有了就把自己的登记去掉，下一封进来能再起一条。
    #[test]
    fn a_target_has_one_courier_that_takes_letters_in_order() {
        let mut outbox = Outbox::default();
        outbox.accept(letter("pm-1", "acp-a", "acp-b"), None);
        outbox.accept(letter("pm-2", "acp-c", "acp-b"), None);
        assert!(outbox.claim_drain("acp-b"));
        assert!(!outbox.claim_drain("acp-b"), "同一个目标只有一条投递线程");
        assert_eq!(outbox.next_for("acp-b").unwrap().0.id, "pm-1");
        outbox.mark_handed_off("pm-1", 3);
        assert_eq!(outbox.next_for("acp-b").unwrap().0.id, "pm-2");
        outbox.settle("pm-2", LetterState::Failed("桌面没开".into()));
        assert!(outbox.next_for("acp-b").is_none());
        assert!(outbox.claim_drain("acp-b"), "线程退出后能再起");

        outbox.held.insert("acp-new".into());
        assert!(!outbox.claim_drain("acp-new"), "正在建的对话不投");
    }

    /// 失败的信退给发送方：来源是没收到的那个对话，正文说清楚哪条、为什么。退信再失败不再退。
    #[test]
    fn a_failed_letter_bounces_back_to_its_sender_once() {
        let mut outbox = Outbox::default();
        outbox.accept(letter("pm-1", "acp-a", "acp-b"), None);
        let bounce = outbox
            .settle("pm-1", LetterState::Failed("Smelt 桌面没开".into()))
            .unwrap();
        assert_eq!(bounce.to, "acp-a");
        assert_eq!(bounce.delivery.id, "pm-1-bounce");
        assert_eq!(bounce.delivery.source_session_id, "acp-b");
        assert_eq!(bounce.delivery.kind, PeerMessageKind::Bounce);
        assert_eq!(bounce.delivery.reply_to.as_deref(), Some("pm-1"));
        assert!(bounce.delivery.text.contains("Smelt 桌面没开"));
        assert!(bounce.delivery.text.contains("重跑 flaky e2e"));

        outbox.accept(bounce, None);
        assert!(
            outbox
                .settle("pm-1-bounce", LetterState::Failed("也没开".into()))
                .is_none()
        );
    }

    fn inbox_view(id: &str, status: PeerMessageStatus) -> PeerMessageView {
        PeerMessageView {
            id: id.into(),
            source_session_id: "acp-a".into(),
            source_title: "修登录".into(),
            kind: PeerMessageKind::Task,
            status,
            queued_at_ms: 1,
            delivered_at_ms: None,
        }
    }

    /// 对方快照里标成送到的结束；还在宿主里的不动；接手它的那一代宿主不在了、还没送到的退回排队重投。
    #[test]
    fn the_inbox_settles_delivered_letters_and_requeues_ones_lost_with_their_host() {
        let mut outbox = Outbox::default();
        for id in ["pm-done", "pm-waiting", "pm-lost"] {
            outbox.accept(letter(id, "acp-a", "acp-b"), None);
            outbox.mark_handed_off(id, 2);
        }
        let inbox = [
            inbox_view("pm-done", PeerMessageStatus::Delivered),
            inbox_view("pm-waiting", PeerMessageStatus::Pending),
        ];
        assert!(
            !outbox.reconcile("acp-b", 2, &inbox),
            "同一代宿主还在，不重投"
        );
        assert_eq!(
            outbox.find("pm-done").unwrap().state,
            LetterState::Delivered
        );
        assert_eq!(
            outbox.find("pm-lost").unwrap().state,
            LetterState::HandedOff { generation: 2 },
            "快照还没带上它，宿主没换就还在路上"
        );

        // 宿主换了一代（重启、挂起后唤醒）：没送到的都要重投，包括旧镜像里还写着待送的。
        assert!(outbox.reconcile("acp-b", 3, &inbox));
        assert_eq!(
            outbox.find("pm-waiting").unwrap().state,
            LetterState::Queued
        );
        assert_eq!(outbox.find("pm-lost").unwrap().state, LetterState::Queued);
        assert_eq!(
            outbox.find("pm-done").unwrap().state,
            LetterState::Delivered
        );
    }

    /// 已结束的信每个来源只留最近一批，没送到的都留着。
    #[test]
    fn settled_letters_are_pruned_per_source() {
        let mut outbox = Outbox::default();
        outbox.accept(letter("pm-open", "acp-a", "acp-b"), None);
        for index in 0..KEEP_SETTLED + 3 {
            let id = format!("pm-{index}");
            outbox.accept(letter(&id, "acp-a", "acp-b"), None);
            outbox.settle(&id, LetterState::Delivered);
        }
        assert_eq!(outbox.letters.len(), KEEP_SETTLED + 1);
        assert!(outbox.find("pm-open").is_some());
        assert!(outbox.find("pm-0").is_none());
    }

    fn live_pi(acp_sessions: &AcpSessions, id: &str) -> Arc<AcpSlot<AcpSession>> {
        let slot = acp_sessions.reserve_with(id, || {
            crate::acp_host::make_acp_session(id, Some("/work".into()), None, None)
        });
        *slot.0.value.launch_spec.lock() =
            Some(smelt_core::agent_kind::ConversationAgentKind::Pi.default_launch());
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.0.value.handle.lock() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
        slot.0
    }

    /// 来源是绑定时记下的会话。请求体里写别的 sourceSessionId 也不改来源。
    #[test]
    fn the_bound_source_session_is_not_taken_from_the_request() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let _a = live_pi(&acp_sessions, "acp-a");
        let _b = live_pi(&acp_sessions, "acp-b");
        let handler = bind_peer_handler(
            "acp-a".into(),
            Arc::downgrade(&_a),
            Arc::clone(&acp_sessions),
            crate::new_event_hub(),
        );
        let listed = handler(
            "session_list",
            &serde_json::json!({"sourceSessionId": "acp-b"}),
        );
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|peer| peer["id"].as_str())
            .collect();
        assert!(!ids.contains(&"acp-a"), "自己不在名单里");
        assert!(ids.contains(&"acp-b"));

        let sent = handler(
            "session_send",
            &serde_json::json!({
                "to": "acp-a",
                "text": "hi",
                "toolCallId": "toolu_forged",
                "sourceSessionId": "acp-b",
            }),
        );
        assert_eq!(sent["reason"], "self");
    }

    /// 事件归约看到送到，就把 HandedOff 结掉。测完把这封从进程全局队列里拿走。
    #[test]
    fn observe_inbox_settles_a_handed_off_letter() {
        let id = format!("pm-observe-{}", uuid::Uuid::new_v4().simple());
        let mut letter = letter(&id, "acp-a", "acp-observe");
        letter.state = LetterState::HandedOff { generation: 3 };
        lock_outbox().letters.push(letter);
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        observe_inbox(
            "acp-observe",
            3,
            &[inbox_view(&id, PeerMessageStatus::Delivered)],
            &acp_sessions,
            &crate::new_event_hub(),
        );
        let state = lock_outbox().find(&id).map(|found| found.state.clone());
        lock_outbox()
            .letters
            .retain(|found| found.delivery.id != id);
        assert_eq!(state, Some(LetterState::Delivered));
    }

    /// 绑定时记下的那一代槽位被换掉之后，旧 runtime 的请求不能再进门。
    #[test]
    fn a_replaced_slot_cannot_keep_using_the_bound_handler() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let old = live_pi(&acp_sessions, "acp-a");
        let _b = live_pi(&acp_sessions, "acp-b");
        let stale = bind_peer_handler(
            "acp-a".into(),
            Arc::downgrade(&old),
            Arc::clone(&acp_sessions),
            crate::new_event_hub(),
        );
        assert!(acp_sessions.remove_if_same("acp-a", &old).is_some());
        let current = live_pi(&acp_sessions, "acp-a");
        assert!(!Arc::ptr_eq(&old, &current));

        let rejected_list = stale("session_list", &serde_json::json!({}));
        assert_eq!(rejected_list["ok"], false, "{rejected_list}");
        assert_eq!(rejected_list["reason"], "not_found");

        let fresh = bind_peer_handler(
            "acp-a".into(),
            Arc::downgrade(&current),
            Arc::clone(&acp_sessions),
            crate::new_event_hub(),
        );
        let listed = fresh("session_list", &serde_json::json!({}));
        assert_eq!(listed["ok"], true, "{listed}");
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|peer| peer["id"].as_str())
            .collect();
        assert!(ids.contains(&"acp-b"), "{listed}");
    }

    /// 驱动在持有本会话锁时 panic。别的会话的 session_list / session_read 仍能返回，
    /// 标成失败之后，这一个会话还能在同一个槽位上重新开始。
    #[test]
    fn a_panicked_session_does_not_break_other_sessions_or_its_own_restart() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let panicked = live_pi(&acp_sessions, "acp-panic");
        let other = live_pi(&acp_sessions, "acp-ok");
        let hub = crate::new_event_hub();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reduced = panicked.value.reduced.lock();
            let _turn = panicked.value.turn_completion.lock();
            let _spec = panicked.value.launch_spec.lock();
            panic!("driver boom");
        }));
        assert!(caught.is_err());
        assert!(
            panicked.value.reduced.is_poisoned(),
            "持锁 unwind 后归约锁应当中毒"
        );

        let handler = bind_peer_handler(
            "acp-ok".into(),
            Arc::downgrade(&other),
            Arc::clone(&acp_sessions),
            hub.clone(),
        );
        let listed = handler("session_list", &serde_json::json!({}));
        assert_eq!(listed["ok"], true, "{listed}");
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|peer| peer["id"].as_str())
            .collect();
        assert!(
            ids.contains(&"acp-panic"),
            "中毒会话仍应被列出来，而不是把整次请求打崩：{listed}"
        );
        let read = handler(
            "session_read",
            &serde_json::json!({"sessionId": "acp-panic", "limit": 1}),
        );
        assert_eq!(read["ok"], true, "{read}");

        let payload: Box<dyn std::any::Any + Send> = Box::new("driver boom".to_string());
        crate::acp_host::mark_session_driver_panic(&panicked, "acp-panic", &hub, payload.as_ref());
        assert_eq!(
            panicked.value.reduced.lock().phase,
            smelt_core::daemon_state::DaemonPhase::Dead,
            "驱动 panic 后这个会话应当标成失败"
        );

        let listed_after = handler("session_list", &serde_json::json!({}));
        assert_eq!(listed_after["ok"], true, "{listed_after}");
        let read_after = handler(
            "session_read",
            &serde_json::json!({"sessionId": "acp-panic", "limit": 1}),
        );
        assert_eq!(read_after["ok"], true, "{read_after}");

        {
            let mut reduced = panicked.value.reduced.lock();
            reduced.history_session_id = Some("hist-resume".to_string());
        }
        *panicked.value.launch_spec.lock() =
            Some(smelt_core::agent_kind::ConversationLaunchSpec::from_command("/bin/true"));
        let restarted = crate::acp_host::restart_acp_session("acp-panic", &acp_sessions, &hub);
        assert!(restarted.is_ok(), "{restarted:?}");
        let current = acp_sessions.get("acp-panic").expect("槽位还在");
        assert!(
            Arc::ptr_eq(&current, &panicked),
            "重新开始应当复用同一个槽位"
        );
        let reduced = current.value.reduced.lock();
        assert_eq!(reduced.history_session_id.as_deref(), Some("hist-resume"));
        drop(reduced);
        assert!(
            crate::acp_host::retire_acp_runtime(&current.value),
            "测试拉起的 runtime 应当能退掉"
        );
    }
}
