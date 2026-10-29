//! 终端分屏当前那段 provider 对话的名字。
//!
//! 名字只存在 `history_title`（改名写那里）。分屏换了对话、或名字被改了，守护查一次放进
//! 会话状态（[`SessionState::custom_title`]），随状态推送给客户端；客户端不另存、不轮询。

use std::collections::HashMap;

use smelt_core::agent_kind::{HistorySourceKind, TerminalAgentKind};

use super::session_state::{SessionState, bump_state_revision};
use super::{EventHubHandle, Sessions, broadcast_state};

/// (agent id, profile id, provider 对话 id) → 名字，同 `session_metadata::all_custom_titles`。
type Titles = HashMap<(String, Option<String>, String), String>;

/// 这一格现在对的是哪段 provider 对话：(历史身份, 对话 id)。终端没有 profile。
fn identity(state: &SessionState) -> Option<(HistorySourceKind, String)> {
    let conversation_id = state
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let terminal = TerminalAgentKind::from_id(state.provider.as_deref()?)?;
    Some((terminal.into(), conversation_id.to_string()))
}

/// 这一格现在那段对话的名字。
pub(crate) fn lookup(state: &SessionState) -> Option<String> {
    let (kind, conversation_id) = identity(state)?;
    smelt_core::session_metadata::custom_title(kind, None, &conversation_id)
}

/// 这一格换了对话：查它现在的名字，变了就推。
pub(crate) fn refresh(sessions: &Sessions, id: &str, event_hub: &EventHubHandle) {
    apply(sessions, id, event_hub, |kind, conversation_id| {
        smelt_core::session_metadata::custom_title(kind, None, conversation_id)
    });
}

/// 名字被改了（改名、删历史）：在跑的终端各对一遍，读一次库。
pub(crate) fn refresh_all(sessions: &Sessions, event_hub: &EventHubHandle) {
    let titles: Titles = smelt_core::session_metadata::all_custom_titles();
    for (id, _) in sessions.snapshot() {
        apply(sessions, &id, event_hub, |kind, conversation_id| {
            titles
                .get(&(kind.id().to_string(), None, conversation_id.to_string()))
                .cloned()
        });
    }
}

fn apply(
    sessions: &Sessions,
    id: &str,
    event_hub: &EventHubHandle,
    title_of: impl Fn(HistorySourceKind, &str) -> Option<String>,
) {
    let Some(before) = sessions.with_live(id, |sess| identity(&sess.state.lock().unwrap())) else {
        return;
    };
    // 查库不占着状态锁；查完这一格又换了对话就不写，以后那次为准。
    let title = before
        .as_ref()
        .and_then(|(kind, conversation_id)| title_of(*kind, conversation_id));
    let snapshot = sessions
        .with_live(id, |sess| {
            let mut state = sess.state.lock().unwrap();
            if identity(&state) != before || state.custom_title == title {
                return None;
            }
            state.custom_title = title;
            bump_state_revision(&mut state);
            Some(state.clone())
        })
        .flatten();
    if let Some(snapshot) = snapshot {
        broadcast_state(event_hub, &snapshot);
    }
}
