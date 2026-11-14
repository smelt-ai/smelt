//! 工作台的通知跳转：点了系统通知、菜单栏菜单项，按守护会话 id 找到会话露出来。

use super::*;

impl Workspace {
    /// 跳到某条通知：切到该会话 + 聚焦该 pane。
    pub(crate) fn goto_notification(
        &mut self,
        session_ix: usize,
        pane: Option<&Entity<TerminalView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = pane
            && let Some(session) = self.sessions.get_mut(session_ix)
        {
            // 同一会话有分屏时先换锚点，避免 activate 把旧活动 pane 的通知标已读。
            session.set_active_term(pane.clone());
        }
        self.activate(session_ix, window, cx);
        cx.notify();
    }

    /// 系统通知只保存稳定的守护会话 id；点击时再查当前侧栏位置，避免通知
    /// 出现后用户重排、关闭会话导致旧索引跳错目标。
    pub(crate) fn goto_notification_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = self
            .sessions
            .iter()
            .enumerate()
            .find_map(|(session_ix, session)| match &session.kind {
                SessionKind::Term { .. } => session
                    .term_leaves()
                    .into_iter()
                    .find(|pane| pane.read(cx).session_id() == session_id)
                    .map(|pane| (session_ix, Some(pane))),
                SessionKind::Conversation(view) if view.read(cx).session_id() == session_id => {
                    Some((session_ix, None))
                }
                SessionKind::Conversation(_) => None,
            });
        if let Some((session_ix, pane)) = target {
            self.goto_notification(session_ix, pane.as_ref(), window, cx);
            true
        } else {
            false
        }
    }

    /// 通知携带的是 daemon 稳定 sid。冷启动恢复尚未交货时先挂起；完整恢复结束
    /// 仍找不到才放弃这个已经失效的目标。
    pub(crate) fn request_notification_session_jump(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_notification_session_id = Some(session_id);
        self.consume_pending_notification_session_jump(window, cx);
    }

    pub(crate) fn consume_pending_notification_session_jump(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.pending_notification_session_id.take() else {
            return;
        };
        let target_found = self.goto_notification_session(&session_id, window, cx);
        if target_found {
            // 即使目标恰好已经占着 index 0，通知点击也是一次显式选择；否则恢复收口
            // 可能仍按旧存档活动项把刚跳到的会话覆盖掉。
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
        }
        if should_defer_notification_session_jump(self.sessions_restore_settled(), target_found) {
            self.pending_notification_session_id = Some(session_id);
        }
    }

    /// 返回该通知对应的会话标题，以及它是否正显示在主舞台上。非会话路由、舞台被布局
    /// 里别的视图挡住都看不到活动会话，不能只看 active_session 就判定已在看。
    pub(crate) fn attention_context(
        &self,
        notification: &AttentionItem,
        cx: &App,
    ) -> (bool, Option<String>) {
        let is_current_view =
            self.viewed_session_id(cx).as_deref() == Some(notification.session_id.as_str());
        let mut session_title = None;
        for session in &self.sessions {
            let matches_acp = matches!(
                &session.kind,
                SessionKind::Conversation(view) if view.read(cx).session_id() == notification.session_id
            );
            if matches_acp {
                session_title = Some(Self::title_with_runtime(session, cx));
            }
            for leaf in session.term_leaves() {
                if leaf.read(cx).session_id() == notification.session_id {
                    session_title = Some(Self::title_with_runtime(session, cx));
                }
            }
        }
        (is_current_view, session_title)
    }
}
