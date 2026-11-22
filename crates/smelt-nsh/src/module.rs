//! `smelt/pilot`：给试点面板的宿主模块，JS 里 `nativeModule("smelt/pilot")` 拿 `Smelt`。
//!
//! 模块函数跑在 NSH 的引擎线程，不能碰 GPUI（宿主的东西都在主线程）。本机的对话列表放在一个
//! `watch` 里，主线程在会话变了时换一份，`Smelt.on("conversations")` 在引擎线程上等它变。

use std::{cell::RefCell, rc::Rc, sync::LazyLock};

use gpui::App;
use nsh_core::prelude::*;
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};
use tokio::sync::watch;

/// 本机的一段跟智能体的对话。
#[derive(Clone, Debug, PartialEq, JsObject)]
pub struct Conversation {
    /// 守护会话 id，对话视图的 `session_id`。
    pub id: String,
    /// 它在目录里的会话 id：插件的视图按这个认它。
    pub conversation: String,
    pub title: String,
}

static CONVERSATIONS: LazyLock<watch::Sender<Vec<Conversation>>> =
    LazyLock::new(|| watch::Sender::new(Vec::new()));

/// 主线程这头：现在的对话放进 `CONVERSATIONS`，会话一变再放。App 启动时调一次。
pub(crate) fn serve(cx: &mut App) {
    publish(cx);
    cx.subscribe(&api::events(cx), |_, event, cx| {
        if matches!(event, HostEvent::SessionsChanged) {
            publish(cx);
        }
    })
    .detach();
}

fn publish(cx: &App) {
    let host = api::host(cx);
    let machine = RuntimeKey::Local;
    let states = host.session_states(&machine, cx);
    let conversations = host
        .workspace_directory(&machine, cx)
        .map(|directory| {
            directory
                .conversations
                .iter()
                .filter(|conversation| conversation.archived_at.is_none())
                .filter_map(|conversation| {
                    let acp = conversation.acp.as_ref()?;
                    Some(Conversation {
                        id: acp.sid.clone(),
                        conversation: conversation.id.to_string(),
                        title: api::conversation_name(conversation, &states)
                            .unwrap_or("对话")
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    CONVERSATIONS.send_if_modified(|current| {
        let changed = *current != conversations;
        *current = conversations;
        changed
    });
}

#[cfg(test)]
pub(crate) fn set_for_test(conversations: Vec<Conversation>) {
    CONVERSATIONS.send_replace(conversations);
}

#[module]
pub struct Smelt;

#[module]
impl Smelt {
    /// 新的守护会话 id，给摆新终端的一方。
    pub fn new_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 现在的对话交给 `listener`，之后每变一次再交一次（事件只有 `conversations`），直到调
    /// 返回的函数或调用方没了。
    pub fn on(
        event: String,
        listener: Callback<Vec<Conversation>>,
        cx: CallCx<'_>,
    ) -> Result<Stop, String> {
        if event != "conversations" {
            return Err(format!("没有 Smelt 事件 `{event}`"));
        }
        let listener = Rc::new(RefCell::new(Some(listener)));
        let held = listener.clone();
        cx.start(async move {
            let mut conversations = CONVERSATIONS.subscribe();
            loop {
                let Some(listener) = held.borrow().clone() else {
                    return;
                };
                listener.post(conversations.borrow_and_update().clone());
                if conversations.changed().await.is_err() {
                    return;
                }
            }
        });
        Ok(Stop::new(move || drop(listener.take())))
    }
}

/// 交给 `set_ui_sets` 的那组。
pub(crate) fn registry() -> Registry {
    let mut registry = Registry::ui_set("smelt/pilot");
    registry.module::<Smelt>();
    registry
}
