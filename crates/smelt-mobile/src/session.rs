//! 会话管理
//!
//! 管理 ACP 会话状态、消息流、审批交互。

use anyhow::Result;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::api::{AcpEntry, ApprovalKind, ApprovalMenu, ApprovalOption, SessionSummary};

/// 全局会话存储
static SESSIONS: once_cell::sync::Lazy<RwLock<HashMap<String, SessionState>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(HashMap::new()));

/// 当前订阅的会话 ID
static SUBSCRIBED_SESSION: once_cell::sync::Lazy<RwLock<Option<String>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

/// 会话状态
#[derive(Default)]
struct SessionState {
    summary: Option<SessionSummary>,
    entries: Vec<AcpEntry>,
    pending_approval: Option<ApprovalMenu>,
}

/// 列出所有会话
pub async fn list_sessions() -> Result<Vec<SessionSummary>> {
    // 请求最新列表
    let request = serde_json::json!({
        "method": "listSessions",
    });
    crate::transport::send(&request.to_string()).await?;

    // 返回当前缓存
    let sessions = SESSIONS.read().unwrap();
    Ok(sessions
        .values()
        .filter_map(|s| s.summary.clone())
        .collect())
}

/// 订阅会话消息流（返回当前条目）
pub async fn subscribe(session_id: &str) -> Result<Vec<AcpEntry>> {
    // 设置当前订阅
    {
        let mut sub = SUBSCRIBED_SESSION.write().unwrap();
        *sub = Some(session_id.to_string());
    }

    // 发送订阅请求
    let request = serde_json::json!({
        "method": "subscribe",
        "params": {
            "sessionId": session_id,
        }
    });
    crate::transport::send(&request.to_string()).await?;

    // 返回当前缓存的条目
    let sessions = SESSIONS.read().unwrap();
    Ok(sessions
        .get(session_id)
        .map(|s| s.entries.clone())
        .unwrap_or_default())
}

/// 轮询新条目
pub async fn poll_entries(session_id: &str, since_index: usize) -> Result<Vec<AcpEntry>> {
    let sessions = SESSIONS.read().unwrap();
    Ok(sessions
        .get(session_id)
        .map(|s| {
            if since_index < s.entries.len() {
                s.entries[since_index..].to_vec()
            } else {
                vec![]
            }
        })
        .unwrap_or_default())
}

/// 取消订阅
pub fn unsubscribe() {
    let mut sub = SUBSCRIBED_SESSION.write().unwrap();
    *sub = None;

    // 发送取消订阅请求
    tokio::spawn(async move {
        let request = serde_json::json!({
            "method": "unsubscribe",
        });
        let _ = crate::transport::send(&request.to_string()).await;
    });
}

/// 发送用户消息
pub async fn send_message(session_id: &str, content: &str) -> Result<()> {
    let request = serde_json::json!({
        "method": "sendMessage",
        "params": {
            "sessionId": session_id,
            "content": content,
        }
    });
    crate::transport::send(&request.to_string()).await
}

/// 响应审批请求
pub async fn respond_approval(
    session_id: &str,
    option_key: &str,
    custom_text: Option<&str>,
) -> Result<()> {
    let request = serde_json::json!({
        "method": "respondApproval",
        "params": {
            "sessionId": session_id,
            "optionKey": option_key,
            "customText": custom_text,
        }
    });
    crate::transport::send(&request.to_string()).await?;

    // 清除待处理审批
    clear_pending_approval(session_id);
    Ok(())
}

/// 获取当前审批菜单
pub async fn get_approval_menu(session_id: &str) -> Result<Option<ApprovalMenu>> {
    Ok(get_pending_approval(session_id))
}

/// 标记会话已读
pub async fn mark_read(session_id: &str) -> Result<()> {
    let request = serde_json::json!({
        "method": "markRead",
        "params": {
            "sessionId": session_id,
        }
    });
    crate::transport::send(&request.to_string()).await
}

/// 中断会话
pub async fn interrupt(session_id: &str) -> Result<()> {
    let request = serde_json::json!({
        "method": "interrupt",
        "params": {
            "sessionId": session_id,
        }
    });
    crate::transport::send(&request.to_string()).await
}

/// 处理从 WebSocket 收到的消息
pub fn handle_message(message: &str) {
    let Ok(msg): Result<serde_json::Value, _> = serde_json::from_str(message) else {
        log::warn!("Invalid JSON message: {}", message);
        return;
    };

    let Some(msg_type) = msg.get("type").and_then(|t| t.as_str()) else {
        log::warn!("Message missing type: {}", message);
        return;
    };

    match msg_type {
        "sessions" => handle_sessions_list(&msg),
        "entry" => handle_entry(&msg),
        "approval" => handle_approval(&msg),
        "sessionUpdate" => handle_session_update(&msg),
        _ => {
            log::debug!("Unknown message type: {}", msg_type);
        }
    }
}

/// 处理会话列表
fn handle_sessions_list(msg: &serde_json::Value) {
    let Some(sessions) = msg.get("sessions").and_then(|s| s.as_array()) else {
        return;
    };

    let mut store = SESSIONS.write().unwrap();
    for session in sessions {
        if let Ok(summary) = serde_json::from_value::<SessionSummary>(session.clone()) {
            let id = summary.id.clone();
            store.entry(id).or_default().summary = Some(summary);
        }
    }
}

/// 处理 ACP 条目
fn handle_entry(msg: &serde_json::Value) {
    let Some(entry) = msg.get("entry") else {
        return;
    };

    let Ok(entry) = serde_json::from_value::<AcpEntry>(entry.clone()) else {
        log::warn!("Failed to parse entry: {:?}", entry);
        return;
    };

    let session_id = msg
        .get("sessionId")
        .and_then(|s| s.as_str())
        .unwrap_or_default();

    // 存储条目
    {
        let mut store = SESSIONS.write().unwrap();
        let state = store.entry(session_id.to_string()).or_default();
        state.entries.push(entry.clone());
    }

    // 检查是否是当前订阅的会话（用于日志）
    let subscribed = {
        let sub = SUBSCRIBED_SESSION.read().unwrap();
        sub.as_ref().map(|s| s == session_id).unwrap_or(false)
    };

    if subscribed {
        log::debug!("New entry for subscribed session: {:?}", entry);
    }
}

/// 处理审批请求
fn handle_approval(msg: &serde_json::Value) {
    let session_id = msg
        .get("sessionId")
        .and_then(|s| s.as_str())
        .unwrap_or_default();

    let Some(approval) = msg.get("approval") else {
        return;
    };

    // 解析审批菜单
    let key = approval
        .get("key")
        .and_then(|k| k.as_str())
        .unwrap_or_default()
        .to_string();

    let prompt = approval
        .get("prompt")
        .or_else(|| approval.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("Permission Required")
        .to_string();

    let allows_text_input = approval
        .get("allowsTextInput")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    let options = approval
        .get("options")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    let kind_str = opt.get("kind").and_then(|k| k.as_str()).unwrap_or("custom");
                    let kind = match kind_str {
                        "approve" | "allow" => ApprovalKind::Approve,
                        "deny" | "reject" => ApprovalKind::Deny,
                        _ => ApprovalKind::Custom,
                    };
                    Some(ApprovalOption {
                        key: opt.get("key")?.as_str()?.to_string(),
                        label: opt.get("label")?.as_str()?.to_string(),
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let menu = ApprovalMenu {
        key,
        prompt,
        options,
        allows_text_input,
    };

    // 存储待处理审批
    {
        let mut store = SESSIONS.write().unwrap();
        let state = store.entry(session_id.to_string()).or_default();
        state.pending_approval = Some(menu.clone());
    }

    log::info!("Approval requested for session {}: {:?}", session_id, menu);
}

/// 处理会话更新
fn handle_session_update(msg: &serde_json::Value) {
    let Some(session) = msg.get("session") else {
        return;
    };

    if let Ok(summary) = serde_json::from_value::<SessionSummary>(session.clone()) {
        let mut store = SESSIONS.write().unwrap();
        let id = summary.id.clone();
        store.entry(id).or_default().summary = Some(summary);
    }
}

/// 处理二进制消息（终端流等，ACP 可能不需要）
pub fn handle_binary(data: &[u8]) {
    // ACP 模式下可能不需要二进制流
    log::debug!("Received binary data: {} bytes", data.len());
}

/// 获取会话的待处理审批
pub fn get_pending_approval(session_id: &str) -> Option<ApprovalMenu> {
    let store = SESSIONS.read().unwrap();
    store.get(session_id)?.pending_approval.clone()
}

/// 清除会话的待处理审批
pub fn clear_pending_approval(session_id: &str) {
    let mut store = SESSIONS.write().unwrap();
    if let Some(state) = store.get_mut(session_id) {
        state.pending_approval = None;
    }
}
