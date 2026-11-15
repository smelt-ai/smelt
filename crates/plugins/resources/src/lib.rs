//! 资源占用：设置「系统 → 资源占用」那一页。
//!
//! 桌面进程（GPUI）在本进程里量；会话都跑在运行时那边，由运行时用 `resource.usage` 报它
//! 自己和每个会话的整棵进程树（见 smeltd `resource_usage`）。远程机器上的会话就在那台
//! 机器上量，这里按工作区目录把守护会话对上对话（终端分屏的几格合成一行）。
//!
//! 只在这一页看得见时采样（[`api::Host::settings_page_shown`]），每 [`monitor::INTERVAL`]
//! 一轮；看不见时循环睡在唤醒信号上，一次也不量。采样循环归页面：设置窗口关了页面放掉，
//! 循环跟着停。曲线只放内存、不落库：它是此刻的观察，不是业务数据。
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
use api::sessions::{self, Activate};
use api::{HostEvent, RuntimeKey, SettingsGroup, SettingsPageSpec};
use gpui::{
    App, AppContext as _, AsyncApp, Context, IntoElement, Render, SharedString, Subscription, Task,
    WeakEntity, Window,
};
use gpui_component::{IconName, IconNamed as _};
use smelt_plugin_api as api;

use monitor::{
    CALL_TIMEOUT, DesktopSample, INTERVAL, PRIME_DELAY, PanelSnapshot, ResourceMonitor,
    RuntimeResults, SortBy,
};

/// 本插件的来源 id。
const OWNER: &str = "smelt.resources";
/// 登记的那一页，`<来源>/<名字>`。
const PAGE: &str = "smelt.resources/main";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    let spec = SettingsPageSpec {
        name: "main".into(),
        title: "资源占用".into(),
        icon: IconName::MemoryStick.path(),
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
        ]
        .into_iter()
        .map(SharedString::from)
        .collect(),
        build: Rc::new(|_, cx| cx.new(ResourcesPage::new).into()),
    };
    if let Err(error) = api::register_settings_pages(OWNER, vec![spec], cx) {
        eprintln!("[resources] 设置页登记失败：{error}");
    }
}

/// 设置窗口里的资源页。
pub(crate) struct ResourcesPage {
    monitor: ResourceMonitor,
    /// 画这一页要的全部内容，采到一轮、会话或机器变了时拼好，render 只读它。
    snapshot: Rc<PanelSnapshot>,
    sort: SortBy,
    /// 展开了明细的行（按行 id 记）。
    expanded: Rc<HashSet<SharedString>>,
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
                HostEvent::ShownSettingsPageChanged => {
                    let _ = page.wake.try_send(());
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

impl Render for ResourcesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = view::page(
            cx.entity(),
            self.snapshot.clone(),
            self.sort,
            self.expanded.clone(),
        );
        api::settings_pane("resources-settings", page)
    }
}

/// 这一页此刻看得见：设置窗口开着、停在这一页。
fn shown(cx: &App) -> bool {
    api::host(cx).settings_page_shown(PAGE, cx)
}

/// 从设置窗口跳回主窗口的那段会话。
pub(crate) fn jump_to_session(session_id: String, cx: &mut App) {
    let task = sessions::ACTIVATE.run(&Activate { session_id }, cx);
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
        let Ok(calls) = this.update(cx, |_, cx| shown(cx).then(|| start_calls(cx))) else {
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
            // 只当垫底，隔一小会再采第一轮真的；上次看到的数和曲线也都作废。
            cx.background_executor().timer(PRIME_DELAY).await;
            if this.update(cx, |page, _| page.monitor.reset()).is_err() {
                break;
            }
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
