//! 智能体详情页上的「自动化」页签：填智能体插件的插槽 [`AGENT_TAB`]，每个打开的智能体一份。
//!
//! 列出挂着这个智能体的自动化（开关就地改）和它们最近的几次运行；新建、改、看运行详情都去
//! 自动化页。读的是插件启动时装好的投影（[`AutomationStore`]），render 只读内存。

use std::rc::Rc;

use api::model::automation::{Automation, AutomationCommand, AutomationRun, AutomationRunStatus};
use api::ui::Switch;
use api::{PluginView, ViewHandle, ViewSpec};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

use crate::VIEW_ID;
use crate::agents::AgentCache;
use crate::peers::agents::{AGENT_TAB, AgentTabArgs};
use crate::prelude::*;
use crate::projection::AutomationStore;

/// 「最近运行」最多列几次。全部记录在自动化页里看。
const RECENT_RUNS: usize = 5;

pub(crate) fn spec() -> ViewSpec {
    ViewSpec {
        name: "agent-tab".into(),
        title: api::text!("title"),
        icon: IconName::Workflow.path(),
        draws_top_bar: false,
        instances: true,
        settings: None,
        fills: vec![AGENT_TAB.into()],
        build: Rc::new(|_, cx| Rc::new(cx.new(AgentTab::new)) as Rc<dyn ViewHandle>),
    }
}

struct AgentTab {
    /// 第一次 `open` 给的智能体；参数不对、还没 `open` 时是 `None`，什么都不画。
    agent: Option<AgentTabArgs>,
    /// 上一次开关没改成，就地写在列表上面。
    error: Option<String>,
    _subscriptions: [Subscription; 2],
}

impl AgentTab {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            agent: None,
            error: None,
            _subscriptions: [
                cx.observe_global::<AutomationStore>(|_, cx| cx.notify()),
                cx.observe_global::<AgentCache>(|_, cx| cx.notify()),
            ],
        }
    }

    /// 开 / 关一条自动化，发给它所在的机器，等它推回来。
    fn set_enabled(&mut self, automation_id: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(machine) = self.agent.as_ref().map(|agent| agent.machine.clone()) else {
            return;
        };
        self.error = None;
        let client = match client_for(&machine, cx) {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let command = AutomationCommand::SetEnabled {
            automation_id,
            enabled,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { api::session_ops::submit_automation_command(&client, &command) },
                )
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
}

impl PluginView for AgentTab {
    fn open(&mut self, args: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        if self.agent.is_some() {
            return;
        }
        match api::serde_json::from_value::<AgentTabArgs>(args.clone()) {
            Ok(args) => {
                self.agent = Some(args);
                cx.notify();
            }
            Err(error) => eprintln!("[automations] 智能体页签的参数不对：{error}"),
        }
    }
}

/// 去自动化页，参数见 [`crate::page`]。
fn open_automations(args: api::serde_json::Value, cx: &mut App) {
    let open = api::OpenView {
        view: VIEW_ID.to_string(),
        args,
    };
    let task = api::OPEN_VIEW.run(&open, cx);
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[automations] 打开自动化页失败：{error}");
        }
    })
    .detach();
}

impl Render for AgentTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(agent) = self.agent.clone() else {
            return div().into_any_element();
        };
        let entity = cx.entity();
        let empty = MachineAutomations::default();
        let machine = automations(cx).on(&agent.machine).unwrap_or(&empty);
        let mine = machine.automations_for(&agent.agent);
        let name = agent_definition(&agent.machine, &agent.agent, cx)
            .map(|definition| agent_display_name(&definition.name))
            .unwrap_or_default();
        let machine_json = api::machine_to_json(&agent.machine);

        let create_args = api::serde_json::json!({
            "machine": machine_json,
            "create_for_agent": agent.agent,
        });
        let create = Button::new("agent-tab-new-automation")
            .small()
            .icon(IconName::Plus)
            .label(t!("agent_tab.new"))
            .on_click(move |_, _, cx| open_automations(create_args.clone(), cx));

        let now = Local::now().timestamp();
        let list = if mine.is_empty() {
            div()
                .px_4()
                .py_4()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child(t!("agent_tab.empty").into_owned())
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(mine.iter().map(|automation| {
                    automation_row(automation, machine, &machine_json, now, entity.clone())
                }))
                .into_any_element()
        };

        let mut runs: Vec<&AutomationRun> = machine
            .runs
            .iter()
            .filter(|run| mine.iter().any(|it| it.id == run.automation_id))
            .collect();
        runs.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        runs.truncate(RECENT_RUNS);

        div()
            .id("agent-tab-automations-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(720.))
                    .mx_auto()
                    .px_8()
                    .py_5()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .children(self.error.clone().map(render_agent_error))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(section_header(
                                t!("agent_tab.title", name = name).into_owned(),
                                Some(create.into_any_element()),
                            ))
                            .child(content_card().child(list)),
                    )
                    .when(!runs.is_empty(), |column| {
                        column.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(section_header(t!("agent_tab.recent").into_owned(), None))
                                .child(content_card().flex().flex_col().children(
                                    runs.into_iter().map(|run| run_row(run, &machine_json)),
                                )),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn section_header(title: String, action: Option<AnyElement>) -> AnyElement {
    div()
        .px_1()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE.ui_medium)
                .text_color(rgb(ui_theme::muted()))
                .overflow_hidden()
                .child(truncated_line(title)),
        )
        .children(action)
        .into_any_element()
}

/// 一条自动化：名字、什么时候跑（下次几时或已暂停），右边开关。点行去自动化页改它。
fn automation_row(
    automation: &Automation,
    machine: &MachineAutomations,
    machine_json: &api::serde_json::Value,
    now: i64,
    entity: Entity<AgentTab>,
) -> AnyElement {
    let name = if automation.name.trim().is_empty() {
        t!("automation.unnamed").into_owned()
    } else {
        automation.name.clone()
    };
    let when = if automation.enabled {
        let next = machine
            .state_for(&automation.id)
            .and_then(|state| state.next_run_at)
            .map(|at| format_relative_until(at, now));
        match next {
            Some(next) => t!("agent_tab.next", when = next).into_owned(),
            None => String::new(),
        }
    } else {
        t!("agent_tab.paused").into_owned()
    };
    let schedule = trigger_summary(&automation.trigger);
    let line = [schedule, when]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let open_args = api::serde_json::json!({
        "machine": machine_json,
        "automation": automation.id,
    });
    let toggle_id = automation.id.clone();
    ListRow::new(SharedString::from(format!(
        "agent-tab-automation-{}",
        automation.id
    )))
    .w_full()
    .px_4()
    .py_3()
    .flex()
    .items_center()
    .gap_3()
    .child(automation_run_lines(name, line, None).flex_1())
    .child(
        div()
            .flex_shrink_0()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Switch::new(SharedString::from(format!(
                    "agent-tab-automation-enabled-{}",
                    automation.id
                )))
                .checked(automation.enabled)
                .accessibility_label(t!("agent_tab.enabled"))
                .on_change(move |enabled, _, cx| {
                    let id = toggle_id.clone();
                    entity.update(cx, |tab, cx| tab.set_enabled(id, enabled, cx));
                }),
            ),
    )
    .on_click(move |_, _, cx| open_automations(open_args.clone(), cx))
    .into_any_element()
}

/// 一次运行：哪条、结果、几时，失败的带一行原因。点了去自动化页停在这次运行上。
fn run_row(run: &AutomationRun, machine_json: &api::serde_json::Value) -> AnyElement {
    let name = if run.context.automation_name.trim().is_empty() {
        t!("automation.unnamed").into_owned()
    } else {
        run.context.automation_name.clone()
    };
    let at = run.finished_at.unwrap_or(run.created_at);
    let error = run
        .error_text()
        .filter(|_| run.status == AutomationRunStatus::Failed)
        .as_deref()
        .map(compact_instructions);
    let open_args = api::serde_json::json!({
        "machine": machine_json,
        "automation": run.automation_id,
        "run": run.id,
    });
    ListRow::new(SharedString::from(format!("agent-tab-run-{}", run.id)))
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(crate::run_detail::run_status_dot(run.status))
        .child(
            automation_run_lines(
                name,
                format!(
                    "{} · {}",
                    run_status_label(run.status),
                    format_unix_local(at)
                ),
                error,
            )
            .flex_1(),
        )
        .on_click(move |_, _, cx| open_automations(open_args.clone(), cx))
        .into_any_element()
}
