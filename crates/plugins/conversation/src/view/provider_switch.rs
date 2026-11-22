use super::*;
use smelt_plugin_api::client::protocol::control::EmptyParams;
use smelt_plugin_api::model::control_api::{ConversationSwitchProvider, NewSessionCatalog};
use smelt_plugin_api::model::provider_switch::ProviderSwitchParams;
use smelt_plugin_api::model::workspace_directory::WorkspaceAcp;

impl ConversationView {
    pub(super) fn current_provider_key(&self) -> String {
        smelt_plugin_api::model::provider_switch::provider_key(&WorkspaceAcp {
            agent: self.agent.id().into(),
            profile_id: self.profile_id.clone(),
            agent_definition_id: self.agent_definition_id.clone(),
            ..Default::default()
        })
    }

    pub(super) fn load_provider_options(&mut self, cx: &mut Context<Self>) {
        let runtime = self.runtime.client().clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    runtime.call::<NewSessionCatalog>(
                        &EmptyParams::default(),
                        std::time::Duration::from_secs(10),
                    )
                })
                .await;
            if let Ok(catalog) = result {
                let _ = this.update(cx, |view, cx| {
                    view.provider_options = catalog.agents;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn adopt_provider(&mut self, provider: WorkspaceAcp) {
        if let Some(agent) = ConversationAgentKind::from_id(&provider.agent) {
            self.agent = agent;
            self.launch = provider.launch;
            self.profile_id = provider.profile_id;
            self.agent_definition_id = provider.agent_definition_id;
            self.launch_prep = None;
            self.pending_initial_prompt = None;
            self.restart_config_values.clear();
        }
    }

    pub(super) fn can_switch_provider(&self) -> bool {
        self.phase == DaemonPhase::Idle
            && !self.provider_switch_pending
            && !self.prompt_dispatch_pending
            && !self.compacting
            && self.permissions.is_empty()
            && self.elicitation.is_none()
            && matches!(
                self.conversation_binding,
                smelt_plugin_api::model::conversation::ConversationBinding::Direct
            )
    }

    pub(super) fn switch_provider(&mut self, target: String, cx: &mut Context<Self>) {
        if !self.can_switch_provider() || self.current_provider_key() == target {
            return;
        }
        let params = ProviderSwitchParams {
            session_id: self.sid.clone(),
            expected_provider: self.current_provider_key(),
            agent_option_id: target,
        };
        self.provider_switch_pending = true;
        self.provider_switch_error = None;
        let runtime = self.runtime.client().clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    runtime
                        .call::<ConversationSwitchProvider>(
                            &params,
                            std::time::Duration::from_secs(60),
                        )
                        .map_err(smelt_plugin_api::call_error_text)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.provider_switch_pending = false;
                match result {
                    Ok(provider) => view.adopt_provider(provider),
                    Err(error) => view.provider_switch_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn provider_selection_keeps_session_and_directory_and_requires_idle(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = cx.new(|cx| {
            ConversationView::placeholder(
                cx,
                ConversationOrigin {
                    runtime: Runtime::local(),
                    agent: ConversationAgentKind::Codex,
                    launch: ConversationLaunchSpec::from_command("codex app-server"),
                    launch_prep: None,
                    profile_id: None,
                    cwd: Some("/workspace/project".into()),
                    reason: String::new(),
                    resume_session_id: None,
                    saved_sid: None,
                },
            )
        });
        view.update(cx, |view, _| {
            let sid = view.sid.clone();
            view.phase = DaemonPhase::Idle;
            assert!(view.can_switch_provider());
            view.adopt_provider(WorkspaceAcp {
                agent: "claude".into(),
                launch: ConversationLaunchSpec::from_command("claude-acp"),
                ..Default::default()
            });
            assert_eq!(view.sid, sid);
            assert_eq!(view.cwd.as_deref(), Some("/workspace/project"));
            assert_eq!(view.agent, ConversationAgentKind::Claude);
            assert_eq!(view.current_provider_key(), "claude");
            view.phase = DaemonPhase::Thinking;
            assert!(!view.can_switch_provider());
        });
    }
}
