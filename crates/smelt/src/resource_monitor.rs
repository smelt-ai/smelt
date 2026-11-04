//! 资源占用：设置「Agent 与运行 → 资源占用」那一页。
//!
//! 桌面进程（GPUI）在本进程里量；会话都跑在运行时那边，由运行时用 `resource.usage` 报它
//! 自己和每个会话的整棵进程树（见 smeltd `resource_usage`）。远程机器上的会话就在那台
//! 机器上量，桌面只按会话 id 对上侧栏里的会话。
//!
//! 只在设置窗口开着、停在这一页时采样，每 [`INTERVAL`] 一轮；看不见时循环睡在唤醒信号上，
//! 一次也不量。结果缓存在 Workspace 上，设置快照从缓存拼，render 只读快照。曲线只放内存、
//! 不落库：它是此刻的观察，不是业务数据。

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::chart::AreaChart;
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_component::{ActiveTheme as _, Icon, IconName, StyledExt as _, h_flex, v_flex};
use smelt_model::control_api::{
    LocalModelInferenceStats, LocalModelUsage, ProcessEntry, ProcessUsage, ResourceUsage,
    ResourceUsageResult, SessionResourceUsage,
};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::EmptyParams;
use smelt_sys::process_usage::{MemoryBreakdown, ProcessSampler};
use smelt_ui::IconAction as _;
use smelt_ui::truncated_line::truncated_line;
use smelt_ui::{Button, ListRow};

use crate::runtimes::RuntimeHub;
use crate::settings::{SettingsSection, SettingsWindowHandle};
use crate::{RuntimeKey, SessionKind, Workspace, ui_theme};

/// 两轮采样的间隔。
const INTERVAL: Duration = Duration::from_secs(2);
/// 刚开始看时先垫一轮，隔这么久再采第一轮真的（见 `start_resource_monitor`）。
const PRIME_DELAY: Duration = Duration::from_millis(500);
/// 一台机器回得慢不拖别的机器（各自并发），但也不能一直等。
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// 曲线的点数；点距是 [`INTERVAL`]，一共两分钟。
const CHART_POINTS: usize = 60;

type RuntimeResults = Vec<(RuntimeKey, Result<ResourceUsageResult, String>)>;

/// 采到的最新一轮结果和本机内存的历史。
#[derive(Default)]
pub(crate) struct ResourceMonitor {
    /// 桌面进程自己。
    desktop: Option<ProcessUsage>,
    /// 桌面进程的内存拆成哪几块。只有 macOS 有。
    desktop_memory: Option<MemoryBreakdown>,
    /// 这一轮问到的机器，本机在前。
    runtimes: RuntimeResults,
    /// 本机合计内存的样本，老的在前。
    history: VecDeque<(Instant, u64)>,
    /// 曲线上的点，收到样本时算好。
    chart: Vec<f64>,
    wake_tx: Option<smol::channel::Sender<()>>,
}

impl ResourceMonitor {
    fn result(&self, key: &RuntimeKey) -> Option<&Result<ResourceUsageResult, String>> {
        self.runtimes
            .iter()
            .find(|(runtime, _)| runtime == key)
            .map(|(_, result)| result)
    }

    /// 本机合计：桌面进程、本机守护、本机会话和本机模型进程。本机运行时没回话时只有桌面。
    fn local_total(&self) -> Option<ProcessUsage> {
        let mut total = self.desktop?;
        if let Some(Ok(local)) = self.result(&RuntimeKey::Local) {
            add(&mut total, runtime_total(local));
        }
        Some(total)
    }

    /// 重新开始看：上次看到的数和曲线都作废。
    fn reset(&mut self) {
        self.desktop = None;
        self.desktop_memory = None;
        self.runtimes.clear();
        self.history.clear();
        self.chart.clear();
    }

    fn apply(&mut self, desktop: Option<ProcessUsage>, runtimes: RuntimeResults, now: Instant) {
        self.desktop = desktop;
        self.runtimes = runtimes;
        if let Some(total) = self.local_total() {
            self.history.push_back((now, total.memory_bytes));
        }
        // 比曲线左端再早一点的样本还得留着：左端那一刻取的是它。
        let keep = INTERVAL * (CHART_POINTS as u32 + 1);
        while self.history.len() > 1
            && self
                .history
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > keep)
        {
            self.history.pop_front();
        }
        self.chart = memory_series(&self.history, now);
    }
}

impl Workspace {
    /// 启动采样循环。Workspace 没了循环就停。
    pub(crate) fn start_resource_monitor(&mut self, cx: &mut Context<Self>) {
        if self.resource_monitor.wake_tx.is_some() {
            return;
        }
        let (wake_tx, wake_rx) = smol::channel::unbounded::<()>();
        self.resource_monitor.wake_tx = Some(wake_tx);
        cx.spawn(async move |this, cx| {
            let mut sampler = ProcessSampler::new();
            let mut showing = false;
            loop {
                // 问哪几台、看不看得见只能在主线程读；问本身放后台。
                let Ok(targets) = this.update(cx, |ws, cx| {
                    ws.resources_visible(cx).then(|| resource_targets(cx))
                }) else {
                    break;
                };
                let Some(targets) = targets else {
                    showing = false;
                    if wake_rx.recv().await.is_err() {
                        break;
                    }
                    continue;
                };
                if !showing {
                    // CPU 是跟上一次采样比的，上一次可能是很久以前，比出来是那一大段的
                    // 平均。先垫一轮，隔一小会再采第一轮真的。
                    let (returned, _, _) = sample_round(targets.clone(), sampler, cx).await;
                    sampler = returned;
                    cx.background_executor().timer(PRIME_DELAY).await;
                    if this
                        .update(cx, |ws, _| ws.resource_monitor.reset())
                        .is_err()
                    {
                        break;
                    }
                    showing = true;
                }
                let (returned, desktop, runtimes) = sample_round(targets, sampler, cx).await;
                sampler = returned;
                let (desktop, desktop_memory) = desktop.unzip();
                if this
                    .update(cx, |ws, cx| {
                        ws.resource_monitor.desktop_memory = desktop_memory.flatten();
                        ws.resource_monitor.apply(desktop, runtimes, Instant::now());
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                // 轮询例外：CPU 和内存占用没有「变了」的事件，只能定时采；只在设置窗口停在
                // 这一页时转，看不见就睡在唤醒信号上；周期 2s，上限是这一页开着的时长。
                smol::future::race(
                    async {
                        let _ = wake_rx.recv().await;
                    },
                    async {
                        smol::Timer::after(INTERVAL).await;
                    },
                )
                .await;
                while wake_rx.try_recv().is_ok() {}
            }
        })
        .detach();
    }

    /// 资源页可能刚变得看得见（切到这一页、开了设置窗口）：叫醒采样循环去看一眼。
    pub(crate) fn wake_resource_monitor(&self) {
        if let Some(tx) = &self.resource_monitor.wake_tx {
            let _ = tx.try_send(());
        }
    }

    /// 设置窗口开着，而且停在资源页。
    fn resources_visible(&self, cx: &mut App) -> bool {
        self.settings_section == SettingsSection::Resources
            && cx
                .try_global::<SettingsWindowHandle>()
                .map(|handle| handle.0)
                .is_some_and(|handle| crate::window_handle_is_open(&handle, cx))
    }

    /// 从设置窗口跳回主窗口的某个会话，并把主窗口提到前面。
    fn jump_to_session_from_settings(&mut self, session_id: String, cx: &mut Context<Self>) {
        let handle = self.window_handle;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_root, window, cx| {
                let _ = view.update(cx, |ws, cx| {
                    ws.request_notification_session_jump(session_id, window, cx);
                });
                window.activate_window();
            });
        });
    }

    /// 资源页的快照。设置快照在停在这一页时取它。
    pub(crate) fn resources_snapshot(&self, cx: &App) -> Rc<PanelSnapshot> {
        let monitor = &self.resource_monitor;
        let total_memory = match monitor.result(&RuntimeKey::Local) {
            Some(Ok(result)) if result.total_memory_bytes > 0 => Some(result.total_memory_bytes),
            _ => None,
        };
        Rc::new(PanelSnapshot {
            local: monitor.local_total(),
            total_memory,
            chart: monitor.chart.clone(),
            sections: sections(self, cx),
        })
    }
}

/// 桌面进程这一轮的占用和内存拆分。
type DesktopSample = (ProcessUsage, Option<MemoryBreakdown>);

/// 一轮：桌面自己在后台量，各台机器各自并发问。采样器带进去再带回来。
async fn sample_round(
    targets: Vec<(RuntimeKey, RuntimeClient)>,
    mut sampler: ProcessSampler,
    cx: &mut AsyncApp,
) -> (ProcessSampler, Option<DesktopSample>, RuntimeResults) {
    let calls: Vec<_> = targets
        .into_iter()
        .map(|(key, client)| {
            cx.background_executor().spawn(async move {
                let result = crate::runtime_call::call_on::<ResourceUsage>(
                    &client,
                    EmptyParams::default(),
                    CALL_TIMEOUT,
                )
                .map_err(crate::runtime_call::error_text);
                (key, result)
            })
        })
        .collect();
    let (sampler, desktop) = cx
        .background_executor()
        .spawn(async move {
            let desktop = sampler.sample_process(std::process::id()).map(|usage| {
                let memory = smelt_sys::process_usage::own_memory();
                // 拆分和总数取同一刻的，展开后几块加起来正好是这一行。
                let usage = ProcessUsage {
                    cpu_percent: usage.cpu_percent,
                    memory_bytes: memory.map_or(usage.memory_bytes, |memory| memory.total),
                    process_count: usage.processes,
                };
                (usage, memory)
            });
            (sampler, desktop)
        })
        .await;
    let mut runtimes = Vec::with_capacity(calls.len());
    for call in calls {
        runtimes.push(call.await);
    }
    (sampler, desktop, runtimes)
}

/// 这一轮问哪几台：本机和在线的远程机器。
fn resource_targets(cx: &App) -> Vec<(RuntimeKey, RuntimeClient)> {
    let mut targets = vec![(RuntimeKey::Local, crate::runtimes::local())];
    if let Some(hub) = cx.try_global::<RuntimeHub>() {
        for remote in &hub.remotes {
            let key = remote.runtime.key().clone();
            if crate::runtimes::runtime_online(&key, cx) {
                targets.push((key, remote.runtime.client().clone()));
            }
        }
    }
    targets
}

fn add(total: &mut ProcessUsage, usage: ProcessUsage) {
    total.cpu_percent += usage.cpu_percent;
    total.memory_bytes += usage.memory_bytes;
    total.process_count += usage.process_count;
}

/// 一台运行时的合计：守护进程、全部会话和在跑的模型进程。模型是守护起的独立进程，
/// 运行时报的 `daemon` 只算守护自己，不含它，所以这里加一次不会重复。
fn runtime_total(result: &ResourceUsageResult) -> ProcessUsage {
    let mut total = result.daemon;
    for session in &result.sessions {
        add(&mut total, session.usage);
    }
    if let Some(LocalModelUsage::Running { usage }) = result.local_model {
        add(&mut total, usage);
    }
    total
}

/// 本地模型那一行。旧运行时不报这一项（`None`），显示「不可用」而不是「未运行」。
fn local_model_row(model: Option<LocalModelUsage>) -> Row {
    let (detail, value) = match model {
        Some(LocalModelUsage::Running { usage }) => {
            ("独立进程".to_string(), RowValue::Usage(usage))
        }
        Some(LocalModelUsage::NotRunning) => ("空闲时不常驻".to_string(), RowValue::Text("未运行")),
        Some(LocalModelUsage::Unsampled) => ("运行中，这次没采到占用".to_string(), RowValue::Text("采样不可用")),
        None => (
            "这台机器的后台服务版本较旧，不报本地模型".to_string(),
            RowValue::Text("不可用"),
        ),
    };
    Row {
        label: "本地模型".to_string(),
        detail: Some(detail),
        value,
        parts: Vec::new(),
    }
}

/// 本地模型推理统计里的一行：名字一行，数字一行。
#[derive(Debug, PartialEq)]
struct InferenceLine {
    label: String,
    detail: String,
}

/// 推理统计怎么显示：要么一段空状态说明，要么上次 + 各模型平均。
#[derive(Debug, PartialEq)]
enum InferenceView {
    Empty(&'static str),
    Lines(Vec<InferenceLine>),
}

const INFERENCE_NOTE: &str = "耗时从启动模型进程算到拿到结果，含模型加载；CPU 是模型进程所有线程累计的用时，\
     可超过耗时；峰值内存口径同上面的实时占用（macOS 峰值 phys_footprint、Linux VmHWM、\
     Windows 峰值工作集），不含独立显存。只记成功的推理，重启后台服务后清空。";

/// 没数据写「—」；`Some(0)` 是真的 0，照常写。
fn or_dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "—".to_string())
}

/// 微秒写成人读的时长。均值是小数，所以收 f64。
fn format_duration_us(us: f64) -> String {
    let us = us.max(0.);
    if us == 0. {
        "0 ms".to_string()
    } else if us < 1_000. {
        format!("{us:.0} µs")
    } else if us < 1_000_000. {
        format!("{:.0} ms", us / 1_000.)
    } else if us < 60_000_000. {
        format!("{:.1} s", us / 1_000_000.)
    } else {
        let secs = (us / 1_000_000.).round() as u64;
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    }
}

/// 均值；只有部分次有数据时注明几次。
fn format_mean(
    mean: Option<smelt_model::control_api::InferenceMean>,
    inferences: u64,
    format: impl Fn(f64) -> String,
) -> String {
    or_dash(mean.map(|mean| {
        let text = format(mean.mean);
        if mean.samples == inferences {
            text
        } else {
            format!("{text}（{} 次有数据）", mean.samples)
        }
    }))
}

/// 后台服务没报（旧版本）是 `None`；报了但还没成功推理过是空统计。
fn inference_view(stats: Option<&LocalModelInferenceStats>) -> InferenceView {
    let Some(stats) = stats else {
        return InferenceView::Empty("这台机器的后台服务版本较旧，不报推理统计");
    };
    if stats.last.is_none() && stats.averages.is_empty() {
        return InferenceView::Empty("后台服务启动以来还没有成功的本地模型推理");
    }
    let mut lines = Vec::with_capacity(stats.averages.len() + 1);
    if let Some(last) = &stats.last {
        lines.push(InferenceLine {
            label: format!("上次推理 · {}", last.model),
            detail: format!(
                "耗时 {} · CPU {} · 峰值内存 {}",
                format_duration_us(last.wall_time_us as f64),
                or_dash(last.cpu_time_us.map(|us| format_duration_us(us as f64))),
                or_dash(last.peak_memory_bytes.map(format_bytes)),
            ),
        });
    }
    // 每个模型各自一行，不混在一起平均。
    for average in &stats.averages {
        lines.push(InferenceLine {
            label: format!("{} · 成功 {} 次", average.model, average.inferences),
            detail: format!(
                "平均耗时 {} · CPU {} · 峰值内存 {}",
                format_duration_us(average.wall_time_us),
                format_mean(average.cpu_time_us, average.inferences, format_duration_us),
                format_mean(average.peak_memory_bytes, average.inferences, |bytes| {
                    format_bytes(bytes.max(0.).round() as u64)
                }),
            ),
        });
    }
    InferenceView::Lines(lines)
}

/// 曲线的点：从 `now` 往回每隔 [`INTERVAL`] 取一刻，取那一刻之前最近的样本，老的在前。
/// 还没有样本的时刻不出点：刚打开时点少，画出来铺满整宽，只是覆盖的时间短。
fn memory_series(history: &VecDeque<(Instant, u64)>, now: Instant) -> Vec<f64> {
    (0..CHART_POINTS)
        .rev()
        .filter_map(|back| {
            let at = now.checked_sub(INTERVAL * back as u32)?;
            history
                .iter()
                .rev()
                .find(|(sampled, _)| *sampled <= at)
                .map(|(_, bytes)| *bytes as f64)
        })
        .collect()
}

/// 一行会话：侧栏里的一个会话，它在这台运行时上的守护会话合在一起算（终端分屏每个 pane
/// 一个）；或者侧栏之外的会话合成的一行。
#[derive(Debug, PartialEq)]
struct SessionGroup {
    title: String,
    usage: ProcessUsage,
    /// 树里的进程，内存大的在前。运行时每个会话最多报一部分，可能比 `usage.process_count` 少。
    processes: Vec<ProcessEntry>,
    /// 点「跳到会话」去哪个守护会话。侧栏之外的会话没有。
    jump: Option<String>,
}

impl SessionGroup {
    fn absorb(&mut self, session: &SessionResourceUsage) {
        add(&mut self.usage, session.usage);
        self.processes.extend(session.processes.iter().cloned());
        self.processes
            .sort_by_key(|process| std::cmp::Reverse(process.usage.memory_bytes));
    }
}

/// 把运行时报来的会话按侧栏会话归拢。对不上的（自动化、子智能体、别的客户端开的）合成
/// 一行放在最后。
fn group_sessions(
    sessions: &[SessionResourceUsage],
    owner: impl Fn(&str) -> Option<(usize, String)>,
) -> (Vec<SessionGroup>, Option<SessionGroup>) {
    let mut groups: Vec<(usize, SessionGroup)> = Vec::new();
    let mut others: Option<(usize, SessionGroup)> = None;
    let empty = |title: String, jump: Option<String>| SessionGroup {
        title,
        usage: ProcessUsage::default(),
        processes: Vec::new(),
        jump,
    };
    for session in sessions {
        let group = match owner(&session.session_id) {
            Some((ix, title)) => match groups.iter().position(|(owner, _)| *owner == ix) {
                Some(at) => &mut groups[at].1,
                None => {
                    groups.push((ix, empty(title, Some(session.session_id.clone()))));
                    &mut groups.last_mut().expect("刚放进去").1
                }
            },
            None => {
                let (count, group) = others.get_or_insert_with(|| (0, empty(String::new(), None)));
                *count += 1;
                group
            }
        };
        group.absorb(session);
    }
    let others = others.map(|(count, mut group)| {
        group.title = format!("侧栏之外的 {count} 个会话");
        group
    });
    (groups.into_iter().map(|(_, group)| group).collect(), others)
}

/// 「5 个进程：node ×3、bun、rg」。按名字归拢，占内存多的名字在前，最多点四个名。
fn process_summary(count: u32, processes: &[ProcessEntry]) -> String {
    const NAMES: usize = 4;
    let mut names: Vec<(&str, usize)> = Vec::new();
    for process in processes {
        match names.iter_mut().find(|(name, _)| *name == process.name) {
            Some((_, seen)) => *seen += 1,
            None => names.push((process.name.as_str(), 1)),
        }
    }
    if names.is_empty() {
        return format!("{count} 个进程");
    }
    let mut listed = names
        .iter()
        .take(NAMES)
        .map(|(name, seen)| match *seen {
            1 => (*name).to_string(),
            seen => format!("{name} ×{seen}"),
        })
        .collect::<Vec<_>>()
        .join("、");
    if names.len() > NAMES {
        listed.push_str(" 等");
    }
    format!("{count} 个进程：{listed}")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SortBy {
    #[default]
    Memory,
    Cpu,
}

impl SortBy {
    /// 重的在前。
    fn sort<G: Borrow<SessionGroup>>(self, groups: &mut [G]) {
        match self {
            SortBy::Memory => {
                groups.sort_by_key(|group| std::cmp::Reverse(group.borrow().usage.memory_bytes))
            }
            SortBy::Cpu => groups.sort_by(|a, b| {
                let (a, b) = (a.borrow().usage.cpu_percent, b.borrow().usage.cpu_percent);
                b.total_cmp(&a)
            }),
        }
    }
}

/// 资源页按什么排。放在设置窗口的元素状态里，窗口关了就丢。
#[derive(Default)]
struct PanelSort(SortBy);

/// 展开了进程明细的会话行（按行 id 记）。同样放在元素状态里。
#[derive(Default)]
struct ExpandedRows(HashSet<SharedString>);

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.;
    const MB: f64 = KB * 1024.;
    const GB: f64 = MB * 1024.;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else {
        format!("{:.0} KB", bytes / KB)
    }
}

fn format_cpu(percent: f32) -> String {
    if percent < 10. {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.0}%")
    }
}

/// 资源页要显示的全部内容，由 Workspace 从缓存拼好放进设置快照。
pub(crate) struct PanelSnapshot {
    local: Option<ProcessUsage>,
    /// 本机物理内存总量，算占比用。
    total_memory: Option<u64>,
    chart: Vec<f64>,
    sections: Vec<Section>,
}

/// 不跟着排序走的一行：桌面进程、守护进程、本地模型。桌面进程能展开看内存花在哪。
#[derive(Debug, PartialEq)]
struct Row {
    label: String,
    detail: Option<String>,
    value: RowValue,
    /// 内存拆成的几块，空的就不能展开。
    parts: Vec<MemoryPart>,
}

/// 一行右边显示什么：量到的数，或者没有数时的状态（「未运行」「不可用」）。
#[derive(Debug, PartialEq)]
enum RowValue {
    Usage(ProcessUsage),
    Text(&'static str),
}

/// 内存里的一块。
#[derive(Debug, PartialEq)]
struct MemoryPart {
    label: &'static str,
    detail: &'static str,
    bytes: u64,
}

/// 桌面进程的内存，按固定顺序列，数字跳动时行不换位置。
fn memory_parts(memory: MemoryBreakdown) -> Vec<MemoryPart> {
    vec![
        MemoryPart {
            label: "画面缓冲与纹理",
            detail: "GPU 绘制用，窗口越大占得越多",
            bytes: memory.graphics,
        },
        MemoryPart {
            label: "程序数据",
            detail: "会话内容、文字排版缓存等，堆上正在用的部分",
            bytes: memory.heap,
        },
        MemoryPart {
            label: "其它",
            detail: "系统库的数据、线程栈、堆里空着的页",
            bytes: memory.other(),
        },
    ]
}

/// 一台机器。
struct Section {
    name: String,
    /// 行 id 的前缀，几台机器的行不能撞。
    id_prefix: String,
    total: Option<ProcessUsage>,
    /// 桌面进程（只有本机）和守护进程。
    fixed: Vec<Row>,
    sessions: Vec<SessionGroup>,
    others: Option<SessionGroup>,
    /// 读取中、离线或出错时的说明。
    note: Option<String>,
    /// 本地模型推理统计；这台机器没回话时没有。
    inference: Option<InferenceView>,
}

/// 每个守护会话属于侧栏哪个会话：(运行时, 守护会话 id) → (会话下标, 标题)。
fn session_owners(ws: &Workspace, cx: &App) -> HashMap<(RuntimeKey, String), (usize, String)> {
    let mut owners = HashMap::new();
    for (ix, session) in ws.sessions.iter().enumerate() {
        let key = session.runtime(cx).key().clone();
        let title = session.title(cx);
        let ids: Vec<String> = match &session.kind {
            SessionKind::Conversation(view) => vec![view.read(cx).session_id().to_string()],
            SessionKind::Term { .. } => session
                .term_leaves()
                .iter()
                .map(|leaf| leaf.read(cx).session_id().to_string())
                .collect(),
        };
        for id in ids {
            owners.insert((key.clone(), id), (ix, title.clone()));
        }
    }
    owners
}

/// 本机在前，远程机器按添加顺序。
fn sections(ws: &Workspace, cx: &App) -> Vec<Section> {
    let monitor = &ws.resource_monitor;
    let owners = session_owners(ws, cx);
    let mut keys = vec![RuntimeKey::Local];
    if let Some(hub) = cx.try_global::<RuntimeHub>() {
        keys.extend(
            hub.remotes
                .iter()
                .map(|remote| remote.runtime.key().clone()),
        );
    }
    keys.into_iter()
        .map(|key| {
            let mut section = Section {
                name: crate::runtimes::runtime_name(&key, cx),
                id_prefix: format!("{key:?}"),
                total: None,
                fixed: Vec::new(),
                sessions: Vec::new(),
                others: None,
                note: None,
                inference: None,
            };
            if key == RuntimeKey::Local
                && let Some(desktop) = monitor.desktop
            {
                section.fixed.push(Row {
                    label: "Smelt 界面".to_string(),
                    detail: Some("GPUI 主进程".to_string()),
                    value: RowValue::Usage(desktop),
                    parts: monitor.desktop_memory.map(memory_parts).unwrap_or_default(),
                });
                section.total = Some(desktop);
            }
            match monitor.result(&key) {
                Some(Ok(result)) => {
                    let mut total = section.total.unwrap_or_default();
                    add(&mut total, runtime_total(result));
                    section.total = Some(total);
                    section.fixed.push(Row {
                        label: "smeltd 后台服务".to_string(),
                        detail: None,
                        value: RowValue::Usage(result.daemon),
                        parts: Vec::new(),
                    });
                    section.fixed.push(local_model_row(result.local_model));
                    section.inference =
                        Some(inference_view(result.local_model_inference.as_ref()));
                    let (groups, others) = group_sessions(&result.sessions, |id| {
                        owners.get(&(key.clone(), id.to_string())).cloned()
                    });
                    section.sessions = groups;
                    section.others = others;
                }
                Some(Err(error)) => section.note = Some(error.clone()),
                None => {
                    section.note = Some(
                        crate::runtimes::runtime_unavailable_label(&key, cx)
                            .unwrap_or("读取中…")
                            .to_string(),
                    );
                }
            }
            section
        })
        .collect()
}

const CPU_COLUMN: f32 = 64.;
const MEMORY_COLUMN: f32 = 80.;

/// 资源页。`snapshot` 只在停在这一页时有。
pub(crate) fn resources_page(
    workspace: Entity<Workspace>,
    snapshot: Option<Rc<PanelSnapshot>>,
) -> SettingPage {
    // 它是监视器不是设置：标题旁写刷新频率，口径说明放到总量卡片底部。
    let page = SettingPage::new("资源占用")
        .title_suffix(|_, cx| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("停在这一页时每 2 秒刷新")
        })
        .resettable(false);
    let Some(snapshot) = snapshot else {
        return page;
    };
    let summary = snapshot.clone();
    let mut page = page.group(SettingGroup::new().item(SettingItem::render(
        move |_, _, cx: &mut App| summary_view(&summary, cx),
    )));
    for ix in 0..snapshot.sections.len() {
        let snapshot = snapshot.clone();
        let workspace = workspace.clone();
        page = page.group(
            SettingGroup::new()
                .title(snapshot.sections[ix].name.clone())
                .item(SettingItem::render(move |_, window, cx: &mut App| {
                    let sort = window.use_keyed_state("settings-resources-sort", cx, |_, _| {
                        PanelSort::default()
                    });
                    let expanded =
                        window.use_keyed_state("settings-resources-expanded", cx, |_, _| {
                            ExpandedRows::default()
                        });
                    section_view(&snapshot.sections[ix], sort, expanded, &workspace, cx)
                })),
        );
    }
    page
}

/// 本机合计、占比和最近两分钟的内存曲线。
fn summary_view(snapshot: &PanelSnapshot, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(local) = snapshot.local else {
        return div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("读取中…")
            .into_any_element();
    };
    let share = snapshot.total_memory.map(|total| {
        format!(
            "占本机 {} 内存的 {:.0}%",
            format_bytes(total),
            local.memory_bytes as f64 * 100. / total as f64
        )
    });
    let mut view = v_flex().w_full().gap_2().child(
        h_flex()
            .gap_3()
            .items_baseline()
            .font_features(ui_theme::tabular_figures())
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(format_bytes(local.memory_bytes)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(format!("CPU {}", format_cpu(local.cpu_percent))),
            )
            .children(share.map(|share| {
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(share)
            })),
    );
    if snapshot.chart.len() > 1 {
        let peak = snapshot.chart.iter().copied().fold(0., f64::max);
        view = view.child(
            div().h(px(96.)).w_full().child(
                AreaChart::new(snapshot.chart.clone().into_iter().enumerate())
                    .id("settings-resources-chart")
                    .x(|(ix, _)| ix.to_string())
                    .y(|(_, bytes)| *bytes)
                    .linear()
                    .x_axis(false)
                    .y_axis(false)
                    .grid(false)
                    .y_domain(0., peak * 1.2)
                    .interactive(false)
                    .appear(false),
            ),
        );
    }
    view.child(div().text_xs().text_color(theme.muted_foreground).child(
        "本机合计：Smelt 界面、后台服务、本机会话和本地模型，曲线最多两分钟。会话按整棵进程树计，\
                 包括智能体起的命令；CPU 按单核计，多核可超过 100%。本地模型是独立进程，\
                 不算在后台服务里；它只在用时运行，短任务可能在两次采样之间跑完而采不到；\
                 GPU 占用不单独统计。",
    ))
    .into_any_element()
}

/// 一台机器的表：列头（点 CPU、内存按它排）、固定行、会话、侧栏之外的会话、合计。
fn section_view(
    section: &Section,
    sort: Entity<PanelSort>,
    expanded: Entity<ExpandedRows>,
    workspace: &Entity<Workspace>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = RowColors {
        fg: theme.foreground,
        muted: theme.muted_foreground,
    };
    let sort_by = sort.read(cx).0;
    let sort_header = |label: &'static str, by: SortBy, width: f32| {
        let sort = sort.clone();
        div()
            .id(SharedString::from(format!("{}-sort-{label}", section.id_prefix)))
            .w(px(width))
            .text_right()
            .text_color(if sort_by == by { colors.fg } else { colors.muted })
            .when(sort_by == by, |d| d.font_medium())
            .on_click(move |_ev, _window, cx| {
                sort.update(cx, |sort, cx| {
                    sort.0 = by;
                    cx.notify();
                });
            })
            // 当前按哪列排，在列头标出来：只靠颜色深浅分不出来。
            .child(if sort_by == by {
                format!("{label} ↓")
            } else {
                label.to_string()
            })
    };
    let mut table = v_flex().w_full().child(
        h_flex()
            .px_2()
            .pb_1()
            .gap_3()
            .text_xs()
            .child(div().w(px(DISCLOSURE_COLUMN)))
            .child(div().flex_1().text_color(colors.muted).child("名称"))
            .child(sort_header("CPU", SortBy::Cpu, CPU_COLUMN))
            .child(sort_header("内存", SortBy::Memory, MEMORY_COLUMN)),
    );
    if let Some(note) = &section.note {
        table = table.child(
            h_flex()
                .px_2()
                .py_1()
                .gap_3()
                .child(div().flex_none().w(px(DISCLOSURE_COLUMN)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(colors.muted)
                        .child(note.clone()),
                ),
        );
    }
    let open = &expanded.read(cx).0;
    for (ix, row) in section.fixed.iter().enumerate() {
        let id = SharedString::from(format!("{}-fixed-{ix}", section.id_prefix));
        let is_open = open.contains(&id);
        table = table.child(fixed_row(id, row, is_open, &expanded, colors));
    }
    if let Some(inference) = &section.inference {
        table = table.child(inference_block(inference, colors));
    }
    let mut sessions: Vec<&SessionGroup> = section.sessions.iter().collect();
    sort_by.sort(&mut sessions);
    for (ix, group) in sessions.into_iter().chain(&section.others).enumerate() {
        let id = SharedString::from(match &group.jump {
            Some(jump) => format!("{}-{jump}", section.id_prefix),
            None => format!("{}-others-{ix}", section.id_prefix),
        });
        let is_open = open.contains(&id);
        table = table.child(session_row(
            id, group, is_open, &expanded, workspace, colors,
        ));
    }
    if let Some(total) = section.total {
        table = table.child(
            h_flex()
                .mt_1()
                .px_2()
                .pt_1()
                .gap_3()
                .border_t_1()
                .border_color(theme.border)
                .child(div().w(px(DISCLOSURE_COLUMN)))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_medium()
                        .text_color(colors.fg)
                        .child("合计"),
                )
                .child(usage_columns(total, colors.fg)),
        );
    }
    table.into_any_element()
}

/// 本地模型推理统计：跟在固定行后面，名字和固定行对齐，只读快照里拼好的字。
fn inference_block(view: &InferenceView, colors: RowColors) -> Div {
    let line = |label: String, detail: Option<String>| {
        h_flex()
            .px_2()
            .py_1()
            .gap_3()
            .items_center()
            .child(div().flex_none().w(px(DISCLOSURE_COLUMN)))
            .child(row_label(label, detail, colors))
    };
    let mut block = v_flex().child(line("本地模型推理".to_string(), None));
    block = match view {
        InferenceView::Empty(text) => block.child(line("—".to_string(), Some(text.to_string()))),
        InferenceView::Lines(lines) => block.children(
            lines
                .iter()
                .map(|l| line(l.label.clone(), Some(l.detail.clone()))),
        ),
    };
    block.child(
        h_flex()
            .px_2()
            .pb_1()
            .gap_3()
            .child(div().flex_none().w(px(DISCLOSURE_COLUMN)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(colors.muted)
                    .child(INFERENCE_NOTE),
            ),
    )
}

/// 展开箭头那一列的宽度。没有明细的行也空出这一列，名字才对得齐。
const DISCLOSURE_COLUMN: f32 = 20.;

#[derive(Clone, Copy)]
struct RowColors {
    fg: Hsla,
    muted: Hsla,
}

/// 一行右边的两列数字。
fn usage_columns(usage: ProcessUsage, color: Hsla) -> Div {
    h_flex()
        .flex_none()
        .font_features(ui_theme::tabular_figures())
        .text_sm()
        .text_color(color)
        .child(
            div()
                .w(px(CPU_COLUMN))
                .text_right()
                .child(format_cpu(usage.cpu_percent)),
        )
        .child(
            div()
                .w(px(MEMORY_COLUMN))
                .text_right()
                .child(format_bytes(usage.memory_bytes)),
        )
}

/// 固定行右边：有数就是两列数字，没有就在两列的位置写状态。
fn row_value_columns(value: &RowValue, colors: RowColors) -> Div {
    match value {
        RowValue::Usage(usage) => usage_columns(*usage, colors.fg),
        RowValue::Text(text) => h_flex()
            .flex_none()
            .w(px(CPU_COLUMN + MEMORY_COLUMN))
            .justify_end()
            .text_sm()
            .text_color(colors.muted)
            .child(*text),
    }
}

fn row_label(label: String, detail: Option<String>, colors: RowColors) -> Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .overflow_hidden()
                .text_sm()
                .text_color(colors.fg)
                .child(truncated_line(label)),
        )
        .children(detail.map(|detail| {
            div()
                .overflow_hidden()
                .text_xs()
                .text_color(colors.muted)
                .child(truncated_line(detail))
        }))
}

/// 桌面进程、守护进程：单个进程。有内存拆分的点整行展开或收起。
fn fixed_row(
    id: SharedString,
    row: &Row,
    is_open: bool,
    expanded: &Entity<ExpandedRows>,
    colors: RowColors,
) -> AnyElement {
    let label = row_label(row.label.clone(), row.detail.clone(), colors);
    if row.parts.is_empty() {
        return h_flex()
            .px_2()
            .py_1()
            .gap_3()
            .items_center()
            .child(div().w(px(DISCLOSURE_COLUMN)))
            .child(label)
            .child(row_value_columns(&row.value, colors))
            .into_any_element();
    }
    let head = ListRow::new(ElementId::from(id.clone()))
        .card()
        .px_2()
        .py_1()
        .gap_3()
        .flex()
        .items_center()
        .rounded(ui_theme::row_radius())
        .on_click(toggle_row(expanded, id))
        .child(disclosure(is_open, colors))
        .child(label)
        .child(row_value_columns(&row.value, colors));
    if !is_open {
        return head.into_any_element();
    }
    let body = v_flex()
        // 和名字对齐：行内边距之后是箭头列，再隔一个列间距。
        .pl(px(DISCLOSURE_COLUMN + 12.))
        .pb_1()
        .children(row.parts.iter().map(|part| memory_part_row(part, colors)));
    v_flex().child(head).child(body).into_any_element()
}

/// 明细里的一块内存：名字、它装的是什么，只有内存一列。
fn memory_part_row(part: &MemoryPart, colors: RowColors) -> impl IntoElement + use<> {
    h_flex()
        .px_2()
        .py(px(3.))
        .gap_3()
        .items_center()
        .child(row_label(
            part.label.to_string(),
            Some(part.detail.to_string()),
            colors,
        ))
        .child(
            h_flex()
                .flex_none()
                .font_features(ui_theme::tabular_figures())
                .text_sm()
                .text_color(colors.muted)
                .child(div().w(px(CPU_COLUMN)))
                .child(
                    div()
                        .w(px(MEMORY_COLUMN))
                        .text_right()
                        .child(format_bytes(part.bytes)),
                ),
        )
}

/// 点一下展开、再点收起。
fn toggle_row(
    expanded: &Entity<ExpandedRows>,
    id: SharedString,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let expanded = expanded.clone();
    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        expanded.update(cx, |expanded, cx| {
            if !expanded.0.remove(&id) {
                expanded.0.insert(id.clone());
            }
            cx.notify();
        });
    }
}

/// 展开箭头。
fn disclosure(is_open: bool, colors: RowColors) -> impl IntoElement + use<> {
    div()
        .w(px(DISCLOSURE_COLUMN))
        .flex_none()
        .text_color(colors.muted)
        .child(Icon::new(if is_open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        }))
}

/// 一行会话。点整行展开或收起它的进程；右边的按钮跳回主窗口的这个会话。
fn session_row(
    id: SharedString,
    group: &SessionGroup,
    is_open: bool,
    expanded: &Entity<ExpandedRows>,
    workspace: &Entity<Workspace>,
    colors: RowColors,
) -> AnyElement {
    let summary = process_summary(group.usage.process_count, &group.processes);
    let detail = match group.jump {
        Some(_) => summary,
        None => format!("自动化、子智能体或别的客户端开的 · {summary}"),
    };
    let jump = group.jump.clone().map(|session_id| {
        let workspace = workspace.clone();
        // 按下就截住：不然这一下也会落到外面那一行上，顺手把它展开了。
        div()
            .flex_none()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(SharedString::from(format!("{id}-jump")))
                    .quiet()
                    .icon_size()
                    .icon(IconName::ExternalLink)
                    .icon_action("跳到会话")
                    .on_click(move |_, _, cx| {
                        let session_id = session_id.clone();
                        workspace.update(cx, |ws, cx| {
                            ws.jump_to_session_from_settings(session_id, cx)
                        });
                    }),
            )
    });
    let head = ListRow::new(ElementId::from(id.clone()))
        .card()
        .px_2()
        .py_1()
        .gap_3()
        .flex()
        .items_center()
        .rounded(ui_theme::row_radius())
        .on_click(toggle_row(expanded, id.clone()))
        .child(disclosure(is_open, colors))
        .child(row_label(group.title.clone(), Some(detail), colors))
        .children(jump)
        .child(usage_columns(group.usage, colors.fg));
    if !is_open {
        return head.into_any_element();
    }
    let mut body = v_flex()
        // 和会话名对齐：行内边距之后是箭头列，再隔一个列间距。
        .pl(px(DISCLOSURE_COLUMN + 12.))
        .pb_1()
        .children(group.processes.iter().map(|process| process_row(process, colors)));
    let unlisted = (group.usage.process_count as usize).saturating_sub(group.processes.len());
    if unlisted > 0 {
        body = body.child(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(colors.muted)
                .child(format!("另有 {unlisted} 个占用更小的进程没列出")),
        );
    }
    v_flex().child(head).child(body).into_any_element()
}

/// 明细里的一个进程：名字和 pid，下面一行是命令行。
fn process_row(process: &ProcessEntry, colors: RowColors) -> impl IntoElement + use<> {
    let command = (!process.command.is_empty()).then(|| process.command.clone());
    h_flex()
        .px_2()
        .py(px(3.))
        .gap_3()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .child(
                            div()
                                .overflow_hidden()
                                .text_sm()
                                .text_color(colors.fg)
                                .child(truncated_line(process.name.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_features(ui_theme::tabular_figures())
                                .text_color(colors.muted)
                                .child(format!("pid {}", process.pid)),
                        ),
                )
                .children(command.map(|command| {
                    div()
                        .overflow_hidden()
                        .text_xs()
                        .font_family(crate::terminal_view::font_family())
                        .text_color(colors.muted)
                        .child(truncated_line(command))
                })),
        )
        .child(usage_columns(process.usage, colors.muted))
}

#[cfg(test)]
mod tests {
    // 不用 `use super::*;`：父模块 glob 导入了 `gpui::*`，带进来会把 gpui 的 `test` 宏和整张
    // trait 解析图一起带进测试模块（见 session_history.rs 的同款注释）。
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use smelt_model::control_api::{
        InferenceMean, LocalModelInference, LocalModelInferenceAverage, LocalModelInferenceStats,
        LocalModelUsage, ProcessEntry, ProcessUsage, ResourceUsageResult, SessionResourceUsage,
    };

    use super::{
        InferenceLine, InferenceView, ResourceMonitor, RowValue, SessionGroup, SortBy,
        format_bytes, format_cpu, format_duration_us, group_sessions, inference_view,
        local_model_row, memory_series, process_summary, runtime_total,
    };
    use crate::RuntimeKey;

    fn usage(cpu_percent: f32, memory_bytes: u64, process_count: u32) -> ProcessUsage {
        ProcessUsage {
            cpu_percent,
            memory_bytes,
            process_count,
        }
    }

    fn session(id: &str, usage: ProcessUsage) -> SessionResourceUsage {
        SessionResourceUsage {
            session_id: id.to_string(),
            usage,
            processes: vec![process(id, usage.memory_bytes)],
        }
    }

    fn process(name: &str, memory_bytes: u64) -> ProcessEntry {
        ProcessEntry {
            pid: 1,
            name: name.to_string(),
            command: String::new(),
            usage: usage(0., memory_bytes, 1),
        }
    }

    #[test]
    fn panes_of_one_session_add_up_and_unknown_sessions_are_pooled() {
        let sessions = [
            session("pane-a", usage(10., 100, 2)),
            session("pane-b", usage(5., 150, 1)),
            session("chat", usage(1., 300, 4)),
            session("automation", usage(2., 20, 1)),
            session("subagent", usage(3., 30, 1)),
        ];
        let (groups, others) = group_sessions(&sessions, |id| match id {
            "pane-a" | "pane-b" => Some((0, "终端".to_string())),
            "chat" => Some((1, "对话".to_string())),
            _ => None,
        });
        assert_eq!(
            groups,
            vec![
                SessionGroup {
                    title: "终端".to_string(),
                    usage: usage(15., 250, 3),
                    // 两个 pane 的进程合在一起，内存大的在前。
                    processes: vec![process("pane-b", 150), process("pane-a", 100)],
                    jump: Some("pane-a".to_string()),
                },
                SessionGroup {
                    title: "对话".to_string(),
                    usage: usage(1., 300, 4),
                    processes: vec![process("chat", 300)],
                    jump: Some("chat".to_string()),
                },
            ]
        );
        assert_eq!(
            others,
            Some(SessionGroup {
                title: "侧栏之外的 2 个会话".to_string(),
                usage: usage(5., 50, 2),
                processes: vec![process("subagent", 30), process("automation", 20)],
                jump: None,
            })
        );
    }

    #[test]
    fn the_summary_names_the_heaviest_processes() {
        let processes = [
            process("node", 500),
            process("bun", 400),
            process("node", 300),
            process("rg", 20),
            process("node", 10),
        ];
        assert_eq!(process_summary(5, &processes), "5 个进程：node ×3、bun、rg");
        let many = ["a", "b", "c", "d", "e"].map(|name| process(name, 1));
        assert_eq!(process_summary(9, &many), "9 个进程：a、b、c、d 等");
        assert_eq!(process_summary(3, &[]), "3 个进程");
    }

    #[test]
    fn sorting_puts_the_heaviest_first() {
        let group = |title: &str, cpu, memory| SessionGroup {
            title: title.to_string(),
            usage: usage(cpu, memory, 1),
            processes: Vec::new(),
            jump: Some(title.to_string()),
        };
        let mut groups = vec![group("a", 50., 10), group("b", 1., 900)];
        SortBy::Memory.sort(&mut groups);
        assert_eq!(groups[0].title, "b");
        SortBy::Cpu.sort(&mut groups);
        assert_eq!(groups[0].title, "a");
    }

    /// 机器回得慢时样本会稀，曲线仍按 2 秒一点、按时刻取值；还没有样本的时刻不出点。
    #[test]
    fn the_chart_follows_time_not_sample_count() {
        let start = Instant::now();
        let history = VecDeque::from([(start, 100), (start + Duration::from_secs(10), 200)]);
        let now = start + Duration::from_secs(14);
        let series = memory_series(&history, now);
        // now 往回到 start 共 8 个点（0、2、…、14 秒），前 5 个取第一份样本。
        assert_eq!(series, vec![100., 100., 100., 100., 100., 200., 200., 200.]);
    }

    #[test]
    fn old_samples_are_dropped_but_the_one_feeding_the_left_edge_stays() {
        let start = Instant::now();
        let mut monitor = ResourceMonitor::default();
        monitor.apply(Some(usage(1., 10, 1)), Vec::new(), start);
        let later = start + Duration::from_secs(600);
        monitor.apply(Some(usage(1., 20, 1)), Vec::new(), later);
        assert_eq!(monitor.history.len(), 1);
        assert_eq!(monitor.chart, vec![20.]);
    }

    /// 隔了一阵再回到资源页，上次的数和曲线不能接着画：中间那段没人量过。
    #[test]
    fn coming_back_starts_from_a_clean_slate() {
        let start = Instant::now();
        let mut monitor = ResourceMonitor::default();
        monitor.apply(Some(usage(1., 10, 1)), Vec::new(), start);
        monitor.reset();
        assert_eq!(monitor.local_total(), None);
        assert!(monitor.history.is_empty() && monitor.chart.is_empty());
        monitor.apply(
            Some(usage(1., 30, 1)),
            Vec::new(),
            start + Duration::from_secs(300),
        );
        assert_eq!(monitor.chart, vec![30.]);
    }

    #[test]
    fn the_local_total_covers_the_desktop_daemon_and_sessions() {
        let mut monitor = ResourceMonitor::default();
        let local = ResourceUsageResult {
            daemon: usage(1., 10, 1),
            sessions: vec![session("s", usage(2., 20, 3))],
            total_memory_bytes: 1000,
            local_model: None,
            local_model_inference: None,
        };
        monitor.apply(
            Some(usage(4., 40, 1)),
            vec![(RuntimeKey::Local, Ok(local))],
            Instant::now(),
        );
        assert_eq!(monitor.local_total(), Some(usage(7., 70, 5)));
    }

    /// 模型进程不在 daemon 里，合计里加且只加一次；没在跑、旧运行时不报都不加。
    #[test]
    fn a_running_local_model_counts_once_in_the_total() {
        let result = |local_model| ResourceUsageResult {
            daemon: usage(1., 10, 1),
            sessions: vec![session("s", usage(2., 20, 3))],
            total_memory_bytes: 1000,
            local_model,
            local_model_inference: None,
        };
        let running = Some(LocalModelUsage::Running {
            usage: usage(50., 700, 1),
        });
        assert_eq!(runtime_total(&result(running)), usage(53., 730, 5));
        assert_eq!(
            runtime_total(&result(Some(LocalModelUsage::NotRunning))),
            usage(3., 30, 4)
        );
        assert_eq!(runtime_total(&result(None)), usage(3., 30, 4));
        assert_eq!(
            runtime_total(&result(Some(LocalModelUsage::Unsampled))),
            usage(3., 30, 4)
        );

        let mut monitor = ResourceMonitor::default();
        monitor.apply(
            Some(usage(4., 40, 1)),
            vec![(RuntimeKey::Local, Ok(result(running)))],
            Instant::now(),
        );
        assert_eq!(monitor.local_total(), Some(usage(57., 770, 6)));
    }

    #[test]
    fn the_local_model_row_says_running_stopped_or_unavailable() {
        let running = usage(50., 700, 1);
        assert_eq!(
            local_model_row(Some(LocalModelUsage::Running { usage: running })).value,
            RowValue::Usage(running)
        );
        assert_eq!(
            local_model_row(Some(LocalModelUsage::NotRunning)).value,
            RowValue::Text("未运行")
        );
        assert_eq!(
            local_model_row(Some(LocalModelUsage::Unsampled)).value,
            RowValue::Text("采样不可用")
        );
        assert_eq!(local_model_row(None).value, RowValue::Text("不可用"));
        assert_eq!(local_model_row(None).label, "本地模型");
    }

    #[test]
    fn numbers_read_like_activity_monitor() {
        assert_eq!(format_bytes(512 * 1024), "512 KB");
        assert_eq!(format_bytes(820 * 1024 * 1024), "820 MB");
        assert_eq!(format_bytes(1288 * 1024 * 1024), "1.3 GB");
        assert_eq!(format_cpu(0.34), "0.3%");
        assert_eq!(format_cpu(135.4), "135%");
    }

    #[test]
    fn durations_read_naturally_and_zero_stays_zero() {
        assert_eq!(format_duration_us(0.), "0 ms");
        assert_eq!(format_duration_us(320.), "320 µs");
        assert_eq!(format_duration_us(850_000.), "850 ms");
        assert_eq!(format_duration_us(1_500_000.), "1.5 s");
        assert_eq!(format_duration_us(65_000_000.), "1 分 5 秒");
    }

    #[test]
    fn inference_stats_have_empty_states() {
        assert_eq!(
            inference_view(None),
            InferenceView::Empty("这台机器的后台服务版本较旧，不报推理统计")
        );
        assert_eq!(
            inference_view(Some(&LocalModelInferenceStats::default())),
            InferenceView::Empty("后台服务启动以来还没有成功的本地模型推理")
        );
    }

    /// 缺的写「—」，真的 0 写 0；每个模型各自一行平均，部分有数据时注明次数。
    #[test]
    fn inference_stats_show_last_and_per_model_averages() {
        let stats = LocalModelInferenceStats {
            last: Some(LocalModelInference {
                model: "qwen3.5-0.8b".into(),
                wall_time_us: 1_500_000,
                cpu_time_us: Some(0),
                peak_memory_bytes: None,
            }),
            averages: vec![
                LocalModelInferenceAverage {
                    model: "a".into(),
                    inferences: 2,
                    wall_time_us: 850_000.,
                    cpu_time_us: Some(InferenceMean { samples: 1, mean: 2_000_000. }),
                    peak_memory_bytes: Some(InferenceMean {
                        samples: 2,
                        mean: 820. * 1024. * 1024.,
                    }),
                },
                LocalModelInferenceAverage {
                    model: "b".into(),
                    inferences: 1,
                    wall_time_us: 0.,
                    cpu_time_us: None,
                    peak_memory_bytes: Some(InferenceMean { samples: 1, mean: 0. }),
                },
            ],
        };
        let line = |label: &str, detail: &str| InferenceLine {
            label: label.to_string(),
            detail: detail.to_string(),
        };
        assert_eq!(
            inference_view(Some(&stats)),
            InferenceView::Lines(vec![
                line("上次推理 · qwen3.5-0.8b", "耗时 1.5 s · CPU 0 ms · 峰值内存 —"),
                line(
                    "a · 成功 2 次",
                    "平均耗时 850 ms · CPU 2.0 s（1 次有数据） · 峰值内存 820 MB"
                ),
                line("b · 成功 1 次", "平均耗时 0 ms · CPU — · 峰值内存 0 KB"),
            ])
        );
    }
}
