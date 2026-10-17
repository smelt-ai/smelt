//! 工作台当前显示哪一个一级入口。
//!
//! 左侧「智能体 / 自动化 / 能力库」是分组标题，不是 iOS/长桥那种保活 Tab：点标题总是进
//! 该面的根页。能力库的模型、插件、MCP、记忆是面内平级页签，不是钻取。对话和运行现场
//! 是侧栏里的子行，只切当前面、不清根页。

/// 左侧一级导航。智能体、自动化、能力库与会话并列。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceRoute {
    Agents,
    Automations,
    /// 能力库：智能体共用的模型、插件和记忆。
    Capabilities,
    #[default]
    Session,
}

impl WorkspaceRoute {
    pub(crate) fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }
}

/// 智能体面内页。定义列表是根；详情（设置）和对话是钻取，不落盘。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentsView {
    #[default]
    Catalog,
    Editor {
        agent_id: String,
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

    pub(crate) fn editor_id(&self) -> Option<&str> {
        match self {
            Self::Editor { agent_id } => Some(agent_id.as_str()),
            Self::Catalog | Self::Conversation { .. } => None,
        }
    }

    pub(crate) fn open_editor(&mut self, agent_id: String) {
        *self = Self::Editor { agent_id };
    }

    pub(crate) fn open_conversation(&mut self, sid: String) {
        *self = Self::Conversation { sid };
    }

    pub(crate) fn pop_to_root(&mut self) {
        *self = Self::Catalog;
    }
}

/// 能力库面内页签。各页平级，切换不算钻取：侧栏回根页时保持当前页签，不落盘。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CapabilitiesTab {
    #[default]
    Models,
    Plugins,
    Mcp,
    Memory,
}

impl CapabilitiesTab {
    pub(crate) const ALL: [Self; 4] = [Self::Models, Self::Plugins, Self::Mcp, Self::Memory];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Models => "模型",
            Self::Plugins => "插件",
            Self::Mcp => "MCP",
            Self::Memory => "记忆",
        }
    }

    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::Models => "capabilities-tab-models",
            Self::Plugins => "capabilities-tab-plugins",
            Self::Mcp => "capabilities-tab-mcp",
            Self::Memory => "capabilities-tab-memory",
        }
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
        automation_id: String,
    },
}

impl AutomationsView {
    pub(crate) fn automation_id(&self) -> Option<&str> {
        match self {
            Self::List => None,
            Self::Editor { automation_id } => Some(automation_id.as_str()),
        }
    }

    pub(crate) fn editor_id(&self) -> Option<&str> {
        match self {
            Self::Editor { automation_id } => Some(automation_id.as_str()),
            Self::List => None,
        }
    }

    pub(crate) fn open_editor(&mut self, automation_id: String) {
        *self = Self::Editor { automation_id };
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
    capabilities: CapabilitiesTab,
}

impl Default for WorkspaceNav {
    fn default() -> Self {
        Self {
            active: WorkspaceRoute::Session,
            agents: AgentsView::Catalog,
            automations: AutomationsView::List,
            capabilities: CapabilitiesTab::Models,
        }
    }
}

impl WorkspaceNav {
    pub(crate) fn from_persisted(route: WorkspaceRoute) -> Self {
        Self {
            active: route,
            agents: AgentsView::Catalog,
            automations: AutomationsView::List,
            capabilities: CapabilitiesTab::Models,
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

    pub(crate) fn capabilities_tab(&self) -> CapabilitiesTab {
        self.capabilities
    }

    pub(crate) fn set_capabilities_tab(&mut self, tab: CapabilitiesTab) {
        self.capabilities = tab;
    }

    pub(crate) fn set_active(&mut self, tab: WorkspaceRoute) {
        self.active = tab;
    }

    pub(crate) fn pop_active_to_root(&mut self) {
        match self.active {
            WorkspaceRoute::Agents => self.agents.pop_to_root(),
            WorkspaceRoute::Automations => self.automations.pop_to_root(),
            WorkspaceRoute::Capabilities | WorkspaceRoute::Session => {}
        }
    }

    pub(crate) fn persist(&self) -> WorkspaceRoute {
        self.active.clone()
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
        let mut view = AgentsView::Catalog;
        view.open_editor("agent-1".into());
        assert_eq!(view.editor_id(), Some("agent-1"));
        assert!(view.conversation_sid().is_none());

        view.open_conversation("sid-1".into());
        assert_eq!(view.conversation_sid(), Some("sid-1"));
        assert!(view.editor_id().is_none());

        view.pop_to_root();
        assert_eq!(view, AgentsView::Catalog);
        assert!(view.conversation_sid().is_none());
        assert!(view.editor_id().is_none());
    }

    #[test]
    fn automations_editor_is_one_level_above_the_list() {
        let mut view = AutomationsView::List;
        view.open_editor("auto-1".into());
        assert_eq!(view.editor_id(), Some("auto-1"));
        assert_eq!(view.automation_id(), Some("auto-1"));

        view.open_editor("auto-1".into());
        assert_eq!(
            view,
            AutomationsView::Editor {
                automation_id: "auto-1".into()
            }
        );

        view.pop_to_root();
        assert_eq!(view, AutomationsView::List);
        assert!(view.editor_id().is_none());
        assert!(view.automation_id().is_none());
    }

    #[test]
    fn sidebar_pop_to_root_clears_drill_in_on_the_active_face_only() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents);
        nav.agents_mut().open_editor("agent-1".into());
        nav.automations_mut().open_editor("auto-1".into());

        nav.pop_active_to_root();
        assert_eq!(nav.agents(), &AgentsView::Catalog);
        assert_eq!(nav.automations().editor_id(), Some("auto-1"));

        nav.set_active(WorkspaceRoute::Automations);
        nav.pop_active_to_root();
        assert_eq!(nav.automations(), &AutomationsView::List);
        assert_eq!(nav.agents(), &AgentsView::Catalog);
    }

    #[test]
    fn capabilities_tab_survives_returning_to_the_root() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Session);
        assert_eq!(nav.capabilities_tab(), CapabilitiesTab::Models);
        nav.set_capabilities_tab(CapabilitiesTab::Plugins);
        nav.set_active(WorkspaceRoute::Capabilities);
        nav.pop_active_to_root();
        assert_eq!(
            nav.capabilities_tab(),
            CapabilitiesTab::Plugins,
            "页签是平级切换，不是钻取，点侧栏标题不应把它重置"
        );
        assert_eq!(nav.persist(), WorkspaceRoute::Capabilities);
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
            serde_json::to_string(&WorkspaceRoute::Capabilities).unwrap(),
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
        assert!(serde_json::from_str::<WorkspaceRoute>("\"retired-plugin\"").is_err());
    }
}
