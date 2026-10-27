//! 别的 Pi 对话发来的消息：这个会话这一头。
//!
//! - 守护把消息交给这里（`deliver`），排队后和后台任务、子智能体的结果走同一条
//!   监听交回会话：扩展写成 `smelt.peer_message` 自定义消息，`message_start` 时回 ack 才算送到；
//!   `clear_queue` 清掉的、换了监听的，再写一次。
//! - 扩展的 `session_list` / `session_read` / `session_send` / `session_spawn` 不在这里回答。
//!   起会话时守护把应答函数装进这个会话；来源是那时候绑上的会话 id，请求体里的字段不算。
//!
//! 只认会话本身（空归属）：子智能体不收也不发。

use std::io::Write;

use crate::acp_conn::ConversationEvent;
use crate::acp_session::{PeerDelivery, PeerMessageStatus, PeerMessageView};

use super::{Inner, ModelNotice, State, TaskStream, now_ms, poke, request_owner};

/// 送到的留最近这么多条，`session_list` 据此告诉发送方送到了没有。
const KEEP_DELIVERED: usize = 64;
pub(super) struct LivePeer {
    view: PeerMessageView,
    delivery: PeerDelivery,
    delivering_to: Option<u64>,
    /// 写出去之后被 `clear_queue` 清掉，又放回来重发的。用户刚按过停止，不再叫醒模型。
    quiet: bool,
}

/// 主 daemon 交来一条。同一个 id 只收一次：发件队列重投、交接重放都不会排两条。
pub(super) fn deliver(inner: &Inner, delivery: PeerDelivery) {
    {
        let mut state = inner.mu.lock().unwrap();
        if state.peers.iter().any(|peer| peer.view.id == delivery.id) {
            return;
        }
        state.peers.push(LivePeer {
            view: PeerMessageView {
                id: delivery.id.clone(),
                source_session_id: delivery.source_session_id.clone(),
                source_title: delivery.source_title.clone(),
                kind: delivery.kind,
                status: PeerMessageStatus::Pending,
                queued_at_ms: now_ms(),
                delivered_at_ms: None,
            },
            delivery,
            delivering_to: None,
            quiet: false,
        });
        publish_locked(&state, &inner.events);
    }
    poke(inner);
}

/// 扩展的请求交给这个会话绑着的应答函数，回答原样写回。子智能体不能用。
///
/// 应答函数自己认来源会话。这里不从请求体里取来源，也不替它补一个。
/// 应答自己 panic 只让这一次失败，不把套接字线程带走。
pub(super) fn forward(
    inner: &super::Inner,
    mut stream: TaskStream,
    op: &str,
    request: &serde_json::Value,
) {
    let reply = if !request_owner(request).is_empty() {
        serde_json::json!({"ok": false, "error": "子智能体不能和别的对话通信"})
    } else {
        match inner.peer_handler.as_ref() {
            Some(handler) => {
                let mut body = request.clone();
                if let Some(fields) = body.as_object_mut() {
                    fields.remove("op");
                    fields.remove("owner");
                }
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(op, &body)))
                {
                    Ok(reply) => reply,
                    Err(_) => serde_json::json!({"ok": false, "error": "处理这次请求时出了问题"}),
                }
            }
            None => serde_json::json!({"ok": false, "error": "这个对话不能和别的对话通信"}),
        }
    };
    let _ = stream.write_all(reply.to_string().as_bytes());
    let _ = stream.write_all(b"\n");
}

fn notice_of(peer: &LivePeer) -> ModelNotice {
    let delivery = &peer.delivery;
    ModelNotice {
        id: delivery.id.clone(),
        kind: "peer_message",
        title: delivery.source_title.clone(),
        status: delivery.kind.as_str().to_string(),
        exit_code: None,
        output: delivery.text.clone(),
        output_tail: String::new(),
        runs: None,
        peer: Some(serde_json::json!({
            "sourceSessionId": delivery.source_session_id,
            "sourceTitle": delivery.source_title,
            "kind": delivery.kind.as_str(),
            "hops": delivery.hops,
            "replyTo": delivery.reply_to,
        })),
        wake: !peer.quiet,
    }
}

fn deliverable(peer: &LivePeer) -> bool {
    peer.view.status == PeerMessageStatus::Pending
}

pub(super) fn claim(state: &mut State, owner: &str, listener_id: u64) -> Option<ModelNotice> {
    if !owner.is_empty() {
        return None;
    }
    let peer = state
        .peers
        .iter_mut()
        .find(|peer| deliverable(peer) && peer.delivering_to != Some(listener_id))?;
    peer.delivering_to = Some(listener_id);
    Some(notice_of(peer))
}

/// 扩展确认这条已经进了会话记录。改了状态返回 true，调用方据此发快照。
pub(super) fn acknowledge(state: &mut State, owner: &str, id: &str) -> bool {
    if !owner.is_empty() {
        return false;
    }
    let Some(peer) = state
        .peers
        .iter_mut()
        .find(|peer| peer.view.id == id && deliverable(peer))
    else {
        return false;
    };
    peer.view.status = PeerMessageStatus::Delivered;
    peer.view.delivered_at_ms = Some(now_ms());
    peer.delivering_to = None;
    prune_delivered(state);
    true
}

pub(super) fn still_claimed(state: &State, id: &str, listener_id: u64) -> bool {
    state.peers.iter().any(|peer| {
        peer.view.id == id && deliverable(peer) && peer.delivering_to == Some(listener_id)
    })
}

pub(super) fn unclaim(state: &mut State, id: &str, listener_id: u64) {
    if let Some(peer) = state
        .peers
        .iter_mut()
        .find(|peer| peer.view.id == id && peer.delivering_to == Some(listener_id))
    {
        peer.delivering_to = None;
    }
}

pub(super) fn queue_cleared(state: &mut State) {
    for peer in &mut state.peers {
        if deliverable(peer) && peer.delivering_to.is_some() {
            peer.delivering_to = None;
            peer.quiet = true;
        }
    }
}

/// 送到的留最近 `KEEP_DELIVERED` 条，没送到的都留着。
fn prune_delivered(state: &mut State) {
    let delivered = state.peers.iter().filter(|peer| !deliverable(peer)).count();
    let mut excess = delivered.saturating_sub(KEEP_DELIVERED);
    state.peers.retain(|peer| {
        if excess > 0 && !deliverable(peer) {
            excess -= 1;
            return false;
        }
        true
    });
}

pub(super) fn publish_locked(state: &State, events: &smol::channel::Sender<ConversationEvent>) {
    let views = state.peers.iter().map(|peer| peer.view.clone()).collect();
    let _ = events.try_send(ConversationEvent::PeerInbox(views));
}
