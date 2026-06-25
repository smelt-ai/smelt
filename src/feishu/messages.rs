//! messages：读取指定会话的最近消息，移植自 lark_tools 的 cmd 58/8 调用链与
//! format_chat_message 解析。只取文本类信号（senderId / timestamp / 正文）。

use super::gateway;
use super::proto::{encode_message, Field, Message, Pb};
use anyhow::Result;

/// 一条解析后的消息（仅文本信号）。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub sender_id: String,
    pub timestamp: u64,
    pub content: String,
}

/// 读取某会话最近 `count` 条消息。
pub async fn fetch(session: &str, chat_id: &str, count: u64) -> Result<Vec<ChatMessage>> {
    let max_pos = match probe_max_position(session, chat_id).await? {
        Some(p) => p,
        None => return Ok(Vec::new()), // 空会话
    };

    let start = max_pos.saturating_sub(count.saturating_sub(1));
    let fetch_count = max_pos - start + 1;

    // cmd 58：位置 → messageId 列表
    let pos_payload = encode_message(&[
        (1, Pb::Str(chat_id.to_string())),
        (2, Pb::Int(start)),
        (3, Pb::Int(fetch_count)),
    ]);
    let buf = gateway::send(session, 58, &pos_payload).await?;
    let (_, pos_msg) = gateway::decode_response(&buf);
    let msg_ids = position_msg_ids(&pos_msg);
    if msg_ids.is_empty() {
        return Ok(Vec::new());
    }

    // cmd 8：messageId 列表 → 消息体（field 1 是 repeated string）
    let id_fields: Vec<(u32, Pb)> = msg_ids.into_iter().map(|id| (1, Pb::Str(id))).collect();
    let buf = gateway::send(session, 8, &encode_message(&id_fields)).await?;
    let (_, msg_payload) = gateway::decode_response(&buf);

    let mut messages = format_chat_messages(&msg_payload);
    messages.sort_by_key(|m| m.timestamp);
    Ok(messages)
}

/// 探测会话最新位置：先用 999999 快速探，失败再二分。
async fn probe_max_position(session: &str, chat_id: &str) -> Result<Option<u64>> {
    let payload = encode_message(&[
        (1, Pb::Str(chat_id.to_string())),
        (2, Pb::Int(999999)),
        (3, Pb::Int(1)),
    ]);
    let buf = gateway::send(session, 58, &payload).await?;
    let (_, p) = gateway::decode_response(&buf);
    if let Some(Field::Msg(pos0)) = p.get_all(1).first() {
        if let Some(n) = pos0.str(1).and_then(|s| s.parse::<u64>().ok()) {
            return Ok(Some(n));
        }
    }
    find_max_position(session, chat_id).await
}

/// 二分查找最大位置（probe 为空时的兜底）。
async fn find_max_position(session: &str, chat_id: &str) -> Result<Option<u64>> {
    let (mut lo, mut hi) = (0u64, 100_000u64);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let payload = encode_message(&[
            (1, Pb::Str(chat_id.to_string())),
            (2, Pb::Int(mid)),
            (3, Pb::Int(1)),
        ]);
        let buf = gateway::send(session, 58, &payload).await?;
        let (_, p) = gateway::decode_response(&buf);
        if p.get_all(1).is_empty() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo.checked_sub(1))
}

/// 从 cmd 58 响应里取出每个 position 的 messageId(f2)。
fn position_msg_ids(payload: &Message) -> Vec<String> {
    payload
        .get_all(1)
        .iter()
        .filter_map(|f| match f {
            Field::Msg(m) => m.str(2).map(|s| s.to_string()),
            _ => None,
        })
        .collect()
}

/// 解析 cmd 8 响应：payload.f1[] → 每个 item.f2(或 item) 是消息体。
fn format_chat_messages(payload: &Message) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for item in payload.get_all(1) {
        let Field::Msg(wrapper) = item else { continue };
        let msg = wrapper.msg(2).unwrap_or(wrapper);
        if let Some(cm) = format_one(msg) {
            out.push(cm);
        }
    }
    out
}

/// 解析单条消息；正文为空则丢弃（对提炼无用的纯图片/系统消息）。
fn format_one(msg: &Message) -> Option<ChatMessage> {
    let timestamp = msg.str(4).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);

    let f5 = msg.msg(5);
    let plain = f5.and_then(|m| m.str(1)).unwrap_or("");
    let rich = f5.and_then(|m| m.str(2)).unwrap_or("");
    let content = strip_html(if !plain.is_empty() { plain } else { rich });
    if content.is_empty() {
        return None;
    }

    let sender_id = match msg.get(3) {
        Some(Field::Str(s)) => s.clone(),
        Some(Field::Msg(m)) => m
            .msg(1)
            .and_then(|f1| f1.str(4))
            .map(|from| from.strip_prefix("FROM_ID:").unwrap_or(from).to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };

    Some(ChatMessage { sender_id, timestamp, content })
}

/// 去掉 HTML 标签（飞书富文本正文带 `<p>` 之类）。
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>你好<b>世界</b></p>"), "你好世界");
        assert_eq!(strip_html("纯文本"), "纯文本");
        assert_eq!(strip_html("  <span>x</span> "), "x");
    }

    #[test]
    fn format_one_extracts_text_and_sender() {
        // 构造 msg { f3:"u123", f4:"1700000000", f5:{f1:"hi"} }
        let bytes = encode_message(&[
            (3, Pb::Str("u123".into())),
            (4, Pb::Str("1700000000".into())),
            (5, Pb::Msg(vec![(1, Pb::Str("hi".into()))])),
        ]);
        let msg = super::super::proto::generic_decode(&bytes);
        let cm = format_one(&msg).expect("应解析出消息");
        assert_eq!(cm.sender_id, "u123");
        assert_eq!(cm.timestamp, 1700000000);
        assert_eq!(cm.content, "hi");
    }

    #[test]
    fn format_one_unwraps_from_id_sender() {
        // f3 是嵌套 {f1:{f4:"FROM_ID:abc"}}
        let sender = Pb::Msg(vec![(1, Pb::Msg(vec![(4, Pb::Str("FROM_ID:abc".into()))]))]);
        let bytes = encode_message(&[
            (3, sender),
            (4, Pb::Str("1".into())),
            (5, Pb::Msg(vec![(1, Pb::Str("x".into()))])),
        ]);
        let msg = super::super::proto::generic_decode(&bytes);
        let cm = format_one(&msg).unwrap();
        assert_eq!(cm.sender_id, "abc");
    }

    #[test]
    fn format_one_drops_empty_content() {
        let bytes = encode_message(&[(4, Pb::Str("1".into()))]);
        let msg = super::super::proto::generic_decode(&bytes);
        assert!(format_one(&msg).is_none());
    }
}
