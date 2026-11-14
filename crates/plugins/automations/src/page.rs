//! 自动化页的状态，和打开它时的参数。
//!
//! 打开参数（都可以不给，认不出的字段忽略；机器的写法见 [`api::machine_to_json`]）：
//!
//! - `{"machine": …, "automation": id}`：打开那条自动化的编辑器；
//! - 再带 `"run": id`：停在那条自动化里，选中这一次运行（通知点进来就是这个）；
//! - `{"machine": …, "create_for_agent": id}`：在那台机器上给这个智能体新建一条自动化。
//!
//! 什么都不带就停在原处。页面一直留着，离开再回来编辑器还在。
//!
//! render 只读内存：投影变了（[`AutomationStore`]）在事件里对账——要看的自动化没了就退回
//! 目录、选中的运行没了就取消选中、选中运行的落库内容过期了就排后台读取。

use api::model::agent_definition::AgentDefinition;
use api::{EmbeddedConversation, HostEvent, PluginView};

use crate::prelude::*;
use crate::projection::AutomationStore;

/// 智能体页的视图 id，由「智能体」插件登记；没装就不给去那里的入口。
pub(crate) const AGENTS_VIEW_ID: &str = "agents";

/// 某台机器上的一条自动化：两台机器上同 id 的自动化是两回事。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AutomationRef {
    pub(crate) runtime: RuntimeKey,
    pub(crate) id: String,
}

impl AutomationRef {
    pub(crate) fn new(runtime: RuntimeKey, id: impl Into<String>) -> Self {
        Self {
            runtime,
            id: id.into(),
        }
    }
}

/// 目录根页的分段：任务表 / 各台机器的运行。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutomationCatalogTab {
    #[default]
    Tasks,
    Runs,
}

pub(crate) struct AutomationsPage {
    /// 在看的那条自动化；`None` 是目录。
    pub(crate) route: Option<AutomationRef>,
    pub(crate) editor: Option<AutomationEditor>,
    /// 选中运行的现场：守护里还在跑的那段会话，由宿主嵌进来。
    pub(crate) live: Option<EmbeddedConversation>,
    pub(crate) error: Option<String>,
    pub(crate) catalog_tab: AutomationCatalogTab,
    pub(crate) template_filter: Option<crate::templates::AutomationTemplateCategory>,
    /// 编辑器里选中的那一次运行。`None` 表示右侧仍是任务表单。
    pub(crate) selected_run_id: Option<String>,
    /// 选中运行的落库内容。后台读好再放进来，render 里不读库。
    pub(crate) run_detail: Option<AutomationRunDetail>,
    /// 正在后台读取的那份运行版本，防止重复排读取。
    pub(crate) run_detail_loading: Option<RunDetailKey>,
    pub(crate) focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AutomationsPage {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe_global_in::<AutomationStore>(window, |this, window, cx| {
                this.reconcile(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(
                &api::events(cx),
                window,
                |this, _, event: &HostEvent, window, cx| match event {
                    HostEvent::MachinesChanged => {
                        this.reconcile(window, cx);
                        cx.notify();
                    }
                    HostEvent::AgentSettingsChanged | HostEvent::ViewsChanged => cx.notify(),
                    _ => {}
                },
            ),
        ];
        Self {
            route: None,
            editor: None,
            live: None,
            error: None,
            catalog_tab: AutomationCatalogTab::Tasks,
            template_filter: None,
            selected_run_id: None,
            run_detail: None,
            run_detail_loading: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// 投影变了之后对账：在看的自动化连同运行记录都没了（别处删掉）退回目录，选中的运行
    /// 没了取消选中；自动化刚推到就打开它的编辑器；选中运行没有现场时，落库内容过期就重读。
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(automation) = self.route.clone() else {
            return;
        };
        // 机器移除了：它的自动化不再列，退回目录。
        if !machine_keys(cx).contains(&automation.runtime) {
            self.close_automation_editor(cx);
            return;
        }
        let machine = automations(cx).on(&automation.runtime);
        let store_ready = machine.is_some_and(MachineAutomations::is_ready);
        let automation_exists =
            machine.is_some_and(|machine| machine.automation(&automation.id).is_some());
        let runs_exist = machine.is_some_and(|machine| {
            machine
                .runs
                .iter()
                .any(|run| run.automation_id == automation.id)
        });
        let selected_run = self.selected_run_id.as_ref().and_then(|run_id| {
            machine?
                .runs
                .iter()
                .find(|run| run.id == *run_id && run.automation_id == automation.id)
                .cloned()
        });
        let new_draft = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.is_new && editor.automation == automation);
        if store_ready && !automation_exists && !runs_exist && !new_draft {
            self.close_automation_editor(cx);
            return;
        }
        if store_ready && self.selected_run_id.is_some() && selected_run.is_none() {
            self.selected_run_id = None;
            self.live = None;
        }
        if automation_exists
            && self
                .editor
                .as_ref()
                .is_none_or(|editor| editor.automation != automation)
        {
            self.open_automation_editor(automation.clone(), window, cx);
        }
        if let Some(run) = selected_run.filter(|run| !self.shows_live(run)) {
            self.sync_automation_run_detail(&automation.runtime, &run, cx);
        }
    }

    /// 这次运行的现场正嵌着。
    pub(crate) fn shows_live(&self, run: &api::model::automation::AutomationRun) -> bool {
        run.session_id.as_deref().is_some_and(|session_id| {
            self.live
                .as_ref()
                .is_some_and(|live| live.session_id == session_id)
        })
    }
}

impl PluginView for AutomationsPage {
    fn open(&mut self, args: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let machine = args
            .get("machine")
            .map_or(Some(RuntimeKey::Local), api::machine_from_json);
        let text = |key: &str| args.get(key).and_then(|value| value.as_str());
        let Some(machine) = machine else {
            return;
        };
        if let Some(agent_id) = text("create_for_agent") {
            self.add_automation_for_agent(machine, agent_id.to_string(), window, cx);
        } else if let Some(automation_id) = text("automation") {
            let automation = AutomationRef::new(machine, automation_id);
            match text("run") {
                Some(run_id) => {
                    self.navigate_to_automation_run(automation, run_id.to_string(), window, cx);
                }
                None => {
                    self.selected_run_id = None;
                    self.live = None;
                    self.route = Some(automation.clone());
                    self.open_automation_editor(automation, window, cx);
                }
            }
            self.reconcile(window, cx);
        }
        cx.notify();
    }
}

impl Focusable for AutomationsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AutomationsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .child(self.render_automations_page(window, cx))
    }
}

/// 通知点进来、打开那次运行的参数。
pub(crate) fn run_args(
    machine: &RuntimeKey,
    automation_id: &str,
    run_id: &str,
) -> api::serde_json::Value {
    api::serde_json::json!({
        "machine": api::machine_to_json(machine),
        "automation": automation_id,
        "run": run_id,
    })
}

/// 挂着的机器，本机在前。
pub(crate) fn machine_keys(cx: &App) -> Vec<RuntimeKey> {
    api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect()
}

pub(crate) fn machine_name(machine: &RuntimeKey, cx: &App) -> String {
    api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|known| known.key == *machine)
        .map_or_else(
            || match machine {
                RuntimeKey::Local => "本机".to_string(),
                RuntimeKey::Remote(id) => id.clone(),
            },
            |known| known.name,
        )
}

/// 机器连不上、已经移除时给人看的话。
pub(crate) fn machine_unavailable(machine: &RuntimeKey, cx: &App) -> Option<String> {
    match api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|known| known.key == *machine)
    {
        Some(known) => known.unavailable,
        None => Some(api::MACHINE_REMOVED.to_string()),
    }
}

/// 那台机器的运行时客户端。
pub(crate) fn client_for(
    machine: &RuntimeKey,
    cx: &App,
) -> Result<api::client::RuntimeClient, String> {
    api::host(cx)
        .client(machine, cx)
        .ok_or_else(|| api::MACHINE_REMOVED.to_string())
}

/// 那台机器上的智能体；配置还没读到是空的。
pub(crate) fn agents_on<'a>(machine: &RuntimeKey, cx: &'a App) -> &'a [AgentDefinition] {
    api::host(cx)
        .agent_settings(machine, cx)
        .map(|settings| settings.agents.as_slice())
        .unwrap_or_default()
}

pub(crate) fn agent_definition<'a>(
    machine: &RuntimeKey,
    agent_id: &str,
    cx: &'a App,
) -> Option<&'a AgentDefinition> {
    agents_on(machine, cx)
        .iter()
        .find(|agent| agent.id == agent_id)
}

/// 去智能体页，停在那台机器上。
pub(crate) fn open_agents_view(machine: &RuntimeKey, cx: &mut App) {
    let open = api::OpenView {
        view: AGENTS_VIEW_ID.to_string(),
        args: api::serde_json::json!({ "machine": api::machine_to_json(machine) }),
    };
    let task = api::OPEN_VIEW.run(&open, cx);
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[automations] 打开智能体页失败：{error}");
        }
    })
    .detach();
}

/// 一次性的系统通知（「已创建」这类），不挂着等撤。
pub(crate) fn notify_once(body: String, cx: &mut App) {
    let notify = api::Notify {
        id: None,
        subtitle: "完成".to_string(),
        body,
        on_click: None,
    };
    let task = api::NOTIFY.run(&notify, cx);
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[automations] 发通知失败：{error}");
        }
    })
    .detach();
}
