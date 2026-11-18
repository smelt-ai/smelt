//! 资源占用：设置「系统 → 资源占用」那一页。
//!
//! 桌面进程（GPUI）在本进程里量；会话都跑在运行时那边，由运行时用 `resource.usage` 报它
//! 自己和每个会话的整棵进程树（见 smeltd `resource_usage`）。远程机器上的会话就在那台
//! 机器上量，这里按工作区目录把守护会话对上对话（终端分屏的几格合成一行）。
//!
//! 只在这一页看得见时采样（[`api::Host::view_shown`]），每 [`monitor::INTERVAL`]
//! 一轮；看不见时循环睡在唤醒信号上，一次也不量。采样循环归页面，页面的登记撤下时跟着
//! 停。曲线只放内存、不落库：它是此刻的观察，不是业务数据。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

mod monitor;
mod view;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use api::client::protocol::control::EmptyParams;
use api::model::control_api::ResourceUsage;
use api::process_usage::ProcessSampler;
use api::ui::IconName;
use api::{HostEvent, PluginView, RuntimeKey, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec};
use gpui::{
    App, AppContext as _, AsyncApp, Context, IntoElement, Render, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use smelt_plugin_api::{self as api, text};

use monitor::{
    CALL_TIMEOUT, DesktopSample, INTERVAL, PRIME_DELAY, PanelSnapshot, ResourceMonitor,
    RuntimeResults, SortBy,
};

rust_i18n::i18n!("locales", fallback = "en");

/// 本插件的来源 id。
const OWNER: &str = "smelt.resources";
/// 登记的那一页，`<来源>/<名字>`。
const PAGE: &str = "smelt.resources/main";

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。
const ACTIVATE: api::Command<Activate, ()> = api::Command::new("smelt.sessions/activate");

/// [`ACTIVATE`] 的参数：哪台机器上的哪段守护会话。
#[derive(serde::Serialize)]
struct Activate {
    #[serde(with = "api::machine_json")]
    machine: RuntimeKey,
    session_id: String,
}

/// 内置插件的入口：宿主启动时按它登记（见 [`smelt_plugin_api::BuiltinPlugin`]）。
pub const PLUGIN: smelt_plugin_api::BuiltinPlugin = smelt_plugin_api::BuiltinPlugin {
    id: OWNER,
    register,
};

/// 宿主启动时调一次。
fn register(cx: &mut App) {
    let spec = ViewSpec {
        name: "main".into(),
        title: text!("page.title"),
        icon: IconName::MemoryStick.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: [
                "资源",
                "性能",
                "内存",
                "内存占用",
                "CPU",
                "占用",
                "卡",
                "慢",
                "活动监视器",
                "GPUI",
                "resources",
                "performance",
                "memory",
                "slow",
                "Activity Monitor",
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(|_, cx| Rc::new(cx.new(ResourcesPage::new)) as Rc<dyn ViewHandle>),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        eprintln!("[resources] 设置页登记失败：{error}");
    }
}

/// 设置里的资源页。
pub(crate) struct ResourcesPage {
    monitor: ResourceMonitor,
    /// 画这一页要的全部内容，采到一轮、会话或机器变了时拼好，render 只读它。
    snapshot: Rc<PanelSnapshot>,
    sort: SortBy,
    /// 展开了明细的行（按行 id 记）。
    expanded: Rc<HashSet<SharedString>>,
    /// 上次收到可见性变化时这一页看不看得见。事件不分是谁变了（工作台里切个标签也发），
    /// 只有这一页自己翻转了才叫醒采样循环，不然每次都多采一轮。
    visible: bool,
    wake: smol::channel::Sender<()>,
    _sampler: Task<()>,
    _events: Subscription,
}

impl ResourcesPage {
    fn new(cx: &mut Context<Self>) -> Self {
        let (wake, wake_rx) = smol::channel::unbounded::<()>();
        let events = cx.subscribe(&api::events(cx), |page, _, event: &HostEvent, cx| {
            match event {
                // 露出来要叫醒采样循环；藏起来也叫醒，循环才能确认看不见了，马上睡下，
                // 不用再等一轮。
                HostEvent::ShownViewsChanged => {
                    let visible = shown(cx);
                    if visible != page.visible {
                        page.visible = visible;
                        let _ = page.wake.try_send(());
                    }
                }
                // 会话改了名、开了关了，机器加了减了：行名和分组跟着变，数等下一轮。
                HostEvent::SessionsChanged | HostEvent::MachinesChanged if shown(cx) => {
                    page.rebuild(cx);
                }
                _ => {}
            }
        });
        let sampler = cx.spawn(async move |this, cx| sample_loop(this, wake_rx, cx).await);
        let mut page = Self {
            monitor: ResourceMonitor::default(),
            snapshot: Rc::default(),
            sort: SortBy::default(),
            expanded: Rc::default(),
            visible: shown(cx),
            wake,
            _sampler: sampler,
            _events: events,
        };
        page.rebuild(cx);
        page
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.snapshot = Rc::new(self.monitor.snapshot(cx));
        cx.notify();
    }

    pub(crate) fn set_sort(&mut self, sort: SortBy, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.notify();
    }

    pub(crate) fn toggle_row(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        let expanded = Rc::make_mut(&mut self.expanded);
        if !expanded.remove(id) {
            expanded.insert(id.clone());
        }
        cx.notify();
    }
}

impl PluginView for ResourcesPage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for ResourcesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        view::page(
            cx.entity(),
            self.snapshot.clone(),
            self.sort,
            self.expanded.clone(),
        )
    }
}

/// 这一页此刻看得见：设置开着、停在这一页。
fn shown(cx: &App) -> bool {
    api::host(cx).view_shown(PAGE, cx)
}

/// 从设置里的这一页跳到那段会话。
pub(crate) fn jump_to_session(machine: RuntimeKey, session_id: String, cx: &mut App) {
    let task = ACTIVATE.run(
        &Activate {
            machine,
            session_id,
        },
        cx,
    );
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[resources] 跳到会话失败：{error}");
        }
    })
    .detach();
}

/// 一轮发出去的问询：本机和在线的远程机器，各自已经在后台跑着。
type Calls = Vec<(
    RuntimeKey,
    Task<Result<api::model::control_api::ResourceUsageResult, String>>,
)>;

fn start_calls(cx: &App) -> Calls {
    api::host(cx)
        .machines(cx)
        .into_iter()
        // 本机总要问：它没回话也要说出原因。远程机器离线就不问，那一节写离线的原因。
        .filter(|machine| machine.key == RuntimeKey::Local || machine.unavailable.is_none())
        .map(|machine| {
            let call = api::call::<ResourceUsage>(
                &machine.key,
                EmptyParams::default(),
                CALL_TIMEOUT,
                cx,
            );
            (machine.key, call)
        })
        .collect()
}

/// 采样循环：看得见时每 [`INTERVAL`] 一轮，看不见就睡在唤醒信号上。页面没了循环就停。
async fn sample_loop(
    this: WeakEntity<ResourcesPage>,
    wake: smol::channel::Receiver<()>,
    cx: &mut AsyncApp,
) {
    let mut sampler = ProcessSampler::new();
    let mut showing = false;
    loop {
        // 看不看得见、问哪几台只能在主线程定；问本身在后台跑。
        let Ok(calls) = this.update(cx, |page, cx| {
            let calls = shown(cx).then(|| start_calls(cx));
            if calls.is_some() && !showing {
                // 重新露出来：上次看到的数和曲线作废，马上换掉，不留到第一轮真的采完。
                page.monitor.reset();
                page.rebuild(cx);
            }
            calls
        }) else {
            break;
        };
        let Some(calls) = calls else {
            showing = false;
            if wake.recv().await.is_err() {
                break;
            }
            continue;
        };
        let (returned, desktop, runtimes) = sample_round(calls, sampler, cx).await;
        sampler = returned;
        if !showing {
            // CPU 是跟上一次采样比的，上一次可能是很久以前，比出来是那一大段的平均。这一轮
            // 只当垫底，隔一小会再采第一轮真的。
            cx.background_executor().timer(PRIME_DELAY).await;
            showing = true;
            continue;
        }
        let applied = this.update(cx, |page, cx| {
            page.monitor.apply(desktop, runtimes, Instant::now());
            page.rebuild(cx);
        });
        if applied.is_err() {
            break;
        }
        // 轮询例外：CPU 和内存占用没有「变了」的事件，只能定时采；只在这一页看得见时转，
        // 看不见就睡在唤醒信号上；周期 2s，上限是这一页开着的时长。
        smol::future::race(
            async {
                let _ = wake.recv().await;
            },
            async {
                smol::Timer::after(INTERVAL).await;
            },
        )
        .await;
        while wake.try_recv().is_ok() {}
    }
}

/// 一轮：桌面自己在后台量，各台机器的问询已经并发跑着。采样器带进去再带回来。
async fn sample_round(
    calls: Calls,
    sampler: ProcessSampler,
    cx: &mut AsyncApp,
) -> (ProcessSampler, Option<DesktopSample>, RuntimeResults) {
    let (sampler, desktop) = cx
        .background_executor()
        .spawn(async move {
            let mut sampler = sampler;
            let desktop = monitor::sample_desktop(&mut sampler);
            (sampler, desktop)
        })
        .await;
    let mut runtimes = Vec::with_capacity(calls.len());
    for (key, call) in calls {
        runtimes.push((key, call.await));
    }
    (sampler, desktop, runtimes)
}
