//! 工作台当前显示哪一个一级入口。
//!
//! 左侧「智能体 / 自动化」是分组标题，不是 iOS/长桥那种保活 Tab：点标题总是进
//! 该面的根页。对话和运行现场是侧栏里的子行，只切当前面、不清根页。插件工作台
//! 与这三个系统入口并列，不再把 `route` 写成 Session 再盖一层 surface。

/// 左侧一级导航。智能体、自动化、会话与插件工作台并列。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceRoute {
    Agents,
    Automations,
    #[default]
    Session,
    Plugin {
        key: String,
    },
}

impl WorkspaceRoute {
    pub(crate) fn plugin_key(&self) -> Option<&str> {
        match self {
            Self::Plugin { key } => Some(key.as_str()),
            _ => None,
        }
    }

    pub(crate) fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }

    pub(crate) fn is_plugin(&self) -> bool {
        matches!(self, Self::Plugin { .. })
    }

    /// 切到这个面时，要不要把插件 WebView 的 key window 交还给 GPUI。
    ///
    /// 插件面本身就是 WebView：侧栏点击落在 GPUI 上，若立刻 `release_focus`，
    /// 子窗口要再点一次才成为 key，表现就是「必须点两次才能进入」。
    pub(crate) fn releases_plugin_key(&self) -> bool {
        !self.is_plugin()
    }

    /// 侧栏分组标题要点的面：应回到根页。插件工作台没有面内钻取。
    pub(crate) fn sidebar_opens_root(&self) -> bool {
        !self.is_plugin()
    }
}

/// 智能体面内页。定义列表是根；编辑器和对话是钻取，不落盘。
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

/// 自动化面内页。列表是根；编辑器 / 历史 / Run 详情 / 运行现场是钻取，不落盘。
///
/// 返回层级：Live 和 Run 回到 History，History 回到 Editor，Editor 回到 List。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutomationsView {
    #[default]
    List,
    Editor {
        automation_id: String,
    },
    History {
        automation_id: String,
    },
    Run {
        automation_id: String,
        run_id: String,
    },
    Live {
        automation_id: String,
        run_id: String,
    },
}

impl AutomationsView {
    pub(crate) fn automation_id(&self) -> Option<&str> {
        match self {
            Self::List => None,
            Self::Editor { automation_id }
            | Self::History { automation_id }
            | Self::Run { automation_id, .. }
            | Self::Live { automation_id, .. } => Some(automation_id.as_str()),
        }
    }

    pub(crate) fn editor_id(&self) -> Option<&str> {
        match self {
            Self::Editor { automation_id } => Some(automation_id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn is_history(&self) -> bool {
        matches!(self, Self::History { .. })
    }

    pub(crate) fn run_id(&self) -> Option<&str> {
        match self {
            Self::Run { run_id, .. } | Self::Live { run_id, .. } => Some(run_id.as_str()),
            Self::List | Self::Editor { .. } | Self::History { .. } => None,
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        matches!(self, Self::Live { .. })
    }

    pub(crate) fn open_editor(&mut self, automation_id: String) {
        *self = Self::Editor { automation_id };
    }

    pub(crate) fn open_history(&mut self, automation_id: String) {
        *self = Self::History { automation_id };
    }

    pub(crate) fn open_run(&mut self, automation_id: String, run_id: String) {
        *self = Self::Run {
            automation_id,
            run_id,
        };
    }

    pub(crate) fn open_live(&mut self, automation_id: String, run_id: String) {
        *self = Self::Live {
            automation_id,
            run_id,
        };
    }

    pub(crate) fn back(&mut self) {
        *self = match self {
            Self::List => Self::List,
            Self::Editor { .. } => Self::List,
            Self::History { automation_id } => Self::Editor {
                automation_id: automation_id.clone(),
            },
            Self::Run { automation_id, .. } | Self::Live { automation_id, .. } => Self::History {
                automation_id: automation_id.clone(),
            },
        };
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
    pub(crate) fn from_persisted(route: WorkspaceRoute, surface: Option<String>) -> Self {
        let active = match surface.filter(|key| !key.trim().is_empty()) {
            Some(key) => WorkspaceRoute::Plugin { key },
            None => match route {
                WorkspaceRoute::Plugin { key } if !key.trim().is_empty() => {
                    WorkspaceRoute::Plugin { key }
                }
                WorkspaceRoute::Plugin { .. } => WorkspaceRoute::Session,
                other => other,
            },
        };
        Self {
            active,
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
            WorkspaceRoute::Session | WorkspaceRoute::Plugin { .. } => {}
        }
    }

    pub(crate) fn close_plugin(&mut self) -> bool {
        if self.active.is_plugin() {
            self.active = WorkspaceRoute::Session;
            true
        } else {
            false
        }
    }

    /// 落盘时拆回旧字段：plugin 仍写 `active_workspace_surface`，`route` 保持三态。
    pub(crate) fn persist(&self) -> (WorkspaceRoute, Option<String>) {
        match &self.active {
            WorkspaceRoute::Plugin { key } => (WorkspaceRoute::Session, Some(key.clone())),
            other => (other.clone(), None),
        }
    }
}

impl crate::Workspace {
    pub(crate) fn active_tab(&self) -> &WorkspaceRoute {
        self.nav.active()
    }

    pub(crate) fn plugin_surface_key(&self) -> Option<&str> {
        self.nav.active().plugin_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_surface_wins_over_session_route() {
        let nav =
            WorkspaceNav::from_persisted(WorkspaceRoute::Session, Some("com.example/board".into()));
        assert_eq!(
            nav.active(),
            &WorkspaceRoute::Plugin {
                key: "com.example/board".into()
            }
        );
        assert_eq!(
            nav.persist(),
            (WorkspaceRoute::Session, Some("com.example/board".into()))
        );
    }

    #[test]
    fn empty_or_missing_surface_keeps_system_tab() {
        let nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents, None);
        assert_eq!(nav.active(), &WorkspaceRoute::Agents);
        assert_eq!(nav.persist(), (WorkspaceRoute::Agents, None));

        let nav = WorkspaceNav::from_persisted(WorkspaceRoute::Automations, Some("  ".into()));
        assert_eq!(nav.active(), &WorkspaceRoute::Automations);
    }

    #[test]
    fn persisted_surface_still_wins_when_route_is_agents() {
        let nav =
            WorkspaceNav::from_persisted(WorkspaceRoute::Agents, Some("com.example/board".into()));
        assert_eq!(nav.active().plugin_key(), Some("com.example/board"));
    }

    #[test]
    fn set_active_only_changes_the_current_tab() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents, None);
        nav.set_active(WorkspaceRoute::Automations);
        assert_eq!(nav.active(), &WorkspaceRoute::Automations);
        nav.set_active(WorkspaceRoute::Agents);
        assert_eq!(nav.active(), &WorkspaceRoute::Agents);
    }

    #[test]
    fn opening_a_plugin_route_must_not_steal_webview_key() {
        assert!(
            !WorkspaceRoute::Plugin {
                key: "com.example.board/board".into()
            }
            .releases_plugin_key(),
            "切到插件工作台时不能 release_focus，否则还要再点一次页面"
        );
        assert!(WorkspaceRoute::Session.releases_plugin_key());
        assert!(WorkspaceRoute::Agents.releases_plugin_key());
        assert!(WorkspaceRoute::Automations.releases_plugin_key());
    }

    #[test]
    fn plugin_tabs_are_distinct_and_close_back_to_session() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Session, None);
        nav.set_active(WorkspaceRoute::Plugin {
            key: "a/board".into(),
        });
        assert_eq!(nav.active().plugin_key(), Some("a/board"));
        nav.set_active(WorkspaceRoute::Plugin {
            key: "b/other".into(),
        });
        assert_eq!(nav.active().plugin_key(), Some("b/other"));
        assert!(nav.close_plugin());
        assert!(nav.active().is_session());
        assert!(!nav.close_plugin());
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
    fn automations_editor_history_and_run_back_in_order() {
        let mut view = AutomationsView::List;
        view.open_editor("auto-1".into());
        assert_eq!(view.editor_id(), Some("auto-1"));

        view.open_history("auto-1".into());
        view.open_run("auto-1".into(), "run-1".into());
        view.back();
        assert_eq!(
            view,
            AutomationsView::History {
                automation_id: "auto-1".into()
            }
        );

        view.open_live("auto-1".into(), "run-2".into());
        view.back();
        assert_eq!(
            view,
            AutomationsView::History {
                automation_id: "auto-1".into()
            }
        );
        view.back();
        assert_eq!(
            view,
            AutomationsView::Editor {
                automation_id: "auto-1".into()
            }
        );
        view.back();
        assert_eq!(view, AutomationsView::List);
    }

    #[test]
    fn sidebar_pop_to_root_clears_drill_in_on_the_active_face_only() {
        let mut nav = WorkspaceNav::from_persisted(WorkspaceRoute::Agents, None);
        nav.agents_mut().open_editor("agent-1".into());
        nav.automations_mut()
            .open_live("auto-1".into(), "run-1".into());

        nav.pop_active_to_root();
        assert_eq!(nav.agents(), &AgentsView::Catalog);
        assert!(nav.automations().is_live());

        nav.set_active(WorkspaceRoute::Automations);
        nav.pop_active_to_root();
        assert_eq!(nav.automations(), &AutomationsView::List);
        assert_eq!(nav.agents(), &AgentsView::Catalog);
    }

    #[test]
    fn sidebar_system_entries_open_root_plugin_does_not() {
        assert!(WorkspaceRoute::Agents.sidebar_opens_root());
        assert!(WorkspaceRoute::Automations.sidebar_opens_root());
        assert!(WorkspaceRoute::Session.sidebar_opens_root());
        assert!(
            !WorkspaceRoute::Plugin {
                key: "a/board".into()
            }
            .sidebar_opens_root()
        );
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
