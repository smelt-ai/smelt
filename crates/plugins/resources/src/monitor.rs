//! 采到的数和怎么把它们拼成一页：按机器分节，守护会话按对话归拢。

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use api::RuntimeKey;
use api::model::control_api::{
    ProcessEntry, ProcessUsage, ResourceUsageResult, SessionResourceUsage,
};
use api::model::daemon_state::DaemonSessionState;
use api::model::workspace_directory::WorkspaceConversation;
use api::process_usage::{MemoryBreakdown, ProcessSampler};
use gpui::App;
use smelt_plugin_api as api;

/// 两轮采样的间隔。
pub(crate) const INTERVAL: Duration = Duration::from_secs(2);
/// 刚开始看时先垫一轮，隔这么久再采第一轮真的（见 `sample_loop`）。
pub(crate) const PRIME_DELAY: Duration = Duration::from_millis(500);
/// 一台机器回得慢不拖别的机器（各自并发），但也不能一直等。
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// 曲线的点数；点距是 [`INTERVAL`]，一共两分钟。
const CHART_POINTS: usize = 60;

pub(crate) type RuntimeResults = Vec<(RuntimeKey, Result<ResourceUsageResult, String>)>;

/// 桌面进程这一轮的占用和内存拆分（只有 macOS 拆得开）。
pub(crate) type DesktopSample = (ProcessUsage, Option<MemoryBreakdown>);

/// 量桌面进程自己。在后台调：要读进程表。
pub(crate) fn sample_desktop(sampler: &mut ProcessSampler) -> Option<DesktopSample> {
    sampler.sample_process(std::process::id()).map(|usage| {
        let memory = api::process_usage::own_memory();
        // 拆分和总数取同一刻的，展开后几块加起来正好是这一行。
        let usage = ProcessUsage {
            cpu_percent: usage.cpu_percent,
            memory_bytes: memory.map_or(usage.memory_bytes, |memory| memory.total),
            process_count: usage.processes,
        };
        (usage, memory)
    })
}

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
}

impl ResourceMonitor {
    fn result(&self, key: &RuntimeKey) -> Option<&Result<ResourceUsageResult, String>> {
        self.runtimes
            .iter()
            .find(|(runtime, _)| runtime == key)
            .map(|(_, result)| result)
    }

    /// 本机合计：桌面进程、本机守护和本机会话。本机运行时没回话时只有桌面。
    fn local_total(&self) -> Option<ProcessUsage> {
        let mut total = self.desktop?;
        if let Some(Ok(local)) = self.result(&RuntimeKey::Local) {
            add(&mut total, runtime_total(local));
        }
        Some(total)
    }

    /// 重新开始看：上次看到的数和曲线都作废。
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn apply(
        &mut self,
        desktop: Option<DesktopSample>,
        runtimes: RuntimeResults,
        now: Instant,
    ) {
        let (desktop, desktop_memory) = desktop.unzip();
        self.desktop = desktop;
        self.desktop_memory = desktop_memory.flatten();
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

    /// 拼这一页：本机在前，远程机器按添加顺序。机器名、离线原因、会话属于哪段对话读宿主的。
    pub(crate) fn snapshot(&self, cx: &App) -> PanelSnapshot {
        let total_memory = match self.result(&RuntimeKey::Local) {
            Some(Ok(result)) if result.total_memory_bytes > 0 => Some(result.total_memory_bytes),
            _ => None,
        };
        let host = api::host(cx);
        let sections = host
            .machines(cx)
            .into_iter()
            .map(|machine| {
                let owners = session_owners(
                    host.workspace_directory(&machine.key, cx)
                        .map(|directory| directory.conversations)
                        .unwrap_or_default(),
                    host.session_states(&machine.key, cx),
                );
                self.section(machine, &owners)
            })
            .collect();
        PanelSnapshot {
            local: self.local_total(),
            total_memory,
            chart: self.chart.clone(),
            sections,
        }
    }

    fn section(&self, machine: api::Machine, owners: &HashMap<String, (usize, String)>) -> Section {
        let mut section = Section {
            name: machine.name,
            id_prefix: format!("{:?}", machine.key),
            machine: machine.key.clone(),
            total: None,
            fixed: Vec::new(),
            sessions: Vec::new(),
            others: None,
            note: None,
        };
        if machine.key == RuntimeKey::Local
            && let Some(desktop) = self.desktop
        {
            section.fixed.push(Row {
                label: "Smelt 界面".to_string(),
                detail: Some("GPUI 主进程".to_string()),
                usage: desktop,
                parts: self.desktop_memory.map(memory_parts).unwrap_or_default(),
            });
            section.total = Some(desktop);
        }
        match self.result(&machine.key) {
            Some(Ok(result)) => {
                let mut total = section.total.unwrap_or_default();
                add(&mut total, runtime_total(result));
                section.total = Some(total);
                section.fixed.push(Row {
                    label: "smeltd 后台服务".to_string(),
                    detail: None,
                    usage: result.daemon,
                    parts: Vec::new(),
                });
                let (groups, others) =
                    group_sessions(&result.sessions, |id| owners.get(id).cloned());
                section.sessions = groups;
                section.others = others;
            }
            Some(Err(error)) => section.note = Some(error.clone()),
            None => {
                section.note = Some(machine.unavailable.unwrap_or_else(|| "读取中…".to_string()));
            }
        }
        section
    }
}

fn add(total: &mut ProcessUsage, usage: ProcessUsage) {
    total.cpu_percent += usage.cpu_percent;
    total.memory_bytes += usage.memory_bytes;
    total.process_count += usage.process_count;
}

/// 一台运行时的合计：守护进程和全部会话。
fn runtime_total(result: &ResourceUsageResult) -> ProcessUsage {
    let mut total = result.daemon;
    for session in &result.sessions {
        add(&mut total, session.usage);
    }
    total
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
pub(crate) struct SessionGroup {
    pub(crate) title: String,
    pub(crate) usage: ProcessUsage,
    /// 树里的进程，内存大的在前。运行时每个会话最多报一部分，可能比 `usage.process_count` 少。
    pub(crate) processes: Vec<ProcessEntry>,
    /// 点「跳到会话」去哪个守护会话。侧栏之外的会话没有。
    pub(crate) jump: Option<String>,
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
pub(crate) fn process_summary(count: u32, processes: &[ProcessEntry]) -> String {
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
pub(crate) enum SortBy {
    #[default]
    Memory,
    Cpu,
}

impl SortBy {
    /// 重的在前。
    pub(crate) fn sort<G: Borrow<SessionGroup>>(self, groups: &mut [G]) {
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

pub(crate) fn format_bytes(bytes: u64) -> String {
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

pub(crate) fn format_cpu(percent: f32) -> String {
    if percent < 10. {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.0}%")
    }
}

/// 资源页要显示的全部内容，采到一轮、会话或机器变了时拼好。
#[derive(Default)]
pub(crate) struct PanelSnapshot {
    pub(crate) local: Option<ProcessUsage>,
    /// 本机物理内存总量，算占比用。
    pub(crate) total_memory: Option<u64>,
    pub(crate) chart: Vec<f64>,
    pub(crate) sections: Vec<Section>,
}

/// 不跟着排序走的一行：桌面进程、守护进程。它们就是单个进程；桌面进程能展开看内存花在哪。
pub(crate) struct Row {
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
    pub(crate) usage: ProcessUsage,
    /// 内存拆成的几块，空的就不能展开。
    pub(crate) parts: Vec<MemoryPart>,
}

/// 内存里的一块。
pub(crate) struct MemoryPart {
    pub(crate) label: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) bytes: u64,
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
pub(crate) struct Section {
    pub(crate) name: String,
    /// 行 id 的前缀，几台机器的行不能撞。
    pub(crate) id_prefix: String,
    /// 这台机器：「跳到会话」跳的是它上面的会话。
    pub(crate) machine: RuntimeKey,
    pub(crate) total: Option<ProcessUsage>,
    /// 桌面进程（只有本机）和守护进程。
    pub(crate) fixed: Vec<Row>,
    pub(crate) sessions: Vec<SessionGroup>,
    pub(crate) others: Option<SessionGroup>,
    /// 读取中、离线或出错时的说明。
    pub(crate) note: Option<String>,
}

/// 每个守护会话属于哪段对话：守护会话 id → (对话下标, 名字)。终端分屏每格一个守护会话，
/// 都归到同一段对话。名字是运行时算好推来的，和侧栏同一个；归档了的不算。
fn session_owners(
    conversations: Vec<WorkspaceConversation>,
    states: Vec<DaemonSessionState>,
) -> HashMap<String, (usize, String)> {
    let states: HashMap<String, DaemonSessionState> = states
        .into_iter()
        .map(|state| (state.id.clone(), state))
        .collect();
    let mut owners = HashMap::new();
    for (ix, conversation) in conversations.iter().enumerate() {
        if conversation.archived_at.is_some() {
            continue;
        }
        let Some((ids, title)) = conversation_sessions(conversation, &states) else {
            continue;
        };
        for id in ids {
            owners.insert(id, (ix, title.clone()));
        }
    }
    owners
}

/// 一段对话的守护会话 id 和名字。终端一格都没起过的是 `None`。
fn conversation_sessions(
    conversation: &WorkspaceConversation,
    states: &HashMap<String, DaemonSessionState>,
) -> Option<(Vec<String>, String)> {
    // 名字运行时算好了，同一段对话里每一格报的都一样。还没推来（旧运行时）写通用名。
    let (ids, fallback): (Vec<String>, &str) = match &conversation.acp {
        Some(acp) => (vec![acp.sid.clone()], "对话"),
        None => (
            conversation
                .terminals
                .iter()
                .filter_map(|terminal| terminal.id.clone())
                .collect(),
            "终端",
        ),
    };
    if ids.is_empty() {
        return None;
    }
    let title = ids
        .iter()
        .find_map(|id| states.get(id)?.conversation_name.clone())
        .unwrap_or_else(|| fallback.to_string());
    Some((ids, title))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use smelt_plugin_api::RuntimeKey;
    use smelt_plugin_api::model::control_api::{
        ProcessEntry, ProcessUsage, ResourceUsageResult, SessionResourceUsage,
    };
    use smelt_plugin_api::model::daemon_state::DaemonSessionState;
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceTerminal,
    };

    use super::{
        ResourceMonitor, SessionGroup, SortBy, format_bytes, format_cpu, group_sessions,
        memory_series, process_summary, session_owners,
    };

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
        monitor.apply(Some((usage(1., 10, 1), None)), Vec::new(), start);
        let later = start + Duration::from_secs(600);
        monitor.apply(Some((usage(1., 20, 1), None)), Vec::new(), later);
        assert_eq!(monitor.history.len(), 1);
        assert_eq!(monitor.chart, vec![20.]);
    }

    /// 隔了一阵再回到资源页，上次的数和曲线不能接着画：中间那段没人量过。
    #[test]
    fn coming_back_starts_from_a_clean_slate() {
        let start = Instant::now();
        let mut monitor = ResourceMonitor::default();
        monitor.apply(Some((usage(1., 10, 1), None)), Vec::new(), start);
        monitor.reset();
        assert_eq!(monitor.local_total(), None);
        assert!(monitor.history.is_empty() && monitor.chart.is_empty());
        monitor.apply(
            Some((usage(1., 30, 1), None)),
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
        };
        monitor.apply(
            Some((usage(4., 40, 1), None)),
            vec![(RuntimeKey::Local, Ok(local))],
            Instant::now(),
        );
        assert_eq!(monitor.local_total(), Some(usage(7., 70, 5)));
    }

    /// 归档的对话不算；终端分屏的几格归到同一段对话，没起过的格不占 id；名字认运行时报的。
    #[test]
    fn owners_follow_the_directory_conversations() {
        let terminal = |id: Option<&str>| WorkspaceTerminal {
            id: id.map(str::to_string),
            ..Default::default()
        };
        let conversations = vec![
            WorkspaceConversation {
                custom_title: Some("旧的".to_string()),
                archived_at: Some(1),
                acp: Some(WorkspaceAcp {
                    sid: "archived".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            WorkspaceConversation {
                acp: Some(WorkspaceAcp {
                    sid: "acp".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            WorkspaceConversation {
                terminals: vec![terminal(Some("a")), terminal(None), terminal(Some("b"))],
                ..Default::default()
            },
            WorkspaceConversation {
                terminals: vec![terminal(None)],
                ..Default::default()
            },
        ];
        let named = |id: &str, name: &str| DaemonSessionState {
            id: id.to_string(),
            conversation_name: Some(name.to_string()),
            ..Default::default()
        };
        let owners = session_owners(conversations, vec![named("b", "分屏")]);
        assert_eq!(owners.len(), 3);
        assert!(!owners.contains_key("archived"));
        // 还没推名字来：通用名。
        assert_eq!(owners["acp"], (1, "对话".to_string()));
        // 哪格报上来都认。
        assert_eq!(owners["a"], (2, "分屏".to_string()));
        assert_eq!(owners["b"], (2, "分屏".to_string()));
    }

    #[test]
    fn numbers_read_like_activity_monitor() {
        assert_eq!(format_bytes(512 * 1024), "512 KB");
        assert_eq!(format_bytes(820 * 1024 * 1024), "820 MB");
        assert_eq!(format_bytes(1288 * 1024 * 1024), "1.3 GB");
        assert_eq!(format_cpu(0.34), "0.3%");
        assert_eq!(format_cpu(135.4), "135%");
    }
}
