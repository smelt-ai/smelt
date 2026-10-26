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

#[cfg(test)]
mod tests {
    use super::*;

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
