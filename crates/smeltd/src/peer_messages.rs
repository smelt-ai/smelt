//! 跨引擎 ACP 对话互发消息：主 daemon 这一头（门卫 + 发件队列）。
//! 设计见 `docs/cross-agent-messaging.md` 与 `docs/pi-session-messaging-design.html`。
//!
//! 扩展 / MCP / CLI 的 `session_list` / `session_read` / `session_send` / `session_spawn`
//! （以及通知用户的 `notify`，见 `crate::agent_notify`）由起会话时绑上的应答函数直接回答。
//! 来源是绑定时记下的会话 id，请求体里的字段不算。
//!
//! 先收下，再处理：门卫只做同步检查，通过就把消息记进内存里的发件队列、回 `accepted`。
//! 拉起对方、投递在后台：每个目标一条投递线程，按收下的先后经
//! [`crate::acp_host::ensure_conversation_runtime`] 拉起对方，把消息交给它的驱动（Pi 经
//! 会话宿主写 `smelt.peer_message`；非 Pi ACP 经守护侧回合闸门和排队发 `session/prompt`）。
//! 事件归约里的 `peer_inbox` 由 [`observe_inbox`] 对账：送到了就结束；
//! 那一代 runtime 已经退出、消息还没进会话就重投。投不了给发送方退信。
//!
//! 消息 id、起出来的对话 id 都由（来源，toolCallId）算出：同一次调用重放落到同一条。
//!
//! 门卫规则，都只在内存里，不落库：
//! - 覆盖所有非自动化的 ACP 对话（Pi、产品智能体、Codex、Claude、Antigravity、自定义 Profile 等），
//!   不能发给自己，不碰自动化的会话；
//! - 三态权限姿态（[`ApprovalPosture`]）：只有 `FullAccess` 发起方能给 `FullAccess` / `Unknown`
//!   目标发消息，绝不静默提权；
//! - 发给对方、还没送到的到 [`INBOX_LIMIT`] 封就拒收；
//! - 每个对话记一个链预算，发一条减一，减到 0 拒绝；用户在这个对话里说话时复位。
//!
//! 别的机器上的对话：id 写成 `<runtime_id>/<会话 id>`，`runtime_id` 是本节点的对端节点
//! （见 `docs/cross-machine.md`）。发过去的信照样先进发件队列，投递线程经节点连接交给对方
//! 的 `node.peer_deliver`，由对方的门卫、发件队列投进它的对话；交出去就算这边的事完了
//! （[`LetterState::Forwarded`]），对方投不进去会把退信发回来。来源带上本机的 runtime id，
//! 对方照着回信。智能体主动找别的机器，要用户允许过本机智能体用那台机器；回给发消息过来的
//! 对话不用。`session_list` / `session_read` 经 `node.peer_list` / `node.peer_read` 问在线的
//! 那几台。`session_spawn` 只在本机：可指定 `agent`（产品智能体 / 第三方引擎 / 自定义 Profile）
//! 和工作区项目内的 `cwd`，省略时照抄发起方。

use super::*;
use smelt_core::acp_session::{
    AcpUserAction, PeerDelivery, PeerMessageKind, PeerMessageStatus, PeerMessageView,
};
use smelt_core::agent_kind::{ApprovalPosture, ConversationAgentKind};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

/// 交给别的机器、问别的机器的对话最多等这么久：要走公网拨号。
const REMOTE_TIMEOUT: Duration = Duration::from_secs(30);
/// 记着收到过哪些别的机器上的对话的消息，最多这么多对。
const CONTACTS_LIMIT: usize = 1024;

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
/// 本机对话和别的机器上哪个对话通过信：（本机对话，带机器的那一头），收到过、发出过都算。
/// 这一对之间回信不要求本机智能体权限，也不要求对方被允许连入本机。只在内存里，按先后
/// 丢掉最早的。
static CONTACTS: LazyLock<Mutex<std::collections::VecDeque<(String, String)>>> =
    LazyLock::new(Default::default);

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
    /// 收下时发送方的权限姿态。对方没打开、收下时不知道档位的，投之前再比。
    source_posture: ApprovalPosture,
    /// 发送方的智能体显示名（给非 Pi 目标的 `<peer-message>` 引导语标明来源引擎）。
    source_agent_label: Option<String>,
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
    /// 交给了别的机器，由它投进对话；投不进去它会退信回来。
    Forwarded,
    Failed(String),
}

impl Letter {
    fn settled(&self) -> bool {
        matches!(
            self.state,
            LetterState::Delivered | LetterState::Forwarded | LetterState::Failed(_)
        )
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
    fn next_for(&mut self, target: &str) -> Option<(PeerDelivery, ApprovalPosture, Option<String>)> {
        let next = self
            .letters
            .iter()
            .find(|letter| letter.to == target && letter.state == LetterState::Queued)
            .map(|letter| {
                (
                    letter.delivery.clone(),
                    letter.source_posture,
                    letter.source_agent_label.clone(),
                )
            });
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
        format!(
            "The new conversation \"{title}\" wasn't created, so its task wasn't sent: {reason}"
        )
    } else {
        format!(
            "Your {} to \"{title}\" ({}) wasn't delivered: {reason}",
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
                "{head}\n\nIt began: {}",
                truncate_chars(&letter.delivery.text, BOUNCE_EXCERPT_CHARS)
            ),
        },
        to: letter.delivery.source_session_id.clone(),
        to_title: letter.delivery.source_title.clone(),
        state: LetterState::Queued,
        source_posture: ApprovalPosture::FullAccess,
        source_agent_label: None,
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
                    let Some((delivery, source_posture, source_agent_label)) =
                        lock_outbox().next_for(&target)
                    else {
                        return;
                    };
                    let id = delivery.id.clone();
                    if let Err(reason) = courier.hand_off(
                        &target,
                        delivery,
                        source_posture,
                        source_agent_label.as_deref(),
                    ) {
                        let bounce = lock_outbox().settle(&id, LetterState::Failed(reason));
                        courier.post_bounce(bounce);
                    }
                }
            });
    }

    /// 拉起对方，交给它的宿主；别的机器上的交给那台机器。
    fn hand_off(
        &self,
        target: &str,
        delivery: PeerDelivery,
        source_posture: ApprovalPosture,
        source_agent_label: Option<&str>,
    ) -> Result<(), String> {
        if let Some((runtime, session)) = split_remote(target) {
            return forward(
                runtime,
                session,
                delivery,
                source_posture == ApprovalPosture::FullAccess,
            );
        }
        let slot = crate::acp_host::ensure_conversation_runtime(
            target,
            &self.acp_sessions,
            &workspace_directory(),
            &self.subscribers,
        )?;
        let sess = &slot.value;
        if delivery.kind != PeerMessageKind::Bounce
            && !source_posture.allows_message_to(approval_posture(target, sess))
        {
            return Err(
                "The other conversation has higher or unverified permissions and you ask for approval each time, so you can't message it (permission)".to_string(),
            );
        }
        // 先记下交给哪一代 runtime 再写：快照可能比这边改状态先回来。中途换了 runtime
        // 也不要紧，记的代数旧了，新的第一份快照会让它重投，驱动按 id 去重。
        let generation = sess.connection_generation.load(Ordering::SeqCst);
        lock_outbox().mark_handed_off(&delivery.id, generation);
        crate::acp_host::deliver_peer_message(
            sess,
            delivery,
            source_agent_label,
            &self.subscribers,
        )
        .map_err(ToOwned::to_owned)
    }
}

/// 交给别的机器：来源带上本机的 runtime id，对方照着回信。对方收下就算交出去了。
fn forward(
    runtime: &str,
    session: &str,
    mut delivery: PeerDelivery,
    source_bypasses: bool,
) -> Result<(), String> {
    let machine = crate::peer_nodes::peer_record(runtime)
        .ok_or_else(|| "That machine is no longer paired with this computer".to_string())?;
    let own = own_runtime_id()?;
    let id = delivery.id.clone();
    // 那边的对话可以回给这边发信的对话（那台机器没被允许连入本机时也行）。
    remember_contact(&delivery.source_session_id, &format!("{runtime}/{session}"));
    delivery.source_session_id = qualify(&own, &delivery.source_session_id);
    crate::peer_nodes::peer_client(&machine)
        .call::<smelt_core::control_api::NodePeerDeliver>(
            &smelt_core::peer_node::NodePeerDeliverParams {
                to: session.to_string(),
                delivery,
                source_bypasses,
            },
            REMOTE_TIMEOUT,
        )
        .map_err(|error| format!("Couldn't hand it to \"{}\": {error}", machine.name))?;
    lock_outbox().settle(&id, LetterState::Forwarded);
    Ok(())
}

/// 别的机器交来一条（`node.peer_deliver`），投给本机的对话 `to`。门卫规则同本机：对方不在、
/// 权限档位不合、积压满了都拒收，发送方那边照常退信。来源必须是调用方那台机器上的对话。
/// 本节点没允许它连入（`inbound`）时，只收回给本机对话的：`to` 之前和这个来源通过信。
pub(crate) fn accept_remote(
    from_runtime: &str,
    inbound: bool,
    params: smelt_core::peer_node::NodePeerDeliverParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let smelt_core::peer_node::NodePeerDeliverParams {
        to,
        mut delivery,
        source_bypasses,
    } = params;
    let refuse = |code, message: &str| Err(ControlError::new(code, message));
    if split_remote(&delivery.source_session_id).map(|(runtime, _)| runtime) != Some(from_runtime) {
        return refuse(
            ControlErrorCode::InvalidParams,
            "The source must be a conversation on the calling machine",
        );
    }
    let Some(machine) = crate::peer_nodes::peer_record(from_runtime) else {
        return refuse(ControlErrorCode::PermissionDenied, "Not a paired machine");
    };
    if !inbound && !contacted(&to, &delivery.source_session_id) {
        return refuse(
            ControlErrorCode::PermissionDenied,
            "This machine only accepts replies from yours: it hasn't allowed your machine to message its conversations first",
        );
    }
    let Some(target) = lookup_peer(&to, acp_sessions) else {
        return refuse(
            ControlErrorCode::NotFound,
            &format!("No such conversation, or it has stopped: {to}"),
        );
    };
    let bounce = delivery.kind == PeerMessageKind::Bounce;
    let source_posture = if source_bypasses {
        ApprovalPosture::FullAccess
    } else {
        ApprovalPosture::RequiresApproval
    };
    if !bounce
        && let Some(target_posture) = target.posture
        && !source_posture.allows_message_to(target_posture)
    {
        return refuse(
            ControlErrorCode::PermissionDenied,
            "The other conversation has higher or unverified permissions and you ask for approval each time, so you can't message it",
        );
    }
    delivery.source_title = match delivery.source_title.trim() {
        "" => machine.name.clone(),
        title => format!("{title} · {}", machine.name),
    };
    let source = delivery.source_session_id.clone();
    let hops = delivery.hops;
    let courier = Courier {
        acp_sessions: Arc::clone(acp_sessions),
        subscribers: subscribers.clone(),
    };
    let letter = Letter {
        delivery,
        to: to.clone(),
        to_title: target.title,
        state: LetterState::Queued,
        source_posture,
        source_agent_label: Some(machine.name),
        spawned: false,
    };
    if let Accepted::InboxFull = courier.post(letter, (!bounce).then_some(INBOX_LIMIT)) {
        return refuse(
            ControlErrorCode::Busy,
            &format!(
                "The other conversation has {INBOX_LIMIT} unread messages; wait until it catches up"
            ),
        );
    }
    if !bounce {
        remember_contact(&to, &source);
        lower_chain(&to, hops);
    }
    Ok(())
}

/// 别的机器问本机有哪些对话（`node.peer_list`）：形状同本机 `session_list` 的每一项。
pub(crate) fn list_for_peer(acp_sessions: &AcpSessions, query: Option<&str>) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = matching(directory("", acp_sessions), query)
        .into_iter()
        .map(|peer| {
            serde_json::json!({
                "id": peer.id,
                "title": peer.title,
                "agent": peer.agent,
                "agentLabel": peer.agent_label,
                "project": peer.project,
                "state": peer.state,
            })
        })
        .collect();
    serde_json::json!({"ok": true, "sessions": sessions})
}

/// 别的机器读本机一个对话最近的回复（`node.peer_read`），回答同本机 `session_read`。
pub(crate) fn read_for_peer(
    acp_sessions: &AcpSessions,
    session_id: &str,
    limit: usize,
) -> serde_json::Value {
    let body = serde_json::json!({"sessionId": session_id, "limit": limit});
    read("", acp_sessions, &body).unwrap_or_else(|error| error)
}

/// 别的机器上的对话：`<runtime_id>/<会话 id>`。
fn split_remote(id: &str) -> Option<(&str, &str)> {
    id.split_once('/')
        .filter(|(runtime, session)| !runtime.is_empty() && !session.is_empty())
}

/// 本机对话的 id 带上本机的 runtime id，别的机器照它回信。已经带着机器的原样。
fn qualify(own_runtime: &str, id: &str) -> String {
    if split_remote(id).is_some() {
        id.to_string()
    } else {
        format!("{own_runtime}/{id}")
    }
}

fn own_runtime_id() -> Result<String, String> {
    crate::runtime_identity::runtime_id()
        .map(|id| id.to_string())
        .ok_or_else(|| "This runtime has no identity yet".to_string())
}

fn remember_contact(target: &str, source: &str) {
    let mut contacts = CONTACTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pair = (target.to_string(), source.to_string());
    if contacts.contains(&pair) {
        return;
    }
    if contacts.len() >= CONTACTS_LIMIT {
        contacts.pop_front();
    }
    contacts.push_back(pair);
}

/// `source` 收到过 `remote` 发来的消息。
fn contacted(source: &str, remote: &str) -> bool {
    CONTACTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|(target, from)| target == source && from == remote)
}

/// 发给、读别的机器上的对话：那台机器要还是对端，而且用户允许过本机智能体用它，或者是
/// 回给发消息过来的对话。
fn remote_machine(
    source_id: &str,
    remote: &str,
    runtime: &str,
) -> Result<smelt_store::PeerNodeRecord, serde_json::Value> {
    let machine = crate::peer_nodes::peer_record(runtime).ok_or_else(|| not_found(remote))?;
    if !machine.agent_access && !contacted(source_id, remote) {
        return Err(rejected(
            "permission",
            format!(
                "Agents on this computer may not use the machine \"{}\". The user can allow it in Smelt's machine settings",
                machine.name
            ),
        ));
    }
    Ok(machine)
}

/// 发给谁：本机的对话查注册表和工作区目录；别的机器上的不知道标题和权限档位，标题写成
/// 「会话 id @ 机器名」，档位由那台机器收下时再比。
fn target_of(
    source_id: &str,
    to: &str,
    acp_sessions: &AcpSessions,
) -> Result<Peer, serde_json::Value> {
    let Some((runtime, session)) = split_remote(to) else {
        return lookup_peer(to, acp_sessions).ok_or_else(|| not_found(to));
    };
    let machine = remote_machine(source_id, to, runtime)?;
    Ok(Peer {
        id: to.to_string(),
        title: format!("{session} @ {}", machine.name),
        agent: String::new(),
        agent_label: String::new(),
        project: None,
        state: "remote",
        posture: None,
    })
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

/// 本机 `smelt peer` CLI 兜底入口：来源由调用方进程树归属到的 `session_id` 决定，
/// 复用与 Pi 扩展、MCP Server 完全一致的门卫和发件队列。
pub(crate) fn dispatch_local_peer_op(
    op: &str,
    session_id: &str,
    body: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<serde_json::Value, ControlError> {
    let Some(slot) = acp_sessions.get(session_id) else {
        return Err(ControlError::new(
            ControlErrorCode::NotFound,
            format!("Calling conversation is not running: {session_id}"),
        ));
    };
    if !is_live_peer(session_id, &slot.value) {
        return Err(ControlError::new(
            ControlErrorCode::PermissionDenied,
            "This conversation can't message other conversations",
        ));
    }
    if !crate::peer_tools::is_tool(op) {
        return Ok(rejected("invalid", format!("Unknown operation {op}")));
    }
    let courier = Courier {
        acp_sessions: Arc::clone(acp_sessions),
        subscribers: subscribers.clone(),
    };
    Ok(answer_tool(op, &slot, session_id, &courier, body))
}

/// 四个跨对话工具的回答，补上回给模型的文字和界面要的 `details`。扩展、MCP、CLI 都走这里。
fn answer_tool(
    op: &str,
    slot: &Arc<AcpSlot<AcpSession>>,
    session_id: &str,
    courier: &Courier,
    body: &serde_json::Value,
) -> serde_json::Value {
    let acp_sessions = &courier.acp_sessions;
    let reply = match op {
        "session_list" => list(session_id, acp_sessions, body),
        "session_read" => read(session_id, acp_sessions, body).unwrap_or_else(|error| error),
        "session_send" => send(slot, session_id, courier, body).unwrap_or_else(|error| error),
        _ => spawn(slot, session_id, courier, body).unwrap_or_else(|error| error),
    };
    crate::peer_tools::present(op, body, reply)
}

/// 起这个 ACP 会话时绑上的应答。来源是 `session_id`，请求体里的字段不算。
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
        // 工具定义不认来源，会话还没登记好时扩展就来要了。
        if op == crate::peer_tools::SESSION_TOOLS_OP {
            return serde_json::json!({"ok": true, "tools": crate::peer_tools::definitions()});
        }
        let courier = Courier {
            acp_sessions: Arc::clone(&acp_sessions),
            subscribers: subscribers.clone(),
        };
        let Some(slot) = source_slot.upgrade() else {
            return rejected(
                "not_found",
                "The conversation that sent the request was replaced",
            );
        };
        if !acp_sessions.is_current(&session_id, &slot) {
            return rejected(
                "not_found",
                "The conversation that sent the request was replaced",
            );
        }
        match op {
            op if crate::peer_tools::is_tool(op) => {
                answer_tool(op, &slot, &session_id, &courier, body)
            }
            "notify" => crate::agent_notify::from_session(
                &session_id,
                &slot.value,
                &courier.subscribers,
                body,
            ),
            _ => rejected("invalid", format!("Unknown operation {op}")),
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

fn workspace_directory() -> WorkspaceDirectoryReader {
    SERVER
        .get()
        .map(|server| server.workspace_directory.clone())
        .unwrap_or_else(WorkspaceDirectoryReader::unavailable)
}

/// 工作区目录里一段对话的名字：用户起的，其次 ACP 会话自己的标题。
fn directory_title(conversation: &smelt_core::workspace_directory::WorkspaceConversation) -> String {
    conversation
        .custom_title
        .as_deref()
        .or_else(|| conversation.acp.as_ref()?.session_title.as_deref())
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// 对话目录里的一项。
struct Peer {
    id: String,
    title: String,
    agent: String,
    agent_label: String,
    project: Option<String>,
    state: &'static str,
    /// 权限姿态。没打开的对话不知道，拉起之后再看。
    posture: Option<ApprovalPosture>,
}

/// 注册表里能互发消息的：在跑或挂起着的非自动化 ACP 对话（不限引擎）。
fn is_live_peer(id: &str, sess: &AcpSession) -> bool {
    if is_automation(id)
        || matches!(
            *sess.conversation_binding.lock(),
            Some(smelt_core::conversation::ConversationBinding::Automation { .. })
        )
    {
        return false;
    }
    acp_runtime_alive(sess) || sess.idle.is_suspended()
}

pub(crate) fn is_automation(id: &str) -> bool {
    id.starts_with("acp-automation-")
}

/// 解析一段对话的引擎种类和给人看的名字（优先产品智能体 / 自定义 Profile 名）。
fn agent_info_from_directory(
    acp: &smelt_core::workspace_directory::WorkspaceAcp,
    options: &[smelt_core::session_control::AcpAgentOption],
) -> (ConversationAgentKind, String) {
    let kind = ConversationAgentKind::from_id(&acp.agent)
        .or_else(|| {
            smelt_core::pi_rpc::is_smelt_pi_launch(&acp.launch)
                .then_some(ConversationAgentKind::Pi)
        })
        .or_else(|| ConversationAgentKind::from_command_loose(&acp.launch.command))
        .unwrap_or(ConversationAgentKind::Custom);
    let label = acp
        .agent_definition_id
        .as_deref()
        .and_then(|def_id| {
            options
                .iter()
                .find(|opt| opt.agent_definition_id() == Some(def_id))
                .map(|opt| opt.label.clone())
        })
        .or_else(|| {
            acp.profile_id.as_deref().and_then(|profile_id| {
                options
                    .iter()
                    .find(|opt| opt.profile_id() == Some(profile_id))
                    .map(|opt| opt.label.clone())
            })
        })
        .unwrap_or_else(|| kind.label().to_string());
    (kind, label)
}

fn agent_info_with(
    id: &str,
    sess: &AcpSession,
    directory: Option<&smelt_core::workspace_directory::WorkspaceDirectory>,
    options: &[smelt_core::session_control::AcpAgentOption],
) -> (ConversationAgentKind, String) {
    if let Some(acp) = directory.and_then(|dir| {
        dir.conversations
            .iter()
            .find_map(|conv| conv.acp.as_ref().filter(|acp| acp.sid == id))
    }) {
        return agent_info_from_directory(acp, options);
    }
    let kind = sess
        .launch_spec
        .lock()
        .as_ref()
        .map(|launch| {
            if smelt_core::pi_rpc::is_smelt_pi_launch(launch) {
                ConversationAgentKind::Pi
            } else {
                ConversationAgentKind::from_command_loose(&launch.command)
                    .unwrap_or(ConversationAgentKind::Custom)
            }
        })
        .unwrap_or(ConversationAgentKind::Custom);
    (kind, kind.label().to_string())
}

fn agent_info_of(id: &str, sess: &AcpSession) -> (ConversationAgentKind, String) {
    let dir = workspace_directory().read().ok();
    let options = dir
        .as_ref()
        .and_then(|dir| {
            dir.conversations
                .iter()
                .any(|conv| conv.acp.as_ref().is_some_and(|acp| acp.sid == id))
                .then(smelt_core::new_session::stored_conversation_options)
        })
        .unwrap_or_default();
    agent_info_with(id, sess, dir.as_ref(), &options)
}

fn title_with(
    id: &str,
    sess: &AcpSession,
    directory: Option<&smelt_core::workspace_directory::WorkspaceDirectory>,
) -> String {
    if let Some(title) = directory
        .and_then(|dir| {
            dir.conversations
                .iter()
                .find(|conversation| conversation.acp.as_ref().is_some_and(|acp| acp.sid == id))
                .map(directory_title)
        })
        .filter(|title| !title.is_empty())
    {
        return title;
    }
    let resolved = sess.reduced.lock().resolved_session_title();
    if let Some(title) = resolved {
        return title;
    }
    sess.cwd
        .as_deref()
        .and_then(|cwd| std::path::Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled conversation".to_string())
}

fn approval_posture_for(kind: ConversationAgentKind, sess: &AcpSession) -> ApprovalPosture {
    let mode = sess
        .reduced
        .lock()
        .config_options
        .iter()
        .find(|config| config.config_id == "mode")
        .and_then(crate::acp_idle::config_value);
    let launch = sess.launch_spec.lock().clone();
    kind.approval_posture(mode.as_deref(), launch.as_ref())
}

fn live_peer_with(
    id: &str,
    sess: &AcpSession,
    directory: Option<&smelt_core::workspace_directory::WorkspaceDirectory>,
    options: &[smelt_core::session_control::AcpAgentOption],
) -> Peer {
    let (kind, agent_label) = agent_info_with(id, sess, directory, options);
    Peer {
        id: id.to_string(),
        title: title_with(id, sess, directory),
        agent: kind.id().to_string(),
        agent_label,
        project: sess.cwd.clone(),
        state: state_of(sess),
        posture: Some(approval_posture_for(kind, sess)),
    }
}

fn live_peer(id: &str, sess: &AcpSession) -> Peer {
    let dir = workspace_directory().read().ok();
    let options = dir
        .as_ref()
        .and_then(|dir| {
            dir.conversations
                .iter()
                .any(|conv| conv.acp.as_ref().is_some_and(|acp| acp.sid == id))
                .then(smelt_core::new_session::stored_conversation_options)
        })
        .unwrap_or_default();
    live_peer_with(id, sess, dir.as_ref(), &options)
}

/// 工作区目录里、这次 daemon 还没打开过的非自动化 ACP 对话。
fn dormant_peers_with(
    acp_sessions: &AcpSessions,
    directory: Option<&smelt_core::workspace_directory::WorkspaceDirectory>,
    options: &[smelt_core::session_control::AcpAgentOption],
) -> Vec<Peer> {
    let Some(directory) = directory else {
        return Vec::new();
    };
    directory
        .conversations
        .iter()
        .filter_map(|conversation| {
            let acp = conversation.acp.as_ref()?;
            (!is_automation(&acp.sid) && acp_sessions.get(&acp.sid).is_none()).then(|| {
                let (kind, agent_label) = agent_info_from_directory(acp, options);
                Peer {
                    id: acp.sid.clone(),
                    title: directory_title(conversation),
                    agent: kind.id().to_string(),
                    agent_label,
                    project: acp.cwd.clone(),
                    state: "dormant",
                    posture: None,
                }
            })
        })
        .collect()
}

fn dormant_peers(acp_sessions: &AcpSessions) -> Vec<Peer> {
    let Ok(directory) = workspace_directory().read() else {
        return Vec::new();
    };
    let options = smelt_core::new_session::stored_conversation_options();
    dormant_peers_with(acp_sessions, Some(&directory), &options)
}

/// 能发给谁：注册表里的全部非自动化 ACP 对话，加侧栏上还没打开的。不含自己。
fn directory_with(
    source_id: &str,
    acp_sessions: &AcpSessions,
    directory: Option<&smelt_core::workspace_directory::WorkspaceDirectory>,
    options: &[smelt_core::session_control::AcpAgentOption],
) -> Vec<Peer> {
    let mut peers: Vec<Peer> = acp_sessions
        .snapshot()
        .into_iter()
        .filter(|(id, slot)| id != source_id && is_live_peer(id, &slot.value))
        .map(|(id, slot)| live_peer_with(&id, &slot.value, directory, options))
        .collect();
    peers.extend(
        dormant_peers_with(acp_sessions, directory, options)
            .into_iter()
            .filter(|peer| peer.id != source_id),
    );
    peers
}

fn directory(source_id: &str, acp_sessions: &AcpSessions) -> Vec<Peer> {
    let dir = workspace_directory().read().ok();
    let options = smelt_core::new_session::stored_conversation_options();
    directory_with(source_id, acp_sessions, dir.as_ref(), &options)
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
        format!(
            "No such conversation, or it has stopped: {id}. Use session_list to find conversations you can message"
        ),
    )
}

/// 侧栏上的名字：工作区目录里的（含用户改过的），其次远程目录里的，再次会话自己的标题。
pub(crate) fn title_of(id: &str, sess: &AcpSession) -> String {
    let dir = SERVER
        .get()
        .and_then(|server| server.workspace_directory.read().ok());
    title_with(id, sess, dir.as_ref())
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

/// 这条会话当前的权限姿态。值来自驱动报的配置项和启动规格，挂起的会话镜像里也还在。
fn approval_posture(id: &str, sess: &AcpSession) -> ApprovalPosture {
    let (kind, _) = agent_info_of(id, sess);
    approval_posture_for(kind, sess)
}

/// 本机可供 `session_spawn` 使用的智能体选项：产品智能体、已安装的内置引擎、自定义 Profile。
fn available_agents_with(
    options: &[smelt_core::session_control::AcpAgentOption],
) -> Vec<serde_json::Value> {
    let diagnostics = smelt_core::acp_conn::locate_acp_runtime();
    let definitions = smelt_core::agent_definition_store::load_agent_definitions();
    options
        .iter()
        .filter(|option| {
            if option.agent_definition_id().is_some() || option.profile {
                return true;
            }
            ConversationAgentKind::from_id(&option.kind).is_some_and(|kind| {
                diagnostics
                    .for_agent(kind)
                    .is_some_and(|runtime| runtime.is_available())
            })
        })
        .map(|option| {
            let category = if option.agent_definition_id().is_some() {
                "agent"
            } else if option.profile {
                "profile"
            } else {
                "engine"
            };
            let description = option.agent_definition_id().and_then(|def_id| {
                definitions
                    .iter()
                    .find(|def| def.id == def_id)
                    .map(|def| def.description.trim().to_string())
                    .filter(|desc| !desc.is_empty())
            });
            let mut item = serde_json::json!({
                "id": option.id,
                "kind": option.kind,
                "label": option.label,
                "category": category,
            });
            if let Some(description) = description {
                item["description"] = description.into();
            }
            item
        })
        .collect()
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
                    LetterState::Forwarded => "forwarded",
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

/// 按标题或 id 过滤，不分大小写。
fn matching(peers: Vec<Peer>, query: Option<&str>) -> Vec<Peer> {
    let query = query
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty());
    peers
        .into_iter()
        .filter(|peer| {
            query.as_ref().is_none_or(|query| {
                peer.title.to_lowercase().contains(query) || peer.id.to_lowercase().contains(query)
            })
        })
        .collect()
}

/// 允许本机智能体用的那几台机器上的对话，同时去问；连不上的写进 `machines` 里。
fn remote_sessions(
    source_id: &str,
    query: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let asked: Vec<_> = crate::peer_nodes::peers()
        .into_iter()
        .filter(|machine| machine.agent_access)
        .map(|machine| {
            let query = query.map(ToOwned::to_owned);
            thread::spawn(move || {
                let reply = crate::peer_nodes::peer_client(&machine)
                    .call::<smelt_core::control_api::NodePeerList>(
                    &smelt_core::peer_node::NodePeerListParams { query },
                    REMOTE_TIMEOUT,
                );
                (machine, reply)
            })
        })
        .collect();
    let mut sessions = Vec::new();
    let mut machines = Vec::new();
    for asked in asked {
        let Ok((machine, reply)) = asked.join() else {
            continue;
        };
        let listed = reply.map_err(|error| error.to_string()).and_then(|reply| {
            reply
                .get("sessions")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .ok_or_else(|| "It answered with no conversation list".to_string())
        });
        match listed {
            Ok(listed) => {
                for mut session in listed {
                    let Some(id) = session.get("id").and_then(serde_json::Value::as_str) else {
                        continue;
                    };
                    let id = format!("{}/{id}", machine.runtime_id);
                    session["sent"] = sent_to(source_id, &id).into();
                    session["id"] = id.into();
                    session["machine"] = machine.name.clone().into();
                    sessions.push(session);
                }
                machines.push(serde_json::json!({"name": machine.name}));
            }
            Err(error) => {
                machines.push(serde_json::json!({"name": machine.name, "error": error}));
            }
        }
    }
    (sessions, machines)
}

fn list(
    source_id: &str,
    acp_sessions: &AcpSessions,
    body: &serde_json::Value,
) -> serde_json::Value {
    let query = body.get("query").and_then(serde_json::Value::as_str);
    let include_agents = body
        .get("includeAgents")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let (remote, machines) = remote_sessions(source_id, query);
    let dir = workspace_directory().read().ok();
    let options = smelt_core::new_session::stored_conversation_options();
    let mut sessions: Vec<serde_json::Value> =
        matching(directory_with(source_id, acp_sessions, dir.as_ref(), &options), query)
            .into_iter()
            .map(|peer| {
                serde_json::json!({
                    "id": peer.id,
                    "title": peer.title,
                    "agent": peer.agent,
                    "agentLabel": peer.agent_label,
                    "project": peer.project,
                    "state": peer.state,
                    "sent": sent_to(source_id, &peer.id),
                })
            })
            .collect();
    sessions.extend(remote);
    let mut reply = serde_json::json!({"ok": true, "sessions": sessions, "machines": machines});
    if include_agents {
        reply["availableAgents"] = available_agents_with(&options).into();
    }
    reply
}

fn read(
    source_id: &str,
    acp_sessions: &AcpSessions,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let id =
        text_field(body, "sessionId").ok_or_else(|| rejected("invalid", "missing sessionId"))?;
    if id == source_id {
        return Err(rejected("self", "That's your own conversation"));
    }
    let limit = body
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(1, |limit| (limit as usize).clamp(1, READ_LIMIT_MAX));
    if let Some((runtime, session)) = split_remote(&id) {
        let machine = remote_machine(source_id, &id, runtime)?;
        let mut reply = crate::peer_nodes::peer_client(&machine)
            .call::<smelt_core::control_api::NodePeerRead>(
                &smelt_core::peer_node::NodePeerReadParams {
                    session_id: session.to_string(),
                    limit,
                },
                REMOTE_TIMEOUT,
            )
            .map_err(|error| {
                rejected(
                    "unreachable",
                    format!("Couldn't reach \"{}\": {error}", machine.name),
                )
            })?;
        if reply.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return Err(reply);
        }
        let title = reply
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(session)
            .to_string();
        reply["sessionId"] = id.clone().into();
        reply["title"] = format!("{title} @ {}", machine.name).into();
        return Ok(reply);
    }
    let Some(slot) = acp_sessions
        .get(&id)
        .filter(|slot| is_live_peer(&id, &slot.value))
    else {
        if lookup_peer(&id, acp_sessions).is_some() {
            return Err(rejected(
                "not_running",
                "This conversation isn't open, so its replies can't be read. Use session_send to give it work; sending a message opens it",
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
        text_field(body, "toolCallId").ok_or_else(|| rejected("invalid", "missing toolCallId"))?;
    let to = text_field(body, "to").ok_or_else(|| rejected("invalid", "missing to"))?;
    let text =
        text_field(body, "text").ok_or_else(|| rejected("invalid", "The message is empty"))?;
    if text.chars().count() > TEXT_LIMIT_CHARS {
        return Err(rejected(
            "invalid",
            format!("The message is too long (max {TEXT_LIMIT_CHARS} characters)"),
        ));
    }
    if to == source_id {
        return Err(rejected("self", "You can't message yourself"));
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
    let target = target_of(source_id, &to, &courier.acp_sessions)?;
    let source = &source_slot.value;
    let (source_kind, source_agent_label) = agent_info_of(source_id, source);
    let source_posture = approval_posture_for(source_kind, source);
    if let Some(target_posture) = target.posture
        && !source_posture.allows_message_to(target_posture)
    {
        return Err(with_target(
            rejected(
                "permission",
                "The other conversation has higher or unverified permissions and you ask for approval each time, so you can't message it",
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
                "This message chain is out of hops: until the user speaks in this conversation, every send will be rejected, so don't retry. Tell the user what the other conversation needs to know",
            ),
            &to,
            &target.title,
        ));
    }
    let reply_to = text_field(body, "replyTo");
    let hops = budget - 1;
    let letter = Letter {
        delivery: PeerDelivery {
            id: id.clone(),
            source_session_id: source_id.to_string(),
            source_title: title_of(source_id, source),
            kind,
            hops,
            reply_to: reply_to.clone(),
            text,
        },
        to: to.clone(),
        to_title: target.title.clone(),
        state: LetterState::Queued,
        source_posture,
        source_agent_label: Some(source_agent_label),
        spawned: false,
    };
    match courier.post(letter, Some(INBOX_LIMIT)) {
        Accepted::New => {}
        Accepted::Replay => {
            return replay_reply(&id)
                .ok_or_else(|| rejected("invalid", "This message was just cleared"));
        }
        Accepted::InboxFull => {
            return Err(with_target(
                rejected(
                    "inbox_full",
                    format!(
                        "The other conversation has {INBOX_LIMIT} unread messages; wait until it catches up"
                    ),
                ),
                &to,
                &target.title,
            ));
        }
    }
    mark_task_replied(source, &to, kind, reply_to.as_deref());
    lower_chain(&to, hops);
    Ok(accepted_reply(&id, &to, &target.title, hops))
}

/// 发起方在本轮里显式回了信：标掉当前 `active_peer_task`，回合结束时不再自动兜底重发。
fn mark_task_replied(
    source: &AcpSession,
    to: &str,
    kind: PeerMessageKind,
    reply_to: Option<&str>,
) {
    let mut active = source.active_peer_task.lock();
    let Some(task) = active.as_mut() else {
        return;
    };
    let answers_task = reply_to == Some(task.message_id.as_str())
        || (to == task.source_session_id && kind == PeerMessageKind::Completion);
    if answers_task {
        task.replied = true;
    }
}

fn auto_complete_body(
    reply_text: &str,
    outcome: Option<smelt_core::acp_session::AcpTurnOutcome>,
) -> String {
    let trimmed = reply_text.trim();
    match outcome {
        Some(smelt_core::acp_session::AcpTurnOutcome::Cancelled) => {
            if trimmed.is_empty() {
                "Task was cancelled before completion.".to_string()
            } else {
                format!("Task was cancelled before completion.\n\n{trimmed}")
            }
        }
        other => match other.and_then(|o| o.failure()) {
            Some(failure) => {
                if trimmed.is_empty() {
                    format!("Task ended without completing ({failure:?}).")
                } else {
                    format!("Task ended without completing ({failure:?}).\n\n{trimmed}")
                }
            }
            None => {
                if trimmed.is_empty() {
                    "Task finished with no text reply.".to_string()
                } else {
                    trimmed.to_string()
                }
            }
        },
    }
}

/// 非 Pi 对话执行跨对话 `Task` 回合结束但未显式调 `session_send` 回报时，
/// 由回合结束事件自动把最终结果投回原发起方（只对 `Task` 触发，不防乒乓之外的消息）。
pub(crate) fn auto_complete_task(
    session_id: &str,
    sess: &AcpSession,
    task: crate::acp_host::ActivePeerTask,
    reply_text: String,
    outcome: Option<smelt_core::acp_session::AcpTurnOutcome>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let (kind, agent_label) = agent_info_of(session_id, sess);
    let source_posture = approval_posture_for(kind, sess);
    // 若发起方是高权限对话而本对话处于逐次审批，自动回传会被门卫拒绝并退信唤醒本对话；
    // 这种情况下直接跳过自动兜底，不制造幽灵退信循环。
    if split_remote(&task.source_session_id).is_none()
        && let Some(target_slot) = acp_sessions.get(&task.source_session_id)
        && is_live_peer(&task.source_session_id, &target_slot.value)
        && !source_posture
            .allows_message_to(approval_posture(&task.source_session_id, &target_slot.value))
    {
        eprintln!(
            "[peer] 跳过 {session_id} -> {} 的自动回报：当前权限姿态 {:?} 不允许向高权限发起方回信",
            task.source_session_id, source_posture
        );
        return;
    }
    let body = auto_complete_body(&reply_text, outcome);
    let courier = Courier {
        acp_sessions: Arc::clone(acp_sessions),
        subscribers: subscribers.clone(),
    };
    let letter = Letter {
        delivery: PeerDelivery {
            id: format!("{}-auto", task.message_id),
            source_session_id: session_id.to_string(),
            source_title: title_of(session_id, sess),
            kind: PeerMessageKind::Completion,
            hops: task.hops.saturating_sub(1),
            reply_to: Some(task.message_id),
            text: truncate_chars(&body, TEXT_LIMIT_CHARS),
        },
        to: task.source_session_id.clone(),
        to_title: task.source_title,
        state: LetterState::Queued,
        source_posture,
        source_agent_label: Some(agent_label),
        spawned: false,
    };
    let _ = courier.post(letter, None);
    lower_chain(&task.source_session_id, task.hops.saturating_sub(1));
}

/// 校验 `session_spawn` 显式指定的 `cwd`：必须是绝对路径、已存在目录，且落在当前工作区已打开的某个项目内。
fn validate_spawn_cwd(cwd: &str) -> Result<String, serde_json::Value> {
    let path = std::path::Path::new(cwd);
    if !path.is_absolute() {
        return Err(rejected(
            "invalid",
            format!("cwd must be an absolute path: {cwd}"),
        ));
    }
    if !path.is_dir() {
        return Err(rejected(
            "invalid",
            format!("cwd does not exist or is not a directory: {cwd}"),
        ));
    }
    let directory = workspace_directory()
        .read()
        .map_err(|error| rejected("invalid", format!("Couldn't read workspace directory: {error}")))?;
    let style = smelt_core::runtime_path::PathStyle::of_os(Some(std::env::consts::OS));
    if smelt_core::workspace_directory::project_root_of(style, &directory.projects, cwd).is_none() {
        return Err(rejected(
            "permission",
            format!("cwd must be inside an open workspace project: {cwd}"),
        ));
    }
    Ok(cwd.to_string())
}

/// 跨引擎 `session_spawn` 且发起方不是 `FullAccess` 时，过滤掉目标默认配置里的全权限 mode。
fn filter_full_access_config(
    kind: ConversationAgentKind,
    config: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let Some(full_mode) = kind.task_params().full_access_mode else {
        return config;
    };
    config
        .into_iter()
        .filter(|(key, value)| !(key == "mode" && value == full_mode))
        .collect()
}

/// 起一个新对话（默认沿用发起方，也可经 `agent` 指定任意产品智能体 / 第三方引擎 / 自定义 Profile），
/// 交给它一个任务。同步只做检查、记账，建对话在后台。见模块说明。
fn spawn(
    source_slot: &Arc<AcpSlot<AcpSession>>,
    source_id: &str,
    courier: &Courier,
    body: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let call =
        text_field(body, "toolCallId").ok_or_else(|| rejected("invalid", "missing toolCallId"))?;
    let title = text_field(body, "title").ok_or_else(|| rejected("invalid", "missing title"))?;
    let title = truncate_chars(&title, TITLE_LIMIT_CHARS);
    let task = text_field(body, "task").ok_or_else(|| rejected("invalid", "The task is empty"))?;
    if task.chars().count() > TEXT_LIMIT_CHARS {
        return Err(rejected(
            "invalid",
            format!("The task is too long (max {TEXT_LIMIT_CHARS} characters)"),
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
        .ok_or_else(|| rejected("invalid", "This process can't start new conversations"))?;
    if was_spawned(source_id) {
        return Err(rejected(
            "spawn_limit",
            "A conversation started by another one can't start new conversations. To get help, use session_send to hand work to an existing conversation",
        ));
    }
    let budget = chain_budget(source_id);
    if budget == 0 {
        return Err(rejected(
            "hops_exhausted",
            "This message chain is out of hops: until the user speaks in this conversation, you can't start new conversations, so don't retry. Tell the user what needs doing",
        ));
    }

    let source = &source_slot.value;
    let (source_kind, source_agent_label) = agent_info_of(source_id, source);
    let source_posture = approval_posture_for(source_kind, source);
    let requested_agent = text_field(body, "agent");
    let requested_cwd = text_field(body, "cwd");

    let (
        target_agent_id,
        target_profile_id,
        target_agent_definition_id,
        launch,
        ephemeral_env,
        initial_config,
        copy_source_config,
    ) = if let Some(agent_opt_id) = requested_agent {
        let option = smelt_core::session_control::find_agent_option(&agent_opt_id).ok_or_else(
            || {
                rejected(
                    "not_found",
                    format!(
                        "Unknown agent option: {agent_opt_id}. Check availableAgents from session_list"
                    ),
                )
            },
        )?;
        let target_kind =
            ConversationAgentKind::from_id(&option.kind).unwrap_or(ConversationAgentKind::Custom);
        let mut target_launch = option.launch.clone();
        let mut target_config = smelt_core::session_control::new_conversation_config(&option);
        if source_posture != ApprovalPosture::FullAccess {
            if !target_kind.strip_full_access(&mut target_launch) {
                return Err(rejected(
                    "permission",
                    format!(
                        "\"{}\" has unverified permissions and you ask for approval each time, so you can't spawn it",
                        option.label
                    ),
                ));
            }
            target_config = filter_full_access_config(target_kind, target_config);
        }
        (
            target_kind.id().to_string(),
            option.profile_id().map(str::to_string),
            option.agent_definition_id().map(str::to_string),
            target_launch,
            BTreeMap::new(),
            target_config,
            false,
        )
    } else {
        let launch = source
            .launch_spec
            .lock()
            .clone()
            .ok_or_else(|| rejected("invalid", "Couldn't find this conversation's launch spec"))?;
        let ephemeral_env = source.ephemeral_env.lock().clone();
        let source_entry = workspace_directory().read().ok().and_then(|directory| {
            directory
                .conversations
                .into_iter()
                .find_map(|conversation| conversation.acp.filter(|acp| acp.sid == source_id))
        });
        (
            source_entry
                .as_ref()
                .map(|acp| acp.agent.clone())
                .unwrap_or_else(|| source_kind.id().to_string()),
            source_entry.as_ref().and_then(|acp| acp.profile_id.clone()),
            source_entry.and_then(|acp| acp.agent_definition_id),
            launch,
            ephemeral_env,
            Vec::new(),
            true,
        )
    };

    let cwd = match requested_cwd {
        Some(cwd) => validate_spawn_cwd(&cwd)?,
        None => match source.cwd.clone() {
            Some(cwd) => cwd,
            None => {
                let def_id = target_agent_definition_id.as_deref().ok_or_else(|| {
                    rejected("invalid", "This conversation has no working directory")
                })?;
                smelt_core::agent_definition_store::ensure_agent_space(def_id)
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .ok_or_else(|| {
                        rejected("invalid", "Couldn't prepare the agent's workspace directory")
                    })?
            }
        },
    };

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
                format!(
                    "You already have {SPAWN_LIMIT} conversations you started still open; use session_send to hand work to them first"
                ),
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
        source_posture,
        source_agent_label: Some(source_agent_label),
        spawned: true,
    };
    {
        let mut outbox = lock_outbox();
        // 同一个 id 已经在 `SPAWNED` 里挡住了，这里只会是新的。
        outbox.accept(letter, None);
    }
    lower_chain(&id, hops);

    let entry = smelt_core::workspace_directory::WorkspaceConversation {
        id: uuid::Uuid::new_v4().to_string(),
        custom_title: Some(title.clone()),
        acp: Some(smelt_core::workspace_directory::WorkspaceAcp {
            cwd: Some(cwd.clone()),
            sid: id.clone(),
            agent: target_agent_id,
            profile_id: target_profile_id,
            launch: launch.clone(),
            agent_definition_id: target_agent_definition_id,
            ..Default::default()
        }),
        ..Default::default()
    };
    let request = AcpOpenRequest {
        id: id.clone(),
        cwd: Some(cwd),
        launch,
        ephemeral_env,
        resume_id: None,
        fork_id: None,
        fork_cut: None,
        conversation_binding: None,
        pending_agent_preset: None,
        omit_history: false,
        initial_config,
        initial_prompt: None,
        disallowed_tools: Vec::new(),
    };
    let reply = accepted_reply(&message, &id, &title, hops);
    let source_slot = Arc::clone(source_slot);
    let courier = courier.clone();
    let _ = thread::Builder::new()
        .name("smelt-peer-spawn".into())
        .spawn(move || {
            let built = build_spawned(
                &request,
                entry,
                &title,
                &source_slot.value,
                copy_source_config,
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

/// 建新对话、写好配置和标题、写进工作区目录。哪一步失败就把建了一半的对话撤掉，侧栏上
/// 不留空对话。
fn build_spawned(
    request: &AcpOpenRequest,
    entry: smelt_core::workspace_directory::WorkspaceConversation,
    title: &str,
    source: &AcpSession,
    copy_source_config: bool,
    server: &crate::protocol::ServerShared,
    courier: &Courier,
) -> Result<(), String> {
    let slot = ensure_acp_session(
        request,
        &server.sessions,
        &courier.acp_sessions,
        &courier.subscribers,
    )
    .map_err(|error| format!("Couldn't start the new conversation: {error}"))?;
    let ready_step = if copy_source_config {
        crate::acp_idle::copy_session_config(
            source,
            &request.id,
            &slot.value,
            &courier.subscribers,
        )
        .map_err(|error| format!("It didn't become ready: {error}"))
    } else {
        crate::acp_idle::wait_for_acp_ready(&slot.value, crate::acp_idle::ACP_WAKE_READY_TIMEOUT)
            .map_err(|error| format!("It didn't become ready: {error}"))
    };
    let configured = ready_step
        .and_then(|()| {
            apply_acp_user_action(
                &slot.value,
                AcpUserAction::SetSessionTitle {
                    title: title.to_string(),
                },
                &courier.subscribers,
            )
            .map_err(|error| format!("Couldn't set the title: {error}"))
        })
        .and_then(|()| {
            crate::workspace_directory::default_store()
                .and_then(|store| {
                    crate::workspace_directory::enter_conversation(
                        &store,
                        entry,
                        &courier.subscribers,
                    )
                })
                .map(|_| ())
                .map_err(|error| {
                    format!(
                        "Couldn't add it to the workspace directory: {}",
                        error.message
                    )
                })
        });
    if let Err(reason) = configured {
        if let Err(error) = kill_acp_session(
            &request.id,
            &courier.acp_sessions,
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
    lock_spawned()
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
    lock_chains().get(session_id).copied().unwrap_or(INITIAL_HOPS)
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
    hex::encode(hasher.finalize())
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
            source_posture: ApprovalPosture::RequiresApproval,
            source_agent_label: None,
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
        let restarted = crate::acp_host::restart_acp_session_from_client(
            "acp-panic",
            &panicked,
            &acp_sessions,
            &hub,
        );
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

    fn far_machine(runtime_id: &str, agent_access: bool) {
        smelt_core::sqlite_state::enable_sqlite_state();
        smelt_core::sqlite_state::default_sqlite_store()
            .unwrap()
            .put_peer_node(&smelt_store::PeerNodeRecord {
                runtime_id: runtime_id.into(),
                endpoint_id: format!("ep-{runtime_id}"),
                name: format!("{runtime_id}-box"),
                relay_url: None,
                os: None,
                agent_access,
                inbound: true,
                added_at_ms: 0,
            })
            .unwrap();
    }

    /// 别的机器上的对话写成 `<runtime_id>/<会话 id>`；本机对话的 id 交出去时带上本机。
    #[test]
    fn remote_ids_carry_their_machine() {
        assert_eq!(split_remote("rt-far/acp-x"), Some(("rt-far", "acp-x")));
        assert_eq!(split_remote("acp-x"), None);
        assert_eq!(split_remote("/acp-x"), None);
        assert_eq!(qualify("rt-me", "acp-x"), "rt-me/acp-x");
        assert_eq!(
            qualify("rt-me", "rt-far/acp-x"),
            "rt-far/acp-x",
            "已经带着机器的原样"
        );
    }

    /// 智能体主动找别的机器要用户允许过；回给发消息过来的那个对话不用。不是对端的机器
    /// 当作找不到。
    #[test]
    fn reaching_another_machine_needs_agent_access_or_a_message_from_it() {
        far_machine("rt-locked", false);
        far_machine("rt-open", true);
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let denied = target_of("acp-me", "rt-locked/acp-x", &acp_sessions)
            .err()
            .unwrap();
        assert_eq!(denied["reason"], "permission");
        let open = target_of("acp-me", "rt-open/acp-x", &acp_sessions)
            .ok()
            .unwrap();
        assert_eq!(open.title, "acp-x @ rt-open-box");
        assert_eq!(open.posture, None, "档位由那台机器收下时再比");
        assert_eq!(
            target_of("acp-me", "rt-unpaired/acp-x", &acp_sessions)
                .err()
                .unwrap()["reason"],
            "not_found"
        );

        remember_contact("acp-me", "rt-locked/acp-x");
        assert!(target_of("acp-me", "rt-locked/acp-x", &acp_sessions).is_ok());
        assert!(
            target_of("acp-other", "rt-locked/acp-x", &acp_sessions).is_err(),
            "只有收到过它消息的那个对话能回"
        );
    }

    /// 别的机器交来的信，来源必须是它自己那台机器上的对话；投给本机不存在的对话直接拒。
    #[test]
    fn a_machine_can_only_deliver_from_its_own_conversations() {
        far_machine("rt-sender", false);
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let deliver = |source: &str, to: &str| {
            accept_remote(
                "rt-sender",
                true,
                smelt_core::peer_node::NodePeerDeliverParams {
                    to: to.into(),
                    delivery: PeerDelivery {
                        id: "pm-remote".into(),
                        source_session_id: source.into(),
                        source_title: "far".into(),
                        kind: PeerMessageKind::Task,
                        hops: 3,
                        reply_to: None,
                        text: "hi".into(),
                    },
                    source_bypasses: false,
                },
                &acp_sessions,
                &crate::new_event_hub(),
            )
            .unwrap_err()
            .code
        };
        assert_eq!(
            deliver("rt-someone-else/acp-s", "acp-t"),
            ControlErrorCode::InvalidParams
        );
        assert_eq!(deliver("acp-s", "acp-t"), ControlErrorCode::InvalidParams);
        assert_eq!(
            deliver("rt-sender/acp-s", "acp-missing"),
            ControlErrorCode::NotFound
        );
    }

    /// 没被允许连入本机的机器只能交回信：本机对话之前和它那个对话通过信。
    #[test]
    fn a_machine_not_allowed_in_can_only_deliver_replies() {
        far_machine("rt-outsider", false);
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let deliver = |to: &str| {
            accept_remote(
                "rt-outsider",
                false,
                smelt_core::peer_node::NodePeerDeliverParams {
                    to: to.into(),
                    delivery: PeerDelivery {
                        id: format!("pm-outsider-{to}"),
                        source_session_id: "rt-outsider/acp-s".into(),
                        source_title: "far".into(),
                        kind: PeerMessageKind::Completion,
                        hops: 3,
                        reply_to: None,
                        text: "done".into(),
                    },
                    source_bypasses: false,
                },
                &acp_sessions,
                &crate::new_event_hub(),
            )
            .unwrap_err()
            .code
        };
        assert_eq!(
            deliver("acp-never-wrote"),
            ControlErrorCode::PermissionDenied
        );
        // 本机对话发过去之后，那边的回信过了这一关（对话不在，接着按找不到拒）。
        remember_contact("acp-wrote", "rt-outsider/acp-s");
        assert_eq!(deliver("acp-wrote"), ControlErrorCode::NotFound);
    }

    /// 别的机器列本机的对话：自己这台机器上的都列，不按来源排除。
    #[test]
    fn a_machine_lists_every_local_conversation() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let _a = live_pi(&acp_sessions, "acp-listed-a");
        let listed = list_for_peer(&acp_sessions, Some("listed-a"));
        let ids: Vec<&str> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|session| session["id"].as_str())
            .collect();
        assert_eq!(ids, ["acp-listed-a"]);
    }

    fn live_agent(
        acp_sessions: &AcpSessions,
        id: &str,
        kind: ConversationAgentKind,
        mode: Option<&str>,
    ) -> (
        Arc<AcpSlot<AcpSession>>,
        smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand>,
    ) {
        let slot = acp_sessions.reserve_with(id, || {
            crate::acp_host::make_acp_session(id, Some("/work".into()), None, None)
        });
        let launch = if kind == ConversationAgentKind::Custom {
            smelt_core::agent_kind::ConversationLaunchSpec::from_command("my-custom-acp")
        } else {
            kind.default_launch()
        };
        *slot.0.value.launch_spec.lock() = Some(launch);
        if let Some(mode) = mode {
            slot.0.value.reduced.lock().config_options =
                vec![smelt_core::acp_conn::SessionConfigState {
                    config_id: "mode".into(),
                    name: "Mode".into(),
                    description: None,
                    current_name: mode.into(),
                    options: vec![(mode.into(), mode.into())],
                    is_thought_level: false,
                    boolean: None,
                    smelt: None,
                }];
        }
        slot.0.value.reduced.lock().phase = smelt_core::daemon_state::DaemonPhase::Idle;
        let (cmd_tx, cmd_rx) = smol::channel::unbounded();
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
        (slot.0, cmd_rx)
    }

    #[test]
    fn non_pi_agents_appear_in_session_list_and_enforce_approval_posture() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let (pi_ask, _) = live_agent(
            &acp_sessions,
            "acp-pi-ask",
            ConversationAgentKind::Pi,
            Some("default"),
        );
        let (codex_full, _) = live_agent(
            &acp_sessions,
            "acp-codex-full",
            ConversationAgentKind::Codex,
            Some("agent-full-access"),
        );
        let (codex_ask, _) = live_agent(
            &acp_sessions,
            "acp-codex-ask",
            ConversationAgentKind::Codex,
            Some("read-only"),
        );
        let (_custom_unknown, _) = live_agent(
            &acp_sessions,
            "acp-custom-unk",
            ConversationAgentKind::Custom,
            None,
        );
        let hub = crate::new_event_hub();

        let ask_handler = bind_peer_handler(
            "acp-pi-ask".into(),
            Arc::downgrade(&pi_ask),
            Arc::clone(&acp_sessions),
            hub.clone(),
        );
        let listed = ask_handler(
            "session_list",
            &serde_json::json!({"includeAgents": false}),
        );
        let sessions = listed["sessions"].as_array().unwrap();
        assert!(sessions.iter().any(|s| s["id"] == "acp-codex-full" && s["agent"] == "codex"));
        assert!(sessions.iter().any(|s| s["id"] == "acp-custom-unk" && s["agent"] == "custom"));

        // 逐次审批不能发给全权限 Codex 或权限未知的 Custom ACP。
        let denied_codex = ask_handler(
            "session_send",
            &serde_json::json!({
                "to": "acp-codex-full",
                "text": "hi",
                "toolCallId": "tc-deny-codex"
            }),
        );
        assert_eq!(denied_codex["ok"], false);
        assert_eq!(denied_codex["reason"], "permission");

        let denied_custom = ask_handler(
            "session_send",
            &serde_json::json!({
                "to": "acp-custom-unk",
                "text": "hi",
                "toolCallId": "tc-deny-custom"
            }),
        );
        assert_eq!(denied_custom["ok"], false);
        assert_eq!(denied_custom["reason"], "permission");

        // 逐次审批可以发给同样逐次审批的 Codex；全权限 Codex 可以发给 Custom ACP。
        let ok_codex = ask_handler(
            "session_send",
            &serde_json::json!({
                "to": "acp-codex-ask",
                "text": "帮我看下代码",
                "kind": "task",
                "toolCallId": "tc-ok-codex"
            }),
        );
        assert_eq!(ok_codex["ok"], true, "{ok_codex}");

        let full_handler = bind_peer_handler(
            "acp-codex-full".into(),
            Arc::downgrade(&codex_full),
            Arc::clone(&acp_sessions),
            hub,
        );
        let ok_custom = full_handler(
            "session_send",
            &serde_json::json!({
                "to": "acp-custom-unk",
                "text": "请评审这个改动",
                "kind": "task",
                "toolCallId": "tc-ok-custom"
            }),
        );
        assert_eq!(ok_custom["ok"], true, "{ok_custom}");
        let _ = codex_ask;
    }

    #[test]
    fn non_pi_delivery_sends_formatted_prompt_records_task_note_and_queues_when_busy() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let (codex, cmd_rx) = live_agent(
            &acp_sessions,
            "acp-codex-rx",
            ConversationAgentKind::Codex,
            Some("read-only"),
        );
        let hub = crate::new_event_hub();
        let first = PeerDelivery {
            id: "pm-nonpi-1".into(),
            source_session_id: "acp-claude".into(),
            source_title: "主控".into(),
            kind: PeerMessageKind::Task,
            hops: 6,
            reply_to: None,
            text: "实现刷新逻辑".into(),
        };
        crate::acp_host::deliver_peer_message(&codex.value, first, Some("Claude Code"), &hub)
            .expect("idle non-pi should deliver immediately");
        match cmd_rx.try_recv().expect("should send ACP Prompt command") {
            smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
                assert!(text.contains("<peer-message"));
                assert!(text.contains("实现刷新逻辑"));
                assert!(text.contains("smelt peer send"), "未连 MCP 时引导用 CLI");
            }
            _ => panic!("expected Prompt command"),
        }
        assert!(matches!(
            codex.value.reduced.lock().entries().first(),
            Some(smelt_core::acp_chat::AcpEntry::TaskNote(note))
                if note.kind == smelt_core::acp_chat::TaskNoteKind::PeerMessage
                    && note.id == "pm-nonpi-1"
        ));
        assert!(codex.value.active_peer_task.lock().is_some());

        // 正忙时第二条进入队列，不并发发给 provider，且不可被用户撤回。
        let second = PeerDelivery {
            id: "pm-nonpi-2".into(),
            source_session_id: "acp-claude".into(),
            source_title: "主控".into(),
            kind: PeerMessageKind::Message,
            hops: 5,
            reply_to: None,
            text: "补充一条说明".into(),
        };
        crate::acp_host::deliver_peer_message(&codex.value, second, Some("Claude Code"), &hub)
            .expect("busy non-pi should queue peer message");
        assert!(cmd_rx.try_recv().is_err(), "busy 时不能并发发第二个 Prompt");
        assert_eq!(codex.value.pending_prompts.lock().len(), 1);
        assert!(
            codex.value.reduced.lock().pending_prompts.is_empty(),
            "peer 来信由 peer_inbox 展示，不混进用户排队条"
        );

        // 排队期间 MCP 完成握手后，放闸派发时应实时采用 Tool 引导，而不是入队时的旧 CLI 文案。
        codex.value.peer_mcp_connected.store(true, Ordering::SeqCst);
        codex.value.prompt_in_flight.store(false, Ordering::SeqCst);
        codex.value.reduced.lock().phase = smelt_core::daemon_state::DaemonPhase::Idle;
        {
            let _guard = codex.value.turn_completion.lock();
            crate::acp_host::flush_pending_acp_prompt_locked(&codex.value, &hub);
        }
        match cmd_rx
            .try_recv()
            .expect("flushed peer prompt should send to driver")
        {
            smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
                assert!(text.contains("补充一条说明"));
                assert!(
                    text.contains("session_send"),
                    "MCP 握手后放闸派发应切到 session_send 工具引导，实际：{text}"
                );
            }
            _ => panic!("expected Prompt command"),
        }

        // 旧代崩溃残留的 Pending 记录（不在 pending_prompts 里）不应吞掉重投。
        codex.value.prompt_in_flight.store(false, Ordering::SeqCst);
        codex.value.reduced.lock().phase = smelt_core::daemon_state::DaemonPhase::Idle;
        codex
            .value
            .non_pi_peer_inbox
            .lock()
            .push(smelt_core::acp_session::PeerMessageView {
                id: "pm-stale-pending".into(),
                source_session_id: "acp-claude".into(),
                source_title: "主控".into(),
                kind: PeerMessageKind::Message,
                status: smelt_core::acp_session::PeerMessageStatus::Pending,
                queued_at_ms: 1,
                delivered_at_ms: None,
            });
        let redelivery = PeerDelivery {
            id: "pm-stale-pending".into(),
            source_session_id: "acp-claude".into(),
            source_title: "主控".into(),
            kind: PeerMessageKind::Message,
            hops: 5,
            reply_to: None,
            text: "换代后重投的消息".into(),
        };
        crate::acp_host::deliver_peer_message(&codex.value, redelivery, None, &hub)
            .expect("stale pending entry should allow redelivery");
        match cmd_rx
            .try_recv()
            .expect("stale pending redelivery must reach driver")
        {
            smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => {
                assert!(text.contains("换代后重投的消息"));
            }
            _ => panic!("expected Prompt command"),
        }
    }

    #[test]
    fn auto_complete_body_reports_cancellation_and_failures_distinctly() {
        use smelt_core::acp_session::AcpTurnOutcome;
        let cancelled = auto_complete_body("", Some(AcpTurnOutcome::Cancelled));
        assert!(cancelled.contains("cancelled"), "{cancelled}");
        let cancelled_partial = auto_complete_body("做到一半", Some(AcpTurnOutcome::Cancelled));
        assert!(cancelled_partial.contains("cancelled"));
        assert!(cancelled_partial.contains("做到一半"));

        let failed = auto_complete_body("", Some(AcpTurnOutcome::MaxTokens));
        assert!(failed.contains("MaxTokens"), "{failed}");

        assert_eq!(
            auto_complete_body("全部完成", Some(AcpTurnOutcome::Succeeded)),
            "全部完成"
        );
        assert_eq!(
            auto_complete_body("   ", Some(AcpTurnOutcome::Succeeded)),
            "Task finished with no text reply."
        );
    }

    #[test]
    fn mark_task_replied_suppresses_auto_completion_only_when_answering_task() {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        let (codex, _) = live_agent(
            &acp_sessions,
            "acp-codex-reply",
            ConversationAgentKind::Codex,
            Some("read-only"),
        );
        *codex.value.active_peer_task.lock() = Some(crate::acp_host::ActivePeerTask {
            message_id: "pm-task-1".into(),
            source_session_id: "acp-boss".into(),
            source_title: "Boss".into(),
            hops: 5,
            replied: false,
        });
        mark_task_replied(&codex.value, "acp-other", PeerMessageKind::Message, None);
        assert!(!codex.value.active_peer_task.lock().as_ref().unwrap().replied);
        mark_task_replied(
            &codex.value,
            "acp-boss",
            PeerMessageKind::Completion,
            Some("pm-task-1"),
        );
        assert!(codex.value.active_peer_task.lock().as_ref().unwrap().replied);
    }
}
