//! 工作台当前显示哪一个一级入口。
//!
//! 左侧「智能体 / 自动化」和插件登记的页面是分组标题，不是 iOS/长桥那种保活 Tab：点标题
//! 总是进该面的根页。对话和运行现场是侧栏里的子行，只切当前面、不清根页。

/// 左侧一级导航。智能体、自动化、插件页面与会话并列。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkspaceRoute {
    Agents,
    Automations,
    /// 插件登记的页面（[`smelt_plugin_api::PageSpec`]），按 id 记。
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
            Self::Agents => "agents",
            Self::Automations => "automations",
            Self::Page(id) => id,
            Self::Desktop => "desktop",
            Self::Session => "session",
        }
    }

    fn from_wire_name(name: String) -> Self {
        match name.as_str() {
            "agents" => Self::Agents,
            "automations" => Self::Automations,
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

/// 智能体面内页。定义列表是根；详情（设置）和对话是钻取，不落盘。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentsView {
    #[default]
    Catalog,
    Editor {
        agent: crate::agents::AgentRef,
    },
    Conversation {
        sid: String,
    },
}

impl AgentsView {
    pub(crate) fn conversation_sid(&self) -> Option<&str> {
        match self {
            Self::Conversation { sid } => Some(sid.as_str()),
            Self::Catalog | Self::Editor { .. } => None,
        }
    }

    pub(crate) fn editor_agent(&self) -> Option<&crate::agents::AgentRef> {
        match self {
            Self::Editor { agent } => Some(agent),
            Self::Catalog | Self::Conversation { .. } => None,
        }
    }

    pub(crate) fn open_editor(&mut self, agent: crate::agents::AgentRef) {
        *self = Self::Editor { agent };
    }

    pub(crate) fn open_conversation(&mut self, sid: String) {
        *self = Self::Conversation { sid };
    }

    pub(crate) fn pop_to_root(&mut self) {
        *self = Self::Catalog;
    }
}

/// 自动化面内页。列表是根，编辑器是唯一一层钻取，不落盘。
///
/// 运行记录不另开一页：它留在编辑器里。选中哪一次运行是编辑器上的选中态，
/// 不是再往下压一层。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutomationsView {
    #[default]
    List,
    Editor {
        automation: crate::agents::AutomationRef,
    },
}

impl AutomationsView {
    /// 编辑器开着的那条自动化。
    pub(crate) fn editor(&self) -> Option<&crate::agents::AutomationRef> {
        match self {
            Self::Editor { automation } => Some(automation),
            Self::List => None,
        }
    }

    pub(crate) fn open_editor(&mut self, automation: crate::agents::AutomationRef) {
        *self = Self::Editor { automation };
    }

    pub(crate) fn pop_to_root(&mut self) {
        *self = Self::List;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceNav {
    active: WorkspaceRoute,
    agents: AgentsView,
    automations: AutomationsView,
}

impl Default for WorkspaceNav {
    fn default() -> Self {
        Self {
            active: WorkspaceRoute::Session,
            agents: AgentsView::Catalog,
            automations: AutomationsView::List,
        }
    }
}

impl WorkspaceNav {
    pub(crate) fn from_persisted(route: WorkspaceRoute) -> Self {
        Self {
            active: route,
            agents: AgentsView::Catalog,
            automations: AutomationsView::List,
        }
    }

    pub(crate) fn active(&self) -> &WorkspaceRoute {
        &self.active
    }

    pub(crate) fn agents(&self) -> &AgentsView {
        &self.agents
    }

    pub(crate) fn agents_mut(&mut self) -> &mut AgentsView {
        &mut self.agents
    }

    pub(crate) fn automations(&self) -> &AutomationsView {
        &self.automations
    }

    pub(crate) fn automations_mut(&mut self) -> &mut AutomationsView {
        &mut self.automations
    }

    pub(crate) fn set_active(&mut self, tab: WorkspaceRoute) {
        self.active = tab;
    }

    pub(crate) fn pop_active_to_root(&mut self) {
        match self.active {
            WorkspaceRoute::Agents => self.agents.pop_to_root(),
            WorkspaceRoute::Automations => self.automations.pop_to_root(),
            WorkspaceRoute::Page(_) | WorkspaceRoute::Desktop | WorkspaceRoute::Session => {}
        }
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
    fn persisted_route_is_restored_as_is() {
        let nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents);
        assert_eq!(nav.active(), &WorkspaceRoute::Agents);
        assert_eq!(nav.persist(), WorkspaceRoute::Agents);
    }

    #[test]
    fn set_active_only_changes_the_current_tab() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents);
        nav.set_active(WorkspaceRoute::Automations);
        assert_eq!(nav.active(), &WorkspaceRoute::Automations);
        nav.set_active(WorkspaceRoute::Agents);
        assert_eq!(nav.active(), &WorkspaceRoute::Agents);
    }

    #[test]
    fn agents_catalog_editor_and_conversation_are_exclusive() {
        let agent =
            crate::agents::AgentRef::new(crate::RuntimeKey::Remote("far".into()), "agent-1".into());
        let mut view = AgentsView::Catalog;
        view.open_editor(agent.clone());
        assert_eq!(view.editor_agent(), Some(&agent));
        assert!(view.conversation_sid().is_none());

        view.open_conversation("sid-1".into());
        assert_eq!(view.conversation_sid(), Some("sid-1"));
        assert!(view.editor_agent().is_none());

        view.pop_to_root();
        assert_eq!(view, AgentsView::Catalog);
        assert!(view.conversation_sid().is_none());
        assert!(view.editor_agent().is_none());
    }

    #[test]
    fn automations_editor_is_one_level_above_the_list() {
        let auto = crate::agents::AutomationRef::new(crate::RuntimeKey::Local, "auto-1");
        let mut view = AutomationsView::List;
        view.open_editor(auto.clone());
        assert_eq!(view.editor(), Some(&auto));

        view.open_editor(auto.clone());
        assert_eq!(
            view,
            AutomationsView::Editor { automation: auto }
        );

        view.pop_to_root();
        assert_eq!(view, AutomationsView::List);
        assert!(view.editor().is_none());
    }

    #[test]
    fn sidebar_pop_to_root_clears_drill_in_on_the_active_face_only() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents);
        nav.agents_mut().open_editor(crate::agents::AgentRef::new(
            crate::RuntimeKey::Local,
            "agent-1".into(),
        ));
        let auto =
            crate::agents::AutomationRef::new(crate::RuntimeKey::Remote("box".into()), "auto-1");
        nav.automations_mut().open_editor(auto.clone());

        nav.pop_active_to_root();
        assert_eq!(nav.agents(), &AgentsView::Catalog);
        assert_eq!(nav.automations().editor(), Some(&auto));

        nav.set_active(WorkspaceRoute::Automations);
        nav.pop_active_to_root();
        assert_eq!(nav.automations(), &AutomationsView::List);
        assert_eq!(nav.agents(), &AgentsView::Catalog);
    }

    #[test]
    fn desktop_route_is_not_restored_after_restart() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents);
        nav.set_active(WorkspaceRoute::Desktop);
        assert_eq!(nav.active(), &WorkspaceRoute::Desktop);
        assert_eq!(nav.persist(), WorkspaceRoute::Session);
    }

    #[test]
    fn page_route_is_restored_as_is() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Session);
        nav.set_active(WorkspaceRoute::Page("capabilities".into()));
        nav.pop_active_to_root();
        assert_eq!(nav.persist(), WorkspaceRoute::Page("capabilities".into()));
    }

    #[test]
    fn known_system_routes_keep_wire_names() {
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Agents).unwrap(),
            "\"agents\""
        );
        assert_eq!(
            serde_json::to_string(&WorkspaceRoute::Automations).unwrap(),
            "\"automations\""
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
        // 不认识的名字是插件页面的 id；插件已经不在时由恢复的地方退回会话。
        assert_eq!(
            serde_json::from_str::<WorkspaceRoute>("\"retired-plugin\"").unwrap(),
            WorkspaceRoute::Page("retired-plugin".into())
        );
    }
}
