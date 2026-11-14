//! 工作台当前显示哪一个一级入口。
//!
//! 侧栏的插件视图入口是分组标题，点了总是进那一页。对话是侧栏「对话」组里的子行，
//! 选中哪一段记在导航上，不落盘：冷启动按恢复出来的当前会话补位。

/// 左侧一级导航。对话、插件视图、远程桌面与项目会话并列。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkspaceRoute {
    /// 侧栏「对话」组里的一段对话。看哪一段记在 [`WorkspaceNav`] 上。
    Conversation,
    /// 插件视图（[`smelt_plugin_api::ViewSpec`]）当舞台上的一页，按 id 记。
    Page(String),
    /// 运行时那台机器的远程桌面。看哪台运行时记在 Workspace 上，不落盘。
    Desktop,
    #[default]
    Session,
}

impl WorkspaceRoute {
    pub(crate) fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }

    /// 落盘的名字。插件页面就是它的 id；宿主自己的几个名字插件不能占用。
    fn wire_name(&self) -> &str {
        match self {
            Self::Conversation => "conversation",
            Self::Page(id) => id,
            Self::Desktop => "desktop",
            Self::Session => "session",
        }
    }

    fn from_wire_name(name: String) -> Self {
        match name.as_str() {
            "conversation" => Self::Conversation,
            "desktop" => Self::Desktop,
            "session" => Self::Session,
            _ => Self::Page(name),
        }
    }
}

impl serde::Serialize for WorkspaceRoute {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

impl<'de> serde::Deserialize<'de> for WorkspaceRoute {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from_wire_name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceNav {
    active: WorkspaceRoute,
    /// 路由在 [`WorkspaceRoute::Conversation`] 时看的那段对话。冷启动时是 `None`，
    /// 等会话恢复完按当前会话补上。
    conversation_sid: Option<String>,
}

impl WorkspaceNav {
    pub(crate) fn from_persisted(route: WorkspaceRoute) -> Self {
        Self {
            active: route,
            conversation_sid: None,
        }
    }

    pub(crate) fn active(&self) -> &WorkspaceRoute {
        &self.active
    }

    /// 正在看的对话；路由不在对话上时是 `None`。
    pub(crate) fn conversation_sid(&self) -> Option<&str> {
        match self.active {
            WorkspaceRoute::Conversation => self.conversation_sid.as_deref(),
            WorkspaceRoute::Page(_) | WorkspaceRoute::Desktop | WorkspaceRoute::Session => None,
        }
    }

    /// 换到别的入口。看的那段对话跟着丢掉，回来时由点的那一行重新给。
    pub(crate) fn set_active(&mut self, tab: WorkspaceRoute) {
        if tab != WorkspaceRoute::Conversation {
            self.conversation_sid = None;
        }
        self.active = tab;
    }

    pub(crate) fn open_conversation(&mut self, sid: String) {
        self.active = WorkspaceRoute::Conversation;
        self.conversation_sid = Some(sid);
    }

    pub(crate) fn persist(&self) -> WorkspaceRoute {
        match &self.active {
            // 远程桌面是现场，重启后不自动连回去。
            WorkspaceRoute::Desktop => WorkspaceRoute::Session,
            route => route.clone(),
        }
    }
}

impl crate::Workspace {
    pub(crate) fn active_tab(&self) -> &WorkspaceRoute {
        self.nav.active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_route_is_restored_without_a_conversation() {
        let nav = WorkspaceNav::from_persisted(WorkspaceRoute::Conversation);
        assert_eq!(nav.active(), &WorkspaceRoute::Conversation);
        assert_eq!(nav.conversation_sid(), None, "看哪段对话不落盘，恢复完再补");
        assert_eq!(nav.persist(), WorkspaceRoute::Conversation);
    }

    #[test]
    fn leaving_a_conversation_forgets_which_one() {
        let mut nav = WorkspaceNav::default();
        nav.open_conversation("sid-1".into());
        assert_eq!(nav.active(), &WorkspaceRoute::Conversation);
        assert_eq!(nav.conversation_sid(), Some("sid-1"));

        nav.set_active(WorkspaceRoute::Page("agents".into()));
        assert_eq!(nav.conversation_sid(), None);
        nav.set_active(WorkspaceRoute::Conversation);
        assert_eq!(nav.conversation_sid(), None);
    }

    #[test]
    fn desktop_route_is_not_restored_after_restart() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Conversation);
        nav.set_active(WorkspaceRoute::Desktop);
        assert_eq!(nav.active(), &WorkspaceRoute::Desktop);
        assert_eq!(nav.persist(), WorkspaceRoute::Session);
    }

    #[test]
    fn page_route_is_restored_as_is() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Session);
        nav.set_active(WorkspaceRoute::Page("capabilities".into()));
        assert_eq!(nav.persist(), WorkspaceRoute::Page("capabilities".into()));
    }

    #[test]
    fn known_system_routes_keep_wire_names() {
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Conversation).unwrap(),
            "\"conversation\""
        );
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Page("capabilities".into())).unwrap(),
            "\"capabilities\""
        );
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Session).unwrap(),
            "\"session\""
        );
        assert_eq!(
            serde_json::from_str::<WorkspaceRoute>("\"session\"").unwrap(),
            WorkspaceRoute::Session
        );
        // 智能体、自动化成了插件视图：旧存档里的名字正好是它们的视图 id。
        assert_eq!(
            serde_json::from_str::<WorkspaceRoute>("\"agents\"").unwrap(),
            WorkspaceRoute::Page("agents".into())
        );
        // 不认识的名字是插件页面的 id；插件已经不在时由恢复的地方退回会话。
        assert_eq!(
            serde_json::from_str::<WorkspaceRoute>("\"retired-plugin\"").unwrap(),
            WorkspaceRoute::Page("retired-plugin".into())
        );
    }
}
