//! 进程的资源占用：CPU 和内存，单个进程或者以它为根的整棵进程树。
//!
//! CPU 是这一次和上一次采样之间的平均占用，按单核计：和活动监视器、`top` 一样，多核跑满
//! 可以超过 100%。所以采样器要留着反复用；一个进程第一次被看到时没有上一次可比，CPU 记 0。
//!
//! 内存取各平台活动监视器给出的那个数。macOS 是 `phys_footprint`：进程自己弄脏、它退出就会
//! 还回去的内存。常驻集（RSS）把映射进来的共享库只读页也算在每个进程头上，GPUI 这样挂着
//! 整个图形栈的进程会凭空多出几百兆，一棵树求和还会把同一份库算好几遍。Linux、Windows 取
//! 常驻集（`top` 的 RES、任务管理器的工作集），偏大，但跟用户在那两个工具里看到的一致。

use std::collections::{HashMap, HashSet};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// 一个进程或一棵进程树的占用。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// 按单核计的百分比，多核可以超过 100。
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// 算进来的进程数。
    pub processes: u32,
}

/// 树里的一个进程，给人认出它是谁。
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// 进程名（`node`、`cargo`……）。
    pub name: String,
    /// 命令行，参数用空格连起来；读不到（权限、已退出）是空的。
    pub command: String,
    /// 只算这一个进程。
    pub usage: Usage,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.cpu_percent += other.cpu_percent;
        self.memory_bytes += other.memory_bytes;
        self.processes += other.processes;
    }
}

/// 常驻的采样器。CPU 要跟上一次比，同一个采样器反复用才有数。
pub struct ProcessSampler {
    system: System,
    total_memory: Option<u64>,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            total_memory: None,
        }
    }

    /// 只看一个进程，不扫整张进程表。进程不在了是 `None`。
    pub fn sample_process(&mut self, pid: u32) -> Option<Usage> {
        let pid = Pid::from_u32(pid);
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh_kind());
        self.system.process(pid).map(single_usage)
    }

    /// 扫一遍整张进程表，再从返回的表里按根进程取各棵树。
    pub fn refresh_all(&mut self) -> ProcessTable<'_> {
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in self.system.processes() {
            if let Some(parent) = process.parent()
                && parent != *pid
            {
                children.entry(parent).or_default().push(*pid);
            }
        }
        ProcessTable {
            system: &self.system,
            children,
        }
    }

    /// 这台机器的物理内存总量。不会变，只读一次。
    pub fn total_memory(&mut self) -> u64 {
        *self.total_memory.get_or_insert_with(|| {
            self.system.refresh_memory();
            self.system.total_memory()
        })
    }
}

/// 一次 [`ProcessSampler::refresh_all`] 之后的进程表。
pub struct ProcessTable<'a> {
    system: &'a System,
    children: HashMap<Pid, Vec<Pid>>,
}

impl ProcessTable<'_> {
    /// 这一个进程，不含子进程。
    pub fn process(&self, pid: u32) -> Option<Usage> {
        self.system.process(Pid::from_u32(pid)).map(single_usage)
    }

    /// 以 `root` 为根的整棵树的合计。根进程不在了是 `None`。
    pub fn tree(&self, root: u32) -> Option<Usage> {
        let processes = self.tree_processes(root)?;
        let mut total = Usage::default();
        for process in &processes {
            total.add(process.usage);
        }
        Some(total)
    }

    /// 以 `root` 为根的整棵树里的每个进程，内存大的在前。根进程不在了是 `None`。
    pub fn tree_processes(&self, root: u32) -> Option<Vec<ProcessInfo>> {
        let root = Pid::from_u32(root);
        self.system.process(root)?;
        let mut processes = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(process) = self.system.process(pid) {
                processes.push(ProcessInfo {
                    pid: pid.as_u32(),
                    name: process.name().to_string_lossy().into_owned(),
                    command: process
                        .cmd()
                        .iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                    usage: single_usage(process),
                });
            }
            if let Some(kids) = self.children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
        processes.sort_by_key(|process| std::cmp::Reverse(process.usage.memory_bytes));
        Some(processes)
    }
}

/// CPU、内存和命令行。命令行一个进程只读一次（它不会变），之后的刷新不再碰。Linux 上
/// 线程也是 `/proc` 里的条目，不要把它们当成进程再算一遍。
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_cmd(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

fn single_usage(process: &Process) -> Usage {
    Usage {
        cpu_percent: process.cpu_usage(),
        memory_bytes: private_memory(process.pid()).unwrap_or_else(|| process.memory()),
        processes: 1,
    }
}

#[cfg(target_os = "macos")]
fn private_memory(pid: Pid) -> Option<u64> {
    let pid = i32::try_from(pid.as_u32()).ok()?;
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: info 是一份完整的 rusage_info_v2，flavor 与之对应，内核最多写这么多字节。
    let read = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            (&mut info as *mut libc::rusage_info_v2).cast(),
        )
    };
    (read == 0).then_some(info.ri_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
fn private_memory(_pid: Pid) -> Option<u64> {
    None
}

/// 本进程的内存拆成几块，加起来是 `total`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryBreakdown {
    /// 和 [`Usage::memory_bytes`] 同一个口径（macOS 的 `phys_footprint`），同一刻读的。
    pub total: u64,
    /// GPU 用的画面缓冲和纹理：窗口的 drawable（IOSurface）、Metal 纹理、图集。跟窗口的像素
    /// 面积成正比。
    pub graphics: u64,
    /// malloc 堆上正在用的字节：程序自己的数据。
    pub heap: u64,
}

impl MemoryBreakdown {
    /// 剩下的：系统库的可写数据段、线程栈、堆里已弄脏但空着的页。
    pub fn other(&self) -> u64 {
        self.total
            .saturating_sub(self.graphics)
            .saturating_sub(self.heap)
    }
}

/// 本进程的内存拆分。一次 `task_info` 加一次 malloc 统计，微秒级。只有 macOS 记了这本账；
/// 别的平台是 `None`。
///
/// 内核按账本记：Metal 纹理记在 graphics 标签下；IOSurface（CAMetalLayer 的 drawable 就是
/// 它）记在可清除但未标记可清除（purgeable nonvolatile）那一栏，不进 graphics 标签。两栏
/// 不重叠，都算进 `phys_footprint`。malloc 只进 internal，跟这两栏也不重叠。
#[cfg(target_os = "macos")]
pub fn own_memory() -> Option<MemoryBreakdown> {
    use mach2::task_info::{TASK_VM_INFO, task_vm_info};
    use mach2::vm_types::natural_t;

    let mut info = task_vm_info::default();
    let mut count = (std::mem::size_of::<task_vm_info>() / std::mem::size_of::<natural_t>()) as u32;
    // SAFETY: info 是一份完整的 task_vm_info，count 按它的大小给，内核最多写这么多。
    let status = unsafe {
        mach2::task::task_info(
            mach2::traps::mach_task_self(),
            TASK_VM_INFO,
            (&mut info as *mut task_vm_info).cast(),
            &mut count,
        )
    };
    // 账本那几栏是后来的版本才有的：老内核回得短，就不拆。
    let ledger_end = std::mem::offset_of!(task_vm_info, ledger_tag_neural_nofootprint_compressed)
        + std::mem::size_of::<i64>();
    if status != mach2::kern_return::KERN_SUCCESS
        || (count as usize) * std::mem::size_of::<natural_t>() < ledger_end
    {
        return None;
    }
    let bytes = |value: i64| u64::try_from(value).unwrap_or(0);
    let graphics = bytes(info.ledger_tag_graphics_footprint)
        + bytes(info.ledger_tag_graphics_footprint_compressed)
        + bytes(info.ledger_purgeable_nonvolatile)
        + bytes(info.ledger_purgeable_novolatile_compressed);
    let mut stats: libc::malloc_statistics_t = unsafe { std::mem::zeroed() };
    // SAFETY: 空 zone 表示所有 zone 合计；stats 是完整的一份。
    unsafe { libc::malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    Some(MemoryBreakdown {
        total: info.phys_footprint,
        graphics,
        heap: stats.size_in_use as u64,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn own_memory() -> Option<MemoryBreakdown> {
    None
}

/// 本进程从启动到此刻累计的资源用量。只算本进程（含所有线程），不含子进程。
/// 读不到或平台不支持的字段是 `None`，不拿 0 充数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LifetimeUsage {
    /// 用户态 + 内核态 CPU 时间，微秒。
    /// macOS / Linux：`getrusage(RUSAGE_SELF)` 的 `ru_utime + ru_stime`；
    /// Windows：`GetProcessTimes` 的 user + kernel。
    pub cpu_time_us: Option<u64>,
    /// 内存峰值，字节，和 [`Usage::memory_bytes`] 的实时口径对应：
    /// macOS：`proc_pid_rusage(V4).ri_lifetime_max_phys_footprint`（峰值 phys_footprint）；
    /// Linux：`/proc/self/status` 的 `VmHWM`（本次 exec 之后的峰值常驻集）。不用
    /// `ru_maxrss`：它跨 exec 保留，vfork/posix_spawn 出来的进程会带上父进程的峰值；
    /// Windows：`PeakWorkingSetSize`（峰值工作集）。
    pub peak_memory_bytes: Option<u64>,
}

/// 读本进程的 [`LifetimeUsage`]。几次系统调用，不阻塞。
pub fn own_lifetime_usage() -> LifetimeUsage {
    LifetimeUsage {
        cpu_time_us: own_cpu_time_us(),
        peak_memory_bytes: own_peak_memory_bytes(),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn own_cpu_time_us() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: usage 是一份完整的 rusage。
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    let us = |t: libc::timeval| -> Option<u64> {
        let secs = u64::try_from(t.tv_sec).ok()?;
        let micros = u64::try_from(t.tv_usec).ok()?;
        secs.checked_mul(1_000_000)?.checked_add(micros)
    };
    us(usage.ru_utime)?.checked_add(us(usage.ru_stime)?)
}

#[cfg(windows)]
fn own_cpu_time_us() -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: 伪句柄 GetCurrentProcess 无需关闭；四个输出都是完整的 FILETIME。
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    // FILETIME 单位是 100 纳秒。
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some(ticks(kernel).checked_add(ticks(user))? / 10)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn own_cpu_time_us() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn own_peak_memory_bytes() -> Option<u64> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: info 是一份完整的 rusage_info_v4，flavor 与之对应。
    let read = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast(),
        )
    };
    (read == 0 && info.ri_lifetime_max_phys_footprint > 0).then_some(info.ri_lifetime_max_phys_footprint)
}

#[cfg(target_os = "linux")]
fn own_peak_memory_bytes() -> Option<u64> {
    parse_vm_hwm(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// `/proc/<pid>/status` 里的 `VmHWM:   12345 kB`。
#[cfg(any(target_os = "linux", test))]
fn parse_vm_hwm(status: &str) -> Option<u64> {
    let line = status.lines().find_map(|line| line.strip_prefix("VmHWM:"))?;
    let mut parts = line.split_whitespace();
    let kib: u64 = parts.next()?.parse().ok()?;
    if parts.next()? != "kB" {
        return None;
    }
    kib.checked_mul(1024).filter(|bytes| *bytes > 0)
}

#[cfg(windows)]
fn own_peak_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    counters.cb = size;
    // SAFETY: counters 是完整的一份，cb 给的就是它的大小。
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (ok != 0 && counters.PeakWorkingSetSize > 0).then_some(counters.PeakWorkingSetSize as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn own_peak_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_hwm_is_parsed_in_bytes_and_bad_lines_are_none() {
        assert_eq!(parse_vm_hwm("Name:\tx\nVmHWM:\t   2048 kB\nVmRSS:\t 1 kB\n"), Some(2048 * 1024));
        assert_eq!(parse_vm_hwm("VmRSS:\t 1 kB\n"), None);
        assert_eq!(parse_vm_hwm("VmHWM:\t 0 kB\n"), None);
        assert_eq!(parse_vm_hwm("VmHWM:\t 12 MB\n"), None);
        assert_eq!(parse_vm_hwm("VmHWM:\t abc kB\n"), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn own_lifetime_usage_is_available_and_monotonic_on_supported_platforms() {
        let before = own_lifetime_usage();
        let cpu_before = before.cpu_time_us.expect("cpu time");
        assert!(before.peak_memory_bytes.expect("peak memory") > 0);
        let buf = vec![1u8; 32 * 1024 * 1024];
        let mut sum = 0u64;
        for _ in 0..4 {
            sum = buf.iter().fold(sum, |acc, b| acc.wrapping_add(u64::from(*b)));
        }
        std::hint::black_box(sum);
        let after = own_lifetime_usage();
        assert!(after.cpu_time_us.unwrap() >= cpu_before);
        assert!(after.peak_memory_bytes.unwrap() >= before.peak_memory_bytes.unwrap());
        assert!(after.peak_memory_bytes.unwrap() >= 32 * 1024 * 1024);
    }

    #[test]
    fn a_process_reports_memory_and_a_missing_one_is_none() {
        let mut sampler = ProcessSampler::new();
        let me = sampler
            .sample_process(std::process::id())
            .expect("自己一定在");
        assert_eq!(me.processes, 1);
        assert!(me.memory_bytes > 0);
        // pid 上限远小于 u32::MAX，这个 pid 不会有进程。
        assert_eq!(sampler.sample_process(u32::MAX - 1), None);
        assert!(sampler.total_memory() > 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn own_memory_splits_the_footprint_and_sees_the_heap_grow() {
        let before = own_memory().expect("macOS 一定有");
        assert!(before.heap > 0 && before.heap + before.graphics <= before.total);
        let block = vec![1u8; 64 << 20];
        let after = own_memory().expect("macOS 一定有");
        assert!(
            after.heap >= before.heap + (64 << 20),
            "{before:?} -> {after:?}"
        );
        assert!(after.total >= before.total + (64 << 20), "{before:?} -> {after:?}");
        drop(block);
    }

    #[test]
    fn other_is_what_is_left() {
        let memory = MemoryBreakdown {
            total: 100,
            graphics: 60,
            heap: 30,
        };
        assert_eq!(memory.other(), 10);
        // 两次读之间有出入时不倒挂。
        let skewed = MemoryBreakdown {
            total: 50,
            graphics: 60,
            heap: 30,
        };
        assert_eq!(skewed.other(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_tree_counts_the_children_but_the_process_alone_does_not() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("起子进程");
        let mut sampler = ProcessSampler::new();
        let table = sampler.refresh_all();
        let me = std::process::id();
        let alone = table.process(me).expect("自己一定在");
        let tree = table.tree(me).expect("自己一定在");
        let sleeper = table.tree(child.id()).expect("子进程还在跑");
        assert_eq!(alone.processes, 1);
        assert!(tree.processes >= 2, "树里应有子进程：{tree:?}");
        assert!(tree.memory_bytes >= alone.memory_bytes + sleeper.memory_bytes);
        let listed = table.tree_processes(me).expect("自己一定在");
        assert_eq!(listed.len() as u32, tree.processes);
        let sleep = listed
            .iter()
            .find(|process| process.pid == child.id())
            .expect("列表里应有子进程");
        assert_eq!(sleep.name, "sleep");
        assert!(sleep.command.ends_with("sleep 30"), "{}", sleep.command);
        child.kill().ok();
        child.wait().ok();
    }
}
