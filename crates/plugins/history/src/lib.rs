//! 历史：当前项目下各家 agent 存在那台机器上的历史会话。看内容、续接（同一家接着聊）、
//! 迁移（把对话重述给别家）、改名、删除。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。存档的扫描、解析、改名、删除都在
//! 运行时（项目在哪台机器就问哪台）；开会话用宿主的 `smelt.sessions/*` 命令。
//!
//! 什么时候重读列表，全靠事件：露出来、换项目、换来源标签、运行时推 `history.changed`
//! （改名、删除、记下会话归属之后）、重新连上、智能体配置变了，还有手动刷新。agent 自己
//! 写新存档没有推送，所以露出来时超过一段时间才重读，不在后台定时扫。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use api::client::protocol::history::{
    HistoryDeleteCall, HistoryDeleteParams, HistoryDetailCall, HistoryDetailParams,
    HistoryListCall, HistoryListParams,
};
use api::model::agent_kind::{ConversationAgentKind, HistorySourceKind};
use api::model::daemon_state::CORE_TOPIC_HISTORY_CHANGED;
use api::model::session_history::{SessionDetail, SessionSummary};
use api::serde_json;
use api::sessions::{self, Handoff, Resume};
use gpui::*;
use gpui_component::WindowExt;
use gpui_component::input::{InputEvent, InputState};
use smelt_plugin_api::{
    self as api, HostEvent, PluginView, Project, RuntimeKey, ViewHandle, ViewSpec,
};

mod model;
mod render;

use model::*;

const VIEW_ID: &str = "history";

/// 选中的来源多久没读就在露出来时重读。
const CURRENT_MAX_AGE: Duration = Duration::from_secs(10);
/// 没选中的来源只决定标签显不显示，放慢：Codex 的列表要把所有项目的会话扫一遍。
const PREFETCH_MAX_AGE: Duration = Duration::from_secs(60);

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_view(
        ViewSpec {
            id: VIEW_ID.into(),
            title: "历史".into(),
            icon: gpui_kit_assets::IconName::Clock.path(),
            per_machine: false,
            draws_top_bar: false,
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| HistoryPage::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        },
        cx,
    )
    .expect("历史的视图 id 不是宿主保留的");
}

/// 一份读到的列表。
struct CachedList {
    machine: RuntimeKey,
    /// 上次读到的时间；`None` 是已经过时（运行时说变了），下次一露出来就重读。
    fetched: Option<Instant>,
    snapshot: ListSnapshot,
    /// resume id → 智能体定义 id（运行时随列表给的）。
    bindings: Rc<HashMap<String, String>>,
}

/// 打开着的那份历史会话，和各轮正文的 Markdown 解析状态。解析状态跟着它一起换、一起丢。
struct OpenedHistory {
    path: String,
    detail: Rc<SessionDetail>,
    turn_markdown: Rc<render::TurnMarkdown>,
}

/// 改哪条历史的名字：运行时按这几样找到它。
struct TitleTarget {
    machine: RuntimeKey,
    agent: HistorySourceKind,
    profile_id: Option<String>,
    cwd: String,
    resume_id: String,
}

/// 正在改名的那条。
struct Renaming {
    target: TitleTarget,
    current_title: String,
    input: Entity<InputState>,
    _enter: Subscription,
}

pub struct HistoryPage {
    project: Option<Project>,
    /// 项目是智能体的 space 时它的引擎：只看这一家，只按智能体续接。
    engine: Option<ConversationAgentKind>,
    /// 用户点选的来源标签（智能体 space 里不看它）。
    picked: (HistorySourceKind, Option<String>),
    lists: HashMap<String, CachedList>,
    inflight: HashSet<String>,
    /// 读的途中运行时说又变了：读回来先不算新鲜，再读一遍。
    invalidated: HashSet<String>,
    detail: Option<OpenedHistory>,
    /// 点一次会话加一：后点的覆盖先点的，晚到的结果丢掉。
    detail_gen: u64,
    detail_list: ListState,
    filter: Entity<InputState>,
    /// 上次知道的是否看得见：从看不见变成看得见才重读。
    shown: bool,
    renaming: Option<Renaming>,
    _subscriptions: Vec<Subscription>,
}

impl HistoryPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称或原始标题…"));
        let events = api::events(cx);
        let subscriptions = vec![
            cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
                this.on_host_event(event, cx);
            }),
            // 过滤在内存里做，键入只要重画。
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut page = Self {
            project: None,
            engine: None,
            picked: (ConversationAgentKind::Claude.into(), None),
            lists: HashMap::new(),
            inflight: HashSet::new(),
            invalidated: HashSet::new(),
            detail: None,
            detail_gen: 0,
            detail_list: ListState::new(0, ListAlignment::Top, px(1000.)),
            filter,
            shown: api::host(cx).view_shown(VIEW_ID, cx),
            renaming: None,
            _subscriptions: subscriptions,
        };
        page.sync_project(cx);
        // 布局恢复时建出来就看得见：不会再有「从看不见变成看得见」，这里就得读。
        page.refresh(cx);
        page
    }

    fn on_host_event(&mut self, event: &HostEvent, cx: &mut Context<Self>) {
        match event {
            HostEvent::ShownViewsChanged => {
                let shown = api::host(cx).view_shown(VIEW_ID, cx);
                let appeared = shown && !self.shown;
                self.shown = shown;
                if appeared {
                    self.refresh(cx);
                }
            }
            HostEvent::ActiveProjectChanged
            | HostEvent::AgentSettingsChanged
            | HostEvent::MachinesChanged => {
                self.sync_project(cx);
                self.refresh(cx);
                cx.notify();
            }
            HostEvent::RuntimeTopic { machine, topic } if topic == CORE_TOPIC_HISTORY_CHANGED => {
                self.mark_stale(machine, cx);
            }
            HostEvent::Reconnected(machine) => self.mark_stale(machine, cx),
            _ => {}
        }
    }

    /// 重读当前项目和它是不是智能体的 space。换了项目，右边的详情清掉。
    fn sync_project(&mut self, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let project = host.active_project(cx);
        self.engine = project.as_ref().and_then(|project| {
            let home = host
                .machines(cx)
                .into_iter()
                .find(|machine| machine.key == project.machine)
                .and_then(|machine| machine.smelt_home);
            space_engine(
                project,
                home.as_deref(),
                host.agent_settings(&project.machine, cx),
            )
        });
        if self.project != project {
            self.project = project;
            self.close_detail();
        }
    }

    /// 这会儿列的是哪个来源。读和画必须用同一个答案，不然读的是 A、标签选的是 B。
    fn source(&self) -> (HistorySourceKind, Option<String>) {
        match self.engine {
            Some(kind) => (kind.into(), None),
            None => self.picked.clone(),
        }
    }

    /// 那台机器的列表都过时了：看得见就马上重读，看不见等露出来再读。
    fn mark_stale(&mut self, machine: &RuntimeKey, cx: &mut Context<Self>) {
        for (key, list) in &mut self.lists {
            if &list.machine == machine {
                list.fetched = None;
                if self.inflight.contains(key) {
                    self.invalidated.insert(key.clone());
                }
            }
        }
        if self
            .project
            .as_ref()
            .is_some_and(|project| &project.machine == machine)
        {
            self.refresh(cx);
        }
    }

    /// 手动刷新：这个项目所在机器上的列表全部当作过时，马上重读。
    fn refresh_now(&mut self, cx: &mut Context<Self>) {
        if let Some(project) = self.project.clone() {
            self.mark_stale(&project.machine, cx);
        }
    }

    /// 看得见时把当前来源读新，其余来源慢一点读，好决定哪些标签该显示。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let current = self.source();
        let tabs = {
            let host = api::host(cx);
            let settings = host.agent_settings(&project.machine, cx);
            source_tabs(
                self.engine,
                settings.into_iter().flat_map(|settings| &settings.profiles),
            )
        };
        for tab in tabs {
            if (tab.kind, tab.profile_id.as_ref()) != (current.0, current.1.as_ref()) {
                self.ensure_list(&project, tab.kind, tab.profile_id, PREFETCH_MAX_AGE, cx);
            }
        }
        self.ensure_list(&project, current.0, current.1, CURRENT_MAX_AGE, cx);
    }

    fn ensure_list(
        &mut self,
        project: &Project,
        agent: HistorySourceKind,
        profile_id: Option<String>,
        max_age: Duration,
        cx: &mut Context<Self>,
    ) {
        let key = list_key(
            &project.machine,
            agent,
            profile_id.as_deref(),
            &project.root,
        );
        let fresh = self
            .lists
            .get(&key)
            .and_then(|list| list.fetched)
            .is_some_and(|fetched| fetched.elapsed() < max_age);
        if fresh || !self.inflight.insert(key.clone()) {
            return;
        }
        let params = HistoryListParams {
            agent: agent.id().to_string(),
            profile_id,
            cwd: project.root.clone(),
        };
        let call = api::call::<HistoryListCall>(&project.machine, params, api::CALL_TIMEOUT, cx);
        let project = project.clone();
        cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                this.inflight.remove(&key);
                let stale = this.invalidated.remove(&key);
                let (snapshot, bindings) = match result {
                    Ok(result) => (
                        ListSnapshot::new(
                            result.sessions.into_iter().map(summary_from_wire).collect(),
                        ),
                        Rc::new(result.agent_definition_ids.into_iter().collect()),
                    ),
                    // 读不到（运行时没起、版本旧）：有旧的留着旧的，没有就当没历史。
                    Err(error) => {
                        eprintln!("[history] 读取历史会话失败：{error}");
                        match this.lists.remove(&key) {
                            Some(old) => (old.snapshot, old.bindings),
                            None => (ListSnapshot::new(Vec::new()), Rc::default()),
                        }
                    }
                };
                this.lists.insert(
                    key,
                    CachedList {
                        machine: project.machine.clone(),
                        fetched: (!stale).then(Instant::now),
                        snapshot,
                        bindings,
                    },
                );
                if stale {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_source(
        &mut self,
        kind: HistorySourceKind,
        profile_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.picked == (kind, profile_id.clone()) {
            return;
        }
        self.picked = (kind, profile_id);
        self.close_detail();
        self.refresh(cx);
        cx.notify();
    }

    fn close_detail(&mut self) {
        self.detail_gen = self.detail_gen.wrapping_add(1);
        self.detail = None;
        self.detail_list.reset(0);
    }

    /// 点开一份会话：运行时解析存档，回来时还是最后点的那份才显示。
    fn open_detail(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        self.close_detail();
        let generation = self.detail_gen;
        let (agent, _) = self.source();
        let call = load_detail(&project.machine, agent, &path, cx);
        cx.spawn(async move |this, cx| {
            let detail = call.await;
            this.update(cx, |this, cx| {
                if this.detail_gen != generation {
                    return;
                }
                match detail {
                    Ok(Some(detail)) => {
                        this.detail_list.reset(detail.turns.len());
                        this.detail = Some(OpenedHistory {
                            path,
                            detail: Rc::new(detail),
                            turn_markdown: Rc::default(),
                        });
                    }
                    Ok(None) => eprintln!("[history] 这份存档读不出内容：{path}"),
                    Err(error) => eprintln!("[history] 读取会话内容失败：{error}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 续接：同一家 agent 接着这段会话聊。`in_terminal` 是起它的 CLI。
    fn resume(&self, session: &SessionSummary, in_terminal: bool, cx: &mut App) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let (agent, profile_id) = self.source();
        let agent_definition_id = self
            .bindings_for_current(cx)
            .get(&session.resume_id)
            .cloned();
        let args = Resume {
            machine: project.machine.clone(),
            agent: agent.id().to_string(),
            profile_id,
            cwd: project.root.clone(),
            resume_id: session.resume_id.clone(),
            agent_definition_id,
        };
        let command = if in_terminal {
            sessions::RESUME_IN_TERMINAL
        } else {
            sessions::RESUME
        };
        report_failure(command.run(&args, cx), cx);
    }

    /// 当前列表里、这台机器上还认得的「会话 → 智能体定义」。
    fn bindings_for_current(&self, cx: &App) -> HashMap<String, String> {
        let Some(project) = self.project.as_ref() else {
            return HashMap::new();
        };
        let (agent, profile_id) = self.source();
        let key = list_key(
            &project.machine,
            agent,
            profile_id.as_deref(),
            &project.root,
        );
        let Some(list) = self.lists.get(&key) else {
            return HashMap::new();
        };
        known_bindings(
            &list.bindings,
            api::host(cx).agent_settings(&project.machine, cx),
        )
    }

    /// 迁移：读出源会话，写成交接说明，交给目标 agent 起一段新会话。存档可能有几 MB，
    /// 解析在运行时。
    fn hand_off(&self, session: &SessionSummary, target: HandoffTarget, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let (agent, profile_id) = self.source();
        let Some(source_agent) = agent.acp() else {
            return;
        };
        let profile_label = profile_id.as_deref().and_then(|id| {
            api::host(cx)
                .agent_settings(&project.machine, cx)?
                .find_profile(id)
                .map(|profile| profile.label.clone())
        });
        let source = HandoffSource {
            agent: source_agent,
            profile_label,
            title: session.title.clone(),
            cwd: project.root.clone(),
        };
        let resume_id = session.resume_id.clone();
        let path = session.path.clone();
        let call = load_detail(&project.machine, agent, &path, cx);
        cx.spawn(async move |_, cx| {
            let detail = call.await;
            cx.update(|cx| {
                let detail = match detail {
                    Ok(Some(detail)) => detail,
                    // 存档被删、换了格式：明说读不出来，别开一段空会话让人猜。
                    Ok(None) => {
                        return notify_error(&format!("读不出这条历史会话，迁移取消：{path}"), cx);
                    }
                    Err(error) => {
                        return notify_error(&format!("读取历史会话失败，迁移取消：{error}"), cx);
                    }
                };
                let args = Handoff {
                    machine: project.machine.clone(),
                    cwd: source.cwd.clone(),
                    agent: target.agent.id().to_string(),
                    profile_id: target.profile_id.clone(),
                    prompt: handoff_prompt(&source, &detail, &target),
                    source_session_id: resume_id,
                };
                report_failure(sessions::HANDOFF.run(&args, cx), cx);
            });
        })
        .detach();
    }

    /// 改名：弹框，回车或点确定提交。名字存在运行时，改完它推 `history.changed`，列表重读。
    fn start_rename(
        &mut self,
        session: &SessionSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.title_target(session) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(session.title.clone()));
        let enter = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_rename(cx);
                window.close_dialog(cx);
            }
        });
        self.renaming = Some(Renaming {
            target,
            current_title: session.title.clone(),
            input: input.clone(),
            _enter: enter,
        });
        let page = cx.entity();
        let dialog_input = input.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let on_ok = page.clone();
            let on_close = page.clone();
            alert
                .confirm()
                .title("重命名")
                .child(gpui_component::input::Input::new(&dialog_input))
                .ok_text("确定")
                .on_ok(move |_, _, cx| {
                    on_ok.update(cx, |page, cx| page.submit_rename(cx));
                    true
                })
                .on_close(move |_, _, cx| {
                    on_close.update(cx, |page, _| page.renaming = None);
                })
        });
        window.defer(cx, move |window, cx| {
            // 名字整段选中：直接打字就是换掉，按方向键才是在原名上改。
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        });
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let text = renaming.input.read(cx).value();
        let Some(title) = submitted_title(&renaming.current_title, &text) else {
            return;
        };
        set_title(renaming.target, title, cx);
    }

    fn title_target(&self, session: &SessionSummary) -> Option<TitleTarget> {
        let project = self.project.as_ref()?;
        let (agent, profile_id) = self.source();
        Some(TitleTarget {
            machine: project.machine.clone(),
            agent,
            profile_id,
            cwd: project.root.clone(),
            resume_id: session.resume_id.clone(),
        })
    }

    /// 「恢复默认名称」：清掉自定义名字。
    fn reset_title(&self, session: &SessionSummary, cx: &App) {
        if let Some(target) = self.title_target(session) {
            set_title(target, None, cx);
        }
    }

    /// 删除先确认：会永久删掉那台机器上的存档。
    fn confirm_delete(
        &self,
        session: &SessionSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let (agent, profile_id) = self.source();
        let params = HistoryDeleteParams {
            agent: agent.id().to_string(),
            profile_id,
            cwd: project.root.clone(),
            resume_id: session.resume_id.clone(),
            path: session.path.clone(),
        };
        let description = format!(
            "将永久删除 {} 的「{}」及其本地对话记录，此操作不可撤销。",
            agent.short_label(),
            session.title
        );
        let title = session.title.clone();
        let page = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let page = page.clone();
            let params = params.clone();
            let machine = project.machine.clone();
            let title = title.clone();
            alert
                .confirm()
                .title("确定删除这条历史会话吗？")
                .description(description.clone())
                .ok_text("确定删除")
                .ok_variant(gpui_component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    page.update(cx, |page, cx| {
                        page.delete(&machine, params.clone(), title.clone(), cx);
                    });
                    true
                })
        });
    }

    /// 删掉。运行时确认存档确实在这个项目的列表里才删，删完推 `history.changed`。
    fn delete(
        &mut self,
        machine: &RuntimeKey,
        params: HistoryDeleteParams,
        title: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .detail
            .as_ref()
            .is_some_and(|opened| opened.path == params.path)
        {
            self.close_detail();
            cx.notify();
        }
        let call = api::call::<HistoryDeleteCall>(machine, params, api::CALL_TIMEOUT, cx);
        cx.spawn(async move |_, cx| {
            let result = call.await;
            cx.update(|cx| match result {
                Ok(_) => notify("完成", &format!("已删除历史会话「{title}」"), cx),
                Err(error) => notify_error(&format!("删除历史会话失败：{error}"), cx),
            });
        })
        .detach();
    }
}

impl PluginView for HistoryPage {
    fn open(&mut self, _args: &serde_json::Value, _window: &mut Window, cx: &mut Context<Self>) {
        // 打开就是要看：不等「看得见的视图变了」那条事件。
        self.shown = true;
        self.sync_project(cx);
        self.refresh(cx);
        cx.notify();
    }
}

/// 改名字存在运行时；改完它推 `history.changed`，列表跟着重读。`None` 是恢复默认名称。
fn set_title(target: TitleTarget, title: Option<String>, cx: &App) {
    let client = api::host(cx).client(&target.machine, cx);
    let rename = cx.background_spawn(async move {
        let client = client.ok_or_else(|| api::MACHINE_REMOVED.to_string())?;
        api::session_ops::rename_history_title(
            &client,
            target.agent,
            target.profile_id.as_deref(),
            &target.resume_id,
            title.as_deref(),
            Some(&target.cwd),
        )
    });
    cx.spawn(async move |cx| {
        if let Err(error) = rename.await {
            cx.update(|cx| notify_error(&format!("改名失败：{error}"), cx));
        }
    })
    .detach();
}

/// 向运行时要一份会话的对话内容。存档没了、认不出格式是 `Ok(None)`。
fn load_detail(
    machine: &RuntimeKey,
    agent: HistorySourceKind,
    path: &str,
    cx: &App,
) -> Task<Result<Option<SessionDetail>, String>> {
    let params = HistoryDetailParams {
        agent: agent.id().to_string(),
        path: path.to_string(),
    };
    let call = api::call::<HistoryDetailCall>(machine, params, api::CALL_TIMEOUT, cx);
    cx.background_spawn(async move { Ok(call.await?.detail.map(detail_from_wire)) })
}

/// 等宿主命令跑完，失败了报出来（命令自己不报）。
fn report_failure(ran: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| notify_error(&error, cx));
        }
    })
    .detach();
}

fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[history] {message}");
    notify("错误", message, cx);
}

fn notify(subtitle: &str, body: &str, cx: &mut App) {
    let notify = api::Notify {
        subtitle: subtitle.to_string(),
        body: body.to_string(),
    };
    api::NOTIFY.run(&notify, cx).detach();
}
