//! 演示包的固定窗口内容：横幅，以及按启动参数落到哪一页。

use gpui::{App, Context, ParentElement, Styled, Window, div, px};
use gpui_component::ActiveTheme;

use super::{
    Session, SessionKind, SessionUiState, Workspace, WorkspaceRoute, next_session_ui_id,
    unix_now_secs,
};
use crate::acp_view;
use crate::settings;

impl Workspace {
    pub(crate) fn install_demo_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !smelt_demo::is_active() {
            return;
        }
        if smelt_demo::route() == smelt_demo::DemoRoute::Conversation {
            self.install_demo_conversation(window, cx);
        }
        self.reassert_demo_surface(window, cx);
        if smelt_demo::route() == smelt_demo::DemoRoute::SettingsAgentRuntime {
            self.settings_section = settings::SettingsSection::AgentRuntime;
            // 构造函数还占着 Workspace。开设置窗口必须等这轮 update 结束。
            cx.defer_in(window, |this, _window, cx| {
                this.open_settings_section(settings::SettingsSection::AgentRuntime, cx);
            });
        }
    }

    /// 目录恢复会改活动会话。演示要落在指定页时，恢复完再钉一次。
    pub(crate) fn reassert_demo_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !smelt_demo::is_active() {
            return;
        }
        match smelt_demo::route() {
            smelt_demo::DemoRoute::Workspace => {
                self.nav.set_active(WorkspaceRoute::Session);
            }
            smelt_demo::DemoRoute::Agents => {
                self.nav.set_active(WorkspaceRoute::Agents);
            }
            smelt_demo::DemoRoute::Automations => {
                self.nav.set_active(WorkspaceRoute::Automations);
            }
            smelt_demo::DemoRoute::Conversation => {
                if let Some(index) = self.demo_conversation_index(cx) {
                    self.open_agent_conversation(index, window, cx);
                }
            }
            smelt_demo::DemoRoute::SettingsAgentRuntime => {}
        }
    }

    fn install_demo_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.demo_conversation_index(cx).is_some() {
            return;
        }
        let project = smelt_demo::project_dir();
        let permission = smelt_demo::showcase_permission(&project);
        let entries = smelt_demo::showcase_entries();
        let view = cx.new(|cx| {
            acp_view::AcpView::showcase(
                window,
                cx,
                acp_view::AcpViewOrigin {
                    runtime: crate::runtimes::Runtime::local(),
                    agent: settings::ConversationAgentKind::Pi,
                    launch: smelt_model::agent_kind::ConversationLaunchSpec::from_command(
                        "demo-local",
                    ),
                    launch_prep: None,
                    // 不跟设置里的真实启动命令走，避免演示对话变成一场真会话。
                    refresh_launch_from_settings: false,
                    profile_id: None,
                    // 没有项目 cwd，这段对话才进智能体页，而不是项目会话列表。
                    cwd: None,
                    reason: String::new(),
                    entries,
                    resume_session_id: None,
                    saved_sid: Some(smelt_demo::SHOWCASE_CONVERSATION_ID.to_string()),
                },
                permission,
            )
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            conversation_id: smelt_demo::SHOWCASE_CONVERSATION_ID.to_string(),
            kind: SessionKind::Conversation(view),
            last_updated_at: unix_now_secs(),
            custom_title: Some("整理灯塔欢迎语".into()),
            agent_definition_id: Some(smelt_demo::SHOWCASE_AGENT_ID.to_string()),
            automation_id: None,
            remote_owned: false,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
    }

    fn demo_conversation_index(&self, cx: &App) -> Option<usize> {
        self.sessions.iter().position(|session| {
            session
                .active_acp()
                .is_some_and(|view| view.read(cx).is_showcase())
        })
    }
}

pub(crate) fn banner(cx: &App) -> gpui::Div {
    let theme = ActiveTheme::theme(cx);
    div()
        .h(px(28.))
        .w_full()
        .pl(px(78.))
        .flex()
        .items_center()
        .text_xs()
        .bg(theme.warning)
        .text_color(theme.warning_foreground)
        .child("演示模式 · 临时数据，不会写入你的智能体配置")
}
