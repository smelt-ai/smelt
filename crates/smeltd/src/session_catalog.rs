//! smeltd 对外发布的会话身份投影。
//!
//! 活 runtime 覆盖崩溃恢复目录里的同名条目；连接数与 runtime 存活正交。legacy
//! `list`、状态订阅和 Control API 必须共用这一层，不能各自拼一份目录语义。

use super::{AcpSessions, SessionState, Sessions};

/// 已发布身份：活 runtime 按 id 覆盖目录里的断连条目，同 id 只出现一次。
pub(crate) fn published_session_states(
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> Vec<SessionState> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (_, session) in sessions.snapshot() {
        let state = session.state.lock().unwrap().clone();
        seen.insert(state.id.clone());
        out.push(state);
    }
    for (_, slot) in acp_sessions.snapshot() {
        let state = slot.value.state.lock().unwrap().clone();
        seen.insert(state.id.clone());
        out.push(state);
    }
    super::with_session_directory(|directory| {
        for state in directory.snapshot() {
            if seen.insert(state.id.clone()) {
                out.push(state);
            }
        }
    });
    out
}

pub(crate) fn session_connection_counts(
    id: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> (usize, usize) {
    if let Some(session) = sessions.live(id) {
        let out = session.out.lock().unwrap();
        return (out.clients.len(), out.watchers.len());
    }
    if let Some(slot) = acp_sessions.get(id) {
        let out = slot.value.out.lock().unwrap();
        let interactive = usize::from(out.client.is_some());
        return (interactive, out.watchers.len());
    }
    (0, 0)
}
