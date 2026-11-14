//! 页面上的动作：发给运行时的写入、编辑器的增删改、选中运行。
//!
//! 写入一律发给自动化所在的那台机器；结果等运行时经 `automations.changed` 推回来，命令回包
//! 不写同一份数据。

use crate::prelude::*;

impl AutomationsPage {
    pub(crate) fn automation_snapshot_is_current_store(
        &self,
        runtime: &RuntimeKey,
        snapshot: &api::model::automation::AutomationFile,
        cx: &App,
    ) -> bool {
        let Some(machine) = automations(cx).on(runtime) else {
            return true;
        };
        !machine.is_retired_store(&snapshot.store_id)
            && (machine.store_id.is_empty() || machine.store_id == snapshot.store_id)
    }

    /// 发给自动化所在的那台机器。
    pub(crate) fn submit_automation_command(
        &mut self,
        runtime: &RuntimeKey,
        command: api::model::automation::AutomationCommand,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        let client = match client_for(runtime, cx) {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { api::session_ops::submit_automation_command(&client, &command) },
                )
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    // 自动化权威状态只由 EventHub 投影写入，命令回包不再写同一份数据。
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn run_automation_once(
        &mut self,
        automation: AutomationRef,
        cx: &mut Context<Self>,
    ) {
        self.submit_automation_command(
            &automation.runtime,
            api::model::automation::AutomationCommand::RunOnce {
                automation_id: automation.id,
            },
            cx,
        );
    }

    pub(crate) fn cancel_automation_run(
        &mut self,
        runtime: &RuntimeKey,
        run_id: String,
        cx: &mut Context<Self>,
    ) {
        self.submit_automation_command(
            runtime,
            api::model::automation::AutomationCommand::CancelRun { run_id },
            cx,
        );
    }

    /// 某台机器上的一次运行（快照里的）。
    pub(crate) fn automation_run(
        runtime: &RuntimeKey,
        matches: impl Fn(&api::model::automation::AutomationRun) -> bool,
        cx: &App,
    ) -> Option<api::model::automation::AutomationRun> {
        automations(cx)
            .on(runtime)?
            .runs
            .iter()
            .find(|run| matches(run))
            .cloned()
    }

    pub(crate) fn attach_automation_run_session(
        &mut self,
        runtime_key: &RuntimeKey,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let run = Self::automation_run(
            runtime_key,
            |run| run.session_id.as_deref() == Some(session_id),
            cx,
        );
        let Some(run) = run else {
            self.error = Some("找不到对应的运行记录".to_string());
            cx.notify();
            return;
        };
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.session_id == session_id)
        {
            self.error = None;
            cx.notify();
            return;
        }
        // 运行的会话不进工作区目录：用的哪个 agent、开在哪看运行记录；守护里已经没有它
        // （运行结束、runtime 已回收）就没有现场可看。
        let host = api::host(cx);
        let live = host
            .session_states(runtime_key, cx)
            .iter()
            .any(|state| state.id == session_id);
        if !live {
            self.live = None;
            self.error = None;
            cx.notify();
            return;
        }
        let Some(agent) = run
            .context
            .engine_kind_id
            .as_deref()
            .and_then(ConversationAgentKind::from_id)
        else {
            self.live = None;
            self.error = Some("无法识别该运行使用的智能体引擎".to_string());
            cx.notify();
            return;
        };
        // 现场在跑这次运行的那台机器上；接的是守护里正在跑的会话，不按规格重新起。
        let request = api::EmbedConversation {
            machine: runtime_key.clone(),
            session_id: session_id.to_string(),
            agent: agent.id().to_string(),
            cwd: run.context.cwd.clone(),
            binding: api::model::conversation::ConversationBinding::Automation { run_id: run.id },
            reason: "正在打开自动化运行现场…".to_string(),
        };
        match host.embed_conversation(request, window, cx) {
            Ok(live) => {
                self.live = Some(live);
                self.error = None;
            }
            Err(error) => {
                self.live = None;
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    /// 停在编辑器里，并选中这一次运行。有会话时右侧详情就是现场。
    pub(crate) fn select_automation_run(
        &mut self,
        runtime: &RuntimeKey,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = Self::automation_run(runtime, |run| run.id == run_id, cx) else {
            return;
        };
        self.selected_run_id = Some(run.id.clone());
        if let Some(session_id) = run.session_id.as_deref() {
            self.attach_automation_run_session(runtime, session_id, window, cx);
        } else {
            self.live = None;
            self.error = None;
            cx.notify();
        }
        // 没有现场的运行看落库内容。
        if !self.shows_live(&run) {
            self.sync_automation_run_detail(runtime, &run, cx);
        }
    }

    /// 回到任务表单。运行列表还在，只是右侧不再是某一次运行。
    pub(crate) fn show_automation_task(&mut self, cx: &mut Context<Self>) {
        self.selected_run_id = None;
        self.live = None;
        cx.notify();
    }

    pub(crate) fn open_catalog_automation_run(
        &mut self,
        runtime: RuntimeKey,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = Self::automation_run(&runtime, |run| run.id == run_id, cx) else {
            return;
        };
        let automation = AutomationRef::new(runtime, run.automation_id);
        self.navigate_to_automation_run(automation, run_id, window, cx);
    }

    pub(crate) fn navigate_to_automation_run(
        &mut self,
        automation: AutomationRef,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let runtime = automation.runtime.clone();
        self.route = Some(automation);
        self.select_automation_run(&runtime, run_id, window, cx);
    }

    pub(crate) fn close_automation_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self.selected_run_id = None;
        self.live = None;
        self.route = None;
        cx.notify();
    }

    /// 在某台机器上新建自动化默认用哪个智能体：那台机器上第一个就绪的。
    pub(crate) fn ready_automation_agent_id(runtime: &RuntimeKey, cx: &App) -> Option<String> {
        agents_on(runtime, cx)
            .iter()
            .find(|agent| agent.is_ready())
            .map(|agent| agent.id.clone())
    }

    pub(crate) fn set_automation_catalog_tab(
        &mut self,
        tab: AutomationCatalogTab,
        cx: &mut Context<Self>,
    ) {
        self.catalog_tab = tab;
        cx.notify();
    }

    pub(crate) fn set_automation_template_filter(
        &mut self,
        filter: Option<crate::templates::AutomationTemplateCategory>,
        cx: &mut Context<Self>,
    ) {
        self.template_filter = filter;
        cx.notify();
    }

    /// 在某台机器上新建自动化。
    pub(crate) fn add_automation(
        &mut self,
        runtime: RuntimeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent_id = Self::ready_automation_agent_id(&runtime, cx).unwrap_or_default();
        self.add_automation_for_agent(runtime, agent_id, window, cx);
    }

    /// 为指定智能体新建自动化，建在智能体所在的机器上。绑定的就是这一个，不改用列表里的
    /// 第一个；`agent_id` 为空时是命令动作。
    pub(crate) fn add_automation_for_agent(
        &mut self,
        runtime: RuntimeKey,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.catalog_tab = AutomationCatalogTab::Tasks;
        let action = if agent_id.trim().is_empty() {
            AutomationAction::shell(String::new(), Vec::new())
        } else {
            AutomationAction::Agent {
                agent_definition_id: agent_id,
                prompt: None,
            }
        };
        let automation = Automation {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            enabled: true,
            workspace_dir: None,
            trigger: AutomationTrigger::default(),
            action,
            sinks: vec![AutomationSink::Local],
        };
        self.open_automation_editor_for(runtime, automation, true, window, cx);
    }

    pub(crate) fn add_automation_from_template(
        &mut self,
        runtime: RuntimeKey,
        template_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = crate::templates::automation_template(template_id) else {
            return;
        };
        self.catalog_tab = AutomationCatalogTab::Tasks;
        let automation = Automation {
            id: uuid::Uuid::new_v4().to_string(),
            name: template.name.to_string(),
            enabled: true,
            workspace_dir: None,
            trigger: AutomationTrigger::schedule(template.schedule),
            action: AutomationAction::Agent {
                agent_definition_id: Self::ready_automation_agent_id(&runtime, cx)
                    .unwrap_or_default(),
                prompt: Some(template.prompt.to_string()),
            },
            sinks: vec![AutomationSink::Local],
        };
        self.open_automation_editor_for(runtime, automation, true, window, cx);
    }

    pub(crate) fn create_automation_from_editor(&mut self, cx: &mut Context<Self>) {
        if self.editor.as_ref().is_some_and(|editor| editor.is_new) {
            self.persist_automation_editor(None, cx);
        }
    }

    /// 删前确认：删掉的自动化连同运行记录一起没了，所以要两步。
    pub(crate) fn start_delete_automation(
        &mut self,
        target: AutomationRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(machine) = automations(cx).on(&target.runtime) else {
            return;
        };
        let Some(automation) = machine.automation(&target.id) else {
            return;
        };
        let name = if automation.name.trim().is_empty() {
            "未命名自动化".to_string()
        } else {
            automation.name.clone()
        };
        let runs = machine.runs_for(&target.id).len();
        let description = if runs == 0 {
            format!("「{name}」会被删除，此操作不可撤销。")
        } else {
            format!("「{name}」和它的 {runs} 条运行记录会被删除，此操作不可撤销。")
        };
        let page = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let page = page.clone();
            let target = target.clone();
            alert
                .confirm()
                .title("确定删除这条自动化吗？")
                .description(description.clone())
                .ok_text("确定删除")
                .ok_variant(gpui_component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let target = target.clone();
                    let _ = page.update(cx, |page, cx| page.delete_automation(target, cx));
                    true
                })
        });
    }

    pub(crate) fn delete_automation(&mut self, target: AutomationRef, cx: &mut Context<Self>) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation == target)
        {
            self.editor = None;
        }
        if self.route.as_ref() == Some(&target) {
            self.route = None;
            self.selected_run_id = None;
            self.live = None;
        }
        self.submit_automation_command(
            &target.runtime,
            api::model::automation::AutomationCommand::Delete {
                automation_id: target.id,
            },
            cx,
        );
    }

    pub(crate) fn set_automation_enabled(
        &mut self,
        target: AutomationRef,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.submit_automation_command(
            &target.runtime,
            api::model::automation::AutomationCommand::SetEnabled {
                automation_id: target.id,
                enabled,
            },
            cx,
        );
    }

    pub(crate) fn open_automation_editor(
        &mut self,
        target: AutomationRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation == target)
        {
            return;
        }
        let Some(automation) = automations(cx)
            .on(&target.runtime)
            .and_then(|machine| machine.automation(&target.id))
            .cloned()
        else {
            self.editor = None;
            return;
        };
        self.open_automation_editor_for(target.runtime, automation, false, window, cx);
    }

    /// 打开编辑器。`runtime` 是这条自动化所在（新建时是要建在）的那台机器。
    pub(crate) fn open_automation_editor_for(
        &mut self,
        runtime: RuntimeKey,
        automation: Automation,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let automation_ref = AutomationRef::new(runtime, automation.id.clone());
        let automation_id = automation.id.clone();
        self.route = Some(automation_ref.clone());
        let keep_run = self.selected_run_id.as_ref().is_some_and(|run_id| {
            Self::automation_run(
                &automation_ref.runtime,
                |run| run.id == *run_id && run.automation_id == automation_id,
                cx,
            )
            .is_some()
        });
        if !keep_run {
            self.selected_run_id = None;
            self.live = None;
        }
        let action_kind = AutomationActionKindId::from_action(&automation.action);
        let agent_id = automation
            .agent_definition_id()
            .unwrap_or_default()
            .to_string();
        let command_default = match &automation.action {
            AutomationAction::Shell { command, args } if args.is_empty() => command.clone(),
            AutomationAction::Shell { command, args } => std::iter::once(command.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            AutomationAction::Agent { .. } => String::new(),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("给它起个名字")
                .default_value(automation.name.clone())
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("例如：汇总今日进展并列出待办事项")
                .default_value(automation.prompt().unwrap_or_default().to_string())
        });
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("例如：curl -fsS https://example.com/health")
                .default_value(command_default)
        });
        let retained_events = automation.trigger.events.clone();
        let mut entries = automation
            .trigger
            .schedules
            .iter()
            .map(|schedule| {
                new_trigger_entry(
                    window,
                    cx,
                    if schedule.is_interval() {
                        AutomationTriggerKindId::Interval
                    } else {
                        AutomationTriggerKindId::Calendar
                    },
                    Some(schedule),
                    None,
                )
            })
            .collect::<Vec<_>>();
        for webhook in &automation.trigger.webhooks {
            let secret = webhook_ingress_secret(webhook);
            entries.push(new_trigger_entry(
                window,
                cx,
                AutomationTriggerKindId::Webhook,
                None,
                Some(secret),
            ));
        }
        let notification = AutomationNotificationPreset::from_sinks(&automation.sinks);
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&name, window, {
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.focus(&this.focus_handle, cx);
                }
                if identity_name_should_finish_edit(event) {
                    this.finish_automation_name_edit(cx);
                }
            }
        }));
        for entry in &entries {
            subscribe_trigger_entry_inputs(&mut subscriptions, entry, cx);
        }
        subscriptions.push(cx.subscribe(&prompt, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
        subscriptions.push(cx.subscribe(&command, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
        self.editor = Some(AutomationEditor {
            instance_id: uuid::Uuid::new_v4(),
            action_kind,
            agent_id,
            automation: automation_ref,
            is_new,
            name,
            name_editing: is_new && automation.name.trim().is_empty(),
            prompt,
            command,
            retained_events,
            entries,
            notification,
            draft_revision: 0,
            saving_revision: None,
            quiet_save: false,
            error: None,
            flash: None,
            _subscriptions: subscriptions,
        });
        let name_input = self
            .editor
            .as_ref()
            .map(|editor| (editor.name_editing, editor.name.clone()));
        match name_input {
            Some((true, name)) => name.update(cx, |state, cx| state.focus(window, cx)),
            _ => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    pub(crate) fn clear_automation_editor_error(&mut self, cx: &mut Context<Self>) {
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_trigger_schedule_kind(
        &mut self,
        index: usize,
        kind: ScheduleKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.kind = kind;
            if kind == ScheduleKind::Weekdays {
                rule.weekday_mask = api::model::automation::SCHEDULE_WEEKDAYS_MASK;
                rule.interval_day_scope = IntervalDayScope::Weekdays;
            } else if kind == ScheduleKind::Weekly {
                if rule.weekday_mask == 0 {
                    rule.weekday_mask = api::model::automation::SCHEDULE_WEEKDAYS_MASK;
                }
                rule.interval_day_scope = IntervalDayScope::Weekly;
            } else if kind == ScheduleKind::Daily {
                rule.weekday_mask = api::model::automation::SCHEDULE_ALL_DAYS_MASK;
                rule.interval_day_scope = IntervalDayScope::Everyday;
            } else {
                rule.interval_day_scope = IntervalDayScope::from_days(rule.weekday_mask);
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_interval_unit(
        &mut self,
        index: usize,
        unit: IntervalUnit,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.interval_unit = unit;
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_interval_day_scope(
        &mut self,
        index: usize,
        scope: IntervalDayScope,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.interval_day_scope = scope;
            match scope {
                IntervalDayScope::Everyday => {
                    rule.weekday_mask = api::model::automation::SCHEDULE_ALL_DAYS_MASK;
                }
                IntervalDayScope::Weekdays => {
                    rule.weekday_mask = api::model::automation::SCHEDULE_WEEKDAYS_MASK;
                }
                IntervalDayScope::Weekly => {
                    if rule.weekday_mask == 0 {
                        rule.weekday_mask = api::model::automation::SCHEDULE_WEEKDAYS_MASK;
                    }
                }
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_interval_window_open(
        &mut self,
        index: usize,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.window_open = open;
            if !open {
                rule.window_start
                    .update(cx, |state, cx| state.set_value("", window, cx));
                rule.window_end
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn toggle_schedule_weekday(
        &mut self,
        index: usize,
        day_bit: u8,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.weekday_mask ^= day_bit;
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn add_trigger_preset(
        &mut self,
        preset: AutomationTriggerPresetId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schedule = preset.schedule();
        let entry = new_trigger_entry(window, cx, preset.trigger_kind(), schedule.as_ref(), None);
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        subscribe_trigger_entry_inputs(&mut editor._subscriptions, &entry, cx);
        editor.entries.push(entry);
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn remove_trigger_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_mut()
            && index < editor.entries.len()
        {
            editor.entries.remove(index);
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_automation_notification(
        &mut self,
        preset: AutomationNotificationPreset,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editor.as_mut() {
            editor.notification = preset;
        }
        self.touch_schedule_editor(cx);
    }

    /// 已创建的自动化改动后自动保存；新建的只记修改，等用户点「创建」。
    pub(crate) fn touch_schedule_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.draft_revision = editor.draft_revision.wrapping_add(1);
        editor.error = None;
        let token = editor.draft_revision;
        let autosave = !editor.is_new;
        cx.notify();
        if !autosave {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(editor) = this.editor.as_ref() else {
                    return;
                };
                if editor.draft_revision != token {
                    return;
                }
                this.persist_automation_editor(None, cx);
            });
        })
        .detach();
    }

    pub(crate) fn regenerate_automation_webhook_secret(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let should_save = self.editor.as_ref().is_some_and(|editor| !editor.is_new);
        let flash_id = self
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| {
                let flash_id = format!(
                    "automation-regen-webhook-{}",
                    entry.schedule.time.entity_id()
                );
                entry.webhook_secret = api::model::automation::new_webhook_secret();
                flash_id
            });
        if should_save {
            if let Some(editor) = self.editor.as_mut() {
                editor.draft_revision = editor.draft_revision.wrapping_add(1);
                editor.error = None;
            }
            self.persist_automation_editor(flash_id.map(|id| (id, "已重新生成 ✓")), cx);
        } else {
            self.touch_schedule_editor(cx);
        }
    }

    pub(crate) fn set_automation_action_kind(
        &mut self,
        kind: AutomationActionKindId,
        cx: &mut Context<Self>,
    ) {
        let runtime = self
            .editor
            .as_ref()
            .map(|editor| editor.automation.runtime.clone());
        let fallback_agent = match runtime {
            Some(runtime) if kind == AutomationActionKindId::Agent => {
                Self::ready_automation_agent_id(&runtime, cx)
            }
            _ => None,
        };
        if let Some(editor) = self.editor.as_mut() {
            editor.action_kind = kind;
            if editor.agent_id.is_empty()
                && let Some(agent_id) = fallback_agent
            {
                editor.agent_id = agent_id;
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn set_automation_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.agent_id = agent_id;
        }
        self.touch_schedule_editor(cx);
    }

    pub(crate) fn test_automation_editor(&mut self, cx: &mut Context<Self>) {
        let Some(automation) = self.editor.as_ref().and_then(|editor| {
            (!editor.is_new && editor.saving_revision.is_none()).then(|| editor.automation.clone())
        }) else {
            return;
        };
        self.run_automation_once(automation, cx);
    }

    pub(crate) fn persist_automation_editor(
        &mut self,
        flash: Option<(String, &'static str)>,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        if editor.saving_revision.is_some() || !editor.missing_fields(cx).is_empty() {
            return;
        }
        let Ok(trigger) = trigger_from_editor(editor, cx) else {
            return;
        };

        let target = editor.automation.clone();
        let enabled = automations(cx)
            .on(&target.runtime)
            .and_then(|machine| machine.automation(&target.id))
            .map(|automation| automation.enabled)
            .unwrap_or(true);
        let client = match client_for(&target.runtime, cx) {
            Ok(client) => client,
            Err(error) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        let automation = Automation {
            id: target.id.clone(),
            name: editor.name.read(cx).value().trim().to_string(),
            enabled,
            workspace_dir: None,
            trigger,
            action: editor.action(cx),
            sinks: editor.notification.to_sinks(),
        };
        let created_name = editor.is_new.then(|| automation.name.clone());
        let editor_instance_id = editor.instance_id;
        let draft_revision = editor.draft_revision;
        if let Some(editor) = self.editor.as_mut() {
            editor.saving_revision = Some(draft_revision);
            editor.quiet_save = flash.is_none();
        }
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let command = api::model::automation::AutomationCommand::Upsert {
                automation: Box::new(automation),
            };
            let result = cx
                .background_executor()
                .spawn(
                    async move { api::session_ops::submit_automation_command(&client, &command) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut flashed = None;
                let retry = match result {
                    Ok((_result, snapshot)) => {
                        let current_store = this.automation_snapshot_is_current_store(
                            &target.runtime,
                            &snapshot,
                            cx,
                        );
                        if let Some(editor) = this.editor.as_mut().filter(|editor| {
                            editor.automation == target && editor.instance_id == editor_instance_id
                        }) {
                            editor.saving_revision = None;
                            if !current_store {
                                editor.error =
                                    Some("自动化服务已切换，请在当前版本重新保存".to_string());
                                false
                            } else {
                                editor.is_new = false;
                                if let Some(name) = &created_name {
                                    notify_once(format!("已创建自动化「{name}」"), cx);
                                }
                                if let Some((flash_id, flash_label)) = flash {
                                    flashed = Some((flash_id, flash_label));
                                }
                                editor.draft_revision != draft_revision
                            }
                        } else {
                            false
                        }
                    }
                    Err(error) => {
                        if let Some(editor) = this.editor.as_mut().filter(|editor| {
                            editor.automation == target && editor.instance_id == editor_instance_id
                        }) {
                            editor.saving_revision = None;
                            editor.error = Some(error);
                        }
                        false
                    }
                };
                if let Some((flash_id, flash_label)) = flashed {
                    this.flash_editor_button(flash_id, flash_label, cx);
                }
                if retry {
                    this.persist_automation_editor(None, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn finish_automation_name_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.name_editing = false;
        }
        cx.notify();
    }

    /// 按钮文案闪一下（「已复制 ✓」），两秒后换回来。
    pub(crate) fn flash_editor_button(
        &mut self,
        id: String,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.flash = Some((id.clone(), label));
        let instance_id = editor.instance_id;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.editor.as_mut()
                    && editor.instance_id == instance_id
                    && editor
                        .flash
                        .as_ref()
                        .is_some_and(|(flashed, _)| *flashed == id)
                {
                    editor.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
