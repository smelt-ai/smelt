//! 智能体主动发的通知：Pi 的 `notify_user` 工具，以及 `smelt notify` 命令（`notify.send`）。
//!
//! 会话状态产生的提醒（等批准、完成……）由运行时判定；这里是智能体自己判断「这件事值得打扰
//! 用户」时发的一条消息，内容由它写。两头都发：
//! - 提醒（`crate::attention`）：记成这段会话的未读、响一下；命令行发的只响。旧桌面还读
//!   `terminal.notification`，也照发一份；
//! - 手机：交给 [`crate::push::send_message`]，推给登记了「智能体消息」的手机，人在不在 Mac 前
//!   都推。
//!
//! 不受自动化的通知开关管：那个开关只管每次跑完的例行提醒。常见用法是自动化关掉例行提醒，
//! 提示词里写「发现异常时调用 notify_user」。

use super::*;

/// 消息正文最多这么多字。推送里还会按 `smelt_push::MESSAGE_MAX_CHARS` 再截一次。
const MESSAGE_LIMIT_CHARS: usize = 2_000;
const TITLE_LIMIT_CHARS: usize = 80;
/// 智能体没给标题、也不是自动化运行时，桌面通知的标题。
const DEFAULT_TITLE: &str = "智能体消息";

/// 发出去的情况，回给调用方。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    /// 有桌面客户端连着、收到了。
    pub(crate) desktop: bool,
    /// 推给了几台手机。
    pub(crate) phones: usize,
}

/// 智能体给的标题和正文，去掉首尾空白、校验长度。
fn parse(title: Option<&str>, message: Option<&str>) -> Result<(Option<String>, String), String> {
    let message = message.map(str::trim).unwrap_or_default();
    if message.is_empty() {
        return Err("通知内容是空的".to_string());
    }
    if message.chars().count() > MESSAGE_LIMIT_CHARS {
        return Err(format!("通知内容太长，最多 {MESSAGE_LIMIT_CHARS} 个字符"));
    }
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    if title.is_some_and(|title| title.chars().count() > TITLE_LIMIT_CHARS) {
        return Err(format!("标题太长，最多 {TITLE_LIMIT_CHARS} 个字符"));
    }
    Ok((title.map(str::to_string), message.to_string()))
}

/// 桌面、手机上显示的文字。
#[derive(Debug, PartialEq, Eq)]
struct Display {
    /// 桌面通知的标题。侧栏里的会话，桌面会在后面接上会话名。
    desktop_title: String,
    /// 推送标题：智能体给的，其次自动化名字，再次会话名。
    push_title: Option<String>,
    /// 推送副标题：项目名。
    push_subtitle: Option<String>,
}

fn display(
    title: Option<&str>,
    automation: Option<&str>,
    session: Option<&str>,
    project: Option<&str>,
) -> Display {
    let named = title.or(automation);
    Display {
        desktop_title: named.unwrap_or(DEFAULT_TITLE).to_string(),
        push_title: named.or(session).map(str::to_string),
        push_subtitle: project.map(str::to_string),
    }
}

/// `session_id` 给桌面认会话；`push_session` 给手机分组、点开跳转，空串表示不跳。
fn deliver(
    event_hub: &EventHubHandle,
    session_id: &str,
    push_session: &str,
    display: &Display,
    message: &str,
) -> Sent {
    let desktop = match event_hub.publish_terminal_notification(
        session_id,
        Some(&display.desktop_title),
        message,
    ) {
        Ok(delivered) => delivered,
        Err(error) => {
            crate::dlog(&format!("智能体通知：发给桌面失败：{error}"));
            false
        }
    };
    event_hub.notify_attention(
        session_id,
        smelt_event_protocol::AttentionKind::Notice,
        &display.desktop_title,
        message,
    );
    let phones = crate::push::send_message(
        push_session,
        display.push_title.as_deref(),
        display.push_subtitle.as_deref(),
        message,
    );
    Sent { desktop, phones }
}

/// 会话归自动化运行时，那次运行的 id。
fn automation_run_id(sess: &AcpSession) -> Option<String> {
    match &*sess.conversation_binding.lock() {
        Some(smelt_core::conversation::ConversationBinding::Automation { run_id }) => {
            Some(run_id.clone())
        }
        _ => None,
    }
}

fn automation_name(run_id: &str) -> Option<String> {
    smelt_core::automation_store::load_run(run_id)
        .map(|run| run.context.automation_name)
        .filter(|name| !name.trim().is_empty())
}

/// Pi 会话的 `notify` 请求。来源是起会话时绑上的会话 id，请求体里的字段不算。
pub(crate) fn from_session(
    session_id: &str,
    sess: &AcpSession,
    event_hub: &EventHubHandle,
    body: &serde_json::Value,
) -> serde_json::Value {
    let field = |name: &str| body.get(name).and_then(serde_json::Value::as_str);
    let (title, message) = match parse(field("title"), field("message")) {
        Ok(parsed) => parsed,
        Err(error) => return serde_json::json!({"ok": false, "reason": "invalid", "error": error}),
    };
    let run_id = automation_run_id(sess);
    let automation = run_id.as_deref().and_then(automation_name);
    let session = crate::peer_messages::title_of(session_id, sess);
    let project = sess
        .cwd
        .as_deref()
        .and_then(|cwd| std::path::Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let display = display(
        title.as_deref(),
        automation.as_deref(),
        Some(session.as_str()).filter(|session| !session.is_empty()),
        project.as_deref(),
    );
    // 自动化运行的会话不在侧栏、手机的会话列表里，点开推送也找不到：推送不带会话。
    let push_session = if run_id.is_some() { "" } else { session_id };
    let sent = deliver(event_hub, session_id, push_session, &display, &message);
    crate::dlog(&format!(
        "智能体通知：会话 {} 发了一条，桌面 {}，手机 {} 台",
        session_id.get(..8).unwrap_or(session_id),
        sent.desktop,
        sent.phones
    ));
    serde_json::json!({"ok": true, "desktop": sent.desktop, "phones": sent.phones})
}

/// `notify.send`：不属于任何会话（命令行、脚本发的）。
pub(crate) fn from_control(
    event_hub: &EventHubHandle,
    params: smelt_core::control_api::NotifySendParams,
) -> Result<smelt_core::control_api::NotifySendResult, String> {
    let (title, message) = parse(params.title.as_deref(), Some(&params.message))?;
    let display = display(title.as_deref(), None, None, None);
    let sent = deliver(event_hub, "", "", &display, &message);
    crate::dlog(&format!(
        "智能体通知：命令行发了一条，桌面 {}，手机 {} 台",
        sent.desktop, sent.phones
    ));
    Ok(smelt_core::control_api::NotifySendResult {
        desktop: sent.desktop,
        phones: sent.phones,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_required_and_trimmed() {
        assert!(parse(None, None).is_err());
        assert!(parse(None, Some("  \n ")).is_err());
        assert_eq!(
            parse(Some("  "), Some(" 磁盘告警 \n")).unwrap(),
            (None, "磁盘告警".to_string()),
            "空白标题当没给"
        );
        assert_eq!(
            parse(Some(" 巡检 "), Some("ok")).unwrap(),
            (Some("巡检".to_string()), "ok".to_string())
        );
    }

    #[test]
    fn overlong_text_is_rejected() {
        assert!(parse(None, Some(&"字".repeat(MESSAGE_LIMIT_CHARS))).is_ok());
        assert!(parse(None, Some(&"字".repeat(MESSAGE_LIMIT_CHARS + 1))).is_err());
        assert!(parse(Some(&"字".repeat(TITLE_LIMIT_CHARS + 1)), Some("ok")).is_err());
    }

    /// 标题依次取智能体给的、自动化名字；桌面会自己接上会话名，推送没有就用会话名。
    #[test]
    fn titles_fall_back_in_order() {
        assert_eq!(
            display(Some("巡检"), Some("日报"), Some("修推送"), Some("smelt")),
            Display {
                desktop_title: "巡检".into(),
                push_title: Some("巡检".into()),
                push_subtitle: Some("smelt".into()),
            }
        );
        assert_eq!(
            display(None, Some("日报"), Some("修推送"), None),
            Display {
                desktop_title: "日报".into(),
                push_title: Some("日报".into()),
                push_subtitle: None,
            }
        );
        assert_eq!(
            display(None, None, Some("修推送"), None),
            Display {
                desktop_title: DEFAULT_TITLE.into(),
                push_title: Some("修推送".into()),
                push_subtitle: None,
            }
        );
        assert_eq!(display(None, None, None, None).push_title, None);
    }
}
