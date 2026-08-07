//! 本地任务数据模型与纯逻辑：smeltd 持有任务权威状态（唯一写者），GUI 与
//! `smelt-task` CLI 通过 socket op 读写。本模块不含 GPUI，不引 UI。
//!
//! - 模型：`Task` / `TaskRun` / 枚举 —— 与 `crates/smelt/src/tasks.rs` 的副本保持
//!   同一套 serde 形状（旧数据兼容靠 `#[serde(default)]`）。
//! - 落盘：`~/.smelt/tasks.json`（复用 `json_store` 读宽容/写静默语义）。
//! - 纯业务：`pick_claimable` / `begin_run_in_file` / `apply_failure_to_task` 等
//!   全部是「load → 改 → save」的磁盘操作，smeltd 的 op handler 直接调用。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::json_store;

// ===================== 模型 =====================

/// 任务列。UI 只暴露三态：**待办 / 执行中 / 完成**。
/// `ready` / `waiting` 仍可从旧 tasks.json 读入，展示时归并到待办 / 执行中。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskColumn {
    #[default]
    Backlog,
    Ready,
    Running,
    Waiting,
    Review,
    Failed,
    Done,
}

impl TaskColumn {
    /// 展示用三态标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Backlog | Self::Ready => "待办",
            Self::Running | Self::Waiting => "执行中",
            Self::Review => "待审查",
            Self::Failed => "失败",
            Self::Done => "完成",
        }
    }

    /// 是否算「执行中」（含旧 waiting）。
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }

    /// 是否算「待办」（含旧 ready）。
    pub fn is_todo(self) -> bool {
        matches!(self, Self::Backlog | Self::Ready)
    }
}

/// 执行通道。`Pty` = 交互终端（startup-arg 首包）；`Acp` = ACP 结构化对话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskChannel {
    Pty,
    Acp {
        agent: String,
        profile_id: Option<String>,
    },
}

impl Default for TaskChannel {
    fn default() -> Self {
        Self::Pty
    }
}

impl Serialize for TaskChannel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("TaskChannel", 1)?;
        match self {
            Self::Pty => st.serialize_field("pty", &())?,
            Self::Acp { agent, profile_id } => {
                st.serialize_field("acp", &AcpChannel { agent, profile_id })?
            }
        }
        st.end()
    }
}

#[derive(Serialize)]
struct AcpChannel<'a> {
    agent: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_id: &'a Option<String>,
}

impl<'de> Deserialize<'de> for TaskChannel {
    /// 兼容旧数据（`"pty"`）与手改的裸 `"acp"` 字符串。
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Repr {
            Pty,
            Acp {
                #[serde(default)]
                agent: String,
                #[serde(default)]
                profile_id: Option<String>,
            },
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            String(String),
            Map(Repr),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Map(Repr::Pty) => Self::Pty,
            Raw::Map(Repr::Acp { agent, profile_id }) => Self::Acp { agent, profile_id },
            Raw::String(s) if s == "acp" => Self::Acp { agent: String::new(), profile_id: None },
            Raw::String(_) => Self::Pty,
        })
    }
}

/// 一次具体执行；Task 保存用户目标，TaskRun 保存每次尝试的现场与结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskRunStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "启动中",
            Self::Running => "执行中",
            Self::Completed => "已交付",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub attempt: u32,
    /// 本次实际执行时选择的通道；不属于 Task 的长期配置。
    pub channel: TaskChannel,
    /// 本次实际执行时解析出的启动命令快照。
    pub launch: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub status: TaskRunStatus,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub finished_at: Option<u64>,
}

/// 已指定时间任务的重复频率。`Once` 保持既有单次执行语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskScheduleFrequency {
    #[default]
    Once,
    Hourly,
    Daily,
}

impl TaskScheduleFrequency {
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "仅一次",
            Self::Hourly => "每小时",
            Self::Daily => "每天",
        }
    }

    pub fn is_recurring(self) -> bool {
        !matches!(self, Self::Once)
    }
}

/// 任务类型：普通（按队列）/ 指定时间开始执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Once,
    Scheduled,
}

/// 失败自动重试策略（任务级）。`max_attempts=1` = 不重试；`0` = 无限。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRetryPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub retry_delay_secs: u64,
    #[serde(default)]
    pub remix_on_retry: bool,
}

impl Default for TaskRetryPolicy {
    fn default() -> Self {
        Self { max_attempts: default_max_attempts(), retry_delay_secs: 0, remix_on_retry: false }
    }
}

impl TaskRetryPolicy {
    /// `attempt` 是已完成的尝试编号（1-based）。已用尝试 < max_attempts 才继续。
    pub fn allows_retry(self, attempt: u32) -> bool {
        self.max_attempts == 0 || attempt < self.max_attempts
    }
}

fn default_max_attempts() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// 本地任务。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub column: TaskColumn,
    pub project_cwd: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub current_run_id: Option<String>,
    #[serde(default)]
    pub kind: TaskKind,
    /// 指定时间任务的重复频率。旧任务缺省为单次，保持原有行为。
    #[serde(default)]
    pub schedule_frequency: TaskScheduleFrequency,
    #[serde(default)]
    pub run_at: Option<u64>,
    #[serde(default = "default_true")]
    pub auto_run: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub retry_policy: TaskRetryPolicy,
    #[serde(default)]
    pub retry_at: Option<u64>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

impl Task {
    pub fn new(project_cwd: String, title: String, body: String) -> Self {
        let now = now_secs();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            body,
            column: TaskColumn::Backlog,
            project_cwd,
            session_id: None,
            current_run_id: None,
            kind: TaskKind::Once,
            schedule_frequency: TaskScheduleFrequency::Once,
            run_at: None,
            auto_run: true,
            depends_on: Vec::new(),
            retry_policy: TaskRetryPolicy::default(),
            retry_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 指定时间任务是否已到点（`run_at <= now`）且允许自动执行。
    pub fn is_due(&self, now: u64) -> bool {
        self.auto_run
            && self.kind == TaskKind::Scheduled
            && self.column.is_todo()
            && self.run_at.map(|at| at <= now).unwrap_or(false)
    }

    pub fn has_recurring_schedule(&self) -> bool {
        self.kind == TaskKind::Scheduled && self.schedule_frequency.is_recurring()
    }

    /// 前置依赖是否全部满足：`depends_on` 引用的任务都处于 Done。
    /// 自引用与已删除的依赖视同满足（防死锁 / 不因删任务卡死队列）。
    pub fn dependencies_met(&self, tasks: &[Task]) -> bool {
        let self_id = self.id.as_str();
        self.depends_on.iter().all(|id| {
            if id == self_id {
                return true;
            }
            match tasks.iter().find(|t| t.id == *id) {
                Some(dep) => dep.column == TaskColumn::Done,
                None => true,
            }
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskFile {
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub runs: Vec<TaskRun>,
}

// ===================== 落盘 =====================

pub fn tasks_global_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("tasks.json"))
}

pub fn tasks_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("tasks"))
}

pub fn load_tasks_file() -> TaskFile {
    json_store::load_json(tasks_global_path())
}

pub fn save_tasks_file(file: &TaskFile) {
    json_store::save_json(tasks_global_path(), file);
}

/// 把首包 prompt 落到磁盘，供 `$(cat …)` 塞进 launch（多行/引号安全）。
pub fn write_prompt_file(task_id: &str, prompt: &str) -> Option<PathBuf> {
    let dir = tasks_dir()?.join("prompts");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{task_id}.txt"));
    std::fs::write(&path, prompt).ok()?;
    Some(path)
}

/// 同步向 smeltd 发一个 task op（GUI 侧用；smeltd 是服务端不调它）。
/// 连不上 / 返回 `ok:false` → `Err`。
pub fn request_task_op(op: serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(crate::daemon_state::smeltd_sock_path())
        .map_err(|e| format!("smeltd unavailable: {e}"))?;
    writeln!(stream, "{op}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim().is_empty() {
        return Err("smeltd 无响应".into());
    }
    let response: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(response
            .get("err")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("任务操作失败")
            .to_string());
    }
    Ok(response)
}

// ===================== 纯逻辑 =====================

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 计算重复任务下一次可执行时间。按原计划时间对齐而不是按完成时间漂移；错过的
/// 时段会跳过，避免恢复应用或项目解锁后连续补跑多次旧任务。
pub fn next_scheduled_run_at(
    frequency: TaskScheduleFrequency,
    scheduled_at: Option<u64>,
    now: u64,
) -> Option<u64> {
    let scheduled_at = scheduled_at.unwrap_or(now);
    if scheduled_at > now {
        return Some(scheduled_at);
    }

    match frequency {
        TaskScheduleFrequency::Once => None,
        TaskScheduleFrequency::Hourly => {
            let elapsed = now.saturating_sub(scheduled_at);
            let periods = elapsed / 3_600 + 1;
            Some(scheduled_at.saturating_add(periods.saturating_mul(3_600)))
        }
        TaskScheduleFrequency::Daily => {
            use chrono::{Local, LocalResult, TimeZone};

            let Some(anchor) = Local.timestamp_opt(scheduled_at as i64, 0).single() else {
                return Some(now.saturating_add(86_400));
            };
            let Some(current) = Local.timestamp_opt(now as i64, 0).single() else {
                return Some(now.saturating_add(86_400));
            };

            let mut date = current.date_naive();
            for _ in 0..8 {
                let local = Local.from_local_datetime(&date.and_time(anchor.time()));
                let timestamp = match local {
                    LocalResult::Single(time) => time.timestamp().max(0) as u64,
                    LocalResult::Ambiguous(earlier, _) => earlier.timestamp().max(0) as u64,
                    LocalResult::None => {
                        date = date.succ_opt()?;
                        continue;
                    }
                };
                if timestamp > now {
                    return Some(timestamp);
                }
                date = date.succ_opt()?;
            }
            Some(now.saturating_add(86_400))
        }
    }
}

fn advance_task_after_success(task: &mut Task, now: u64) {
    task.session_id = None;
    task.retry_at = None;
    task.updated_at = now;
    if task.has_recurring_schedule() {
        task.column = TaskColumn::Backlog;
        task.run_at = next_scheduled_run_at(task.schedule_frequency, task.run_at, now);
    } else {
        task.column = TaskColumn::Review;
    }
}

/// 把一次失败落到 Run + Task（见 GUI 侧同名 helper 的语义注释）。
pub fn apply_failure_to_task(task: &mut Task, run: &mut TaskRun, error: &str, now: u64) {
    run.status = TaskRunStatus::Failed;
    run.error = Some(error.to_string());
    run.finished_at = Some(now);
    if task.current_run_id.as_deref() == Some(run.id.as_str()) {
        task.session_id = None;
        task.updated_at = now;
        if task.retry_policy.allows_retry(run.attempt) {
            task.column = TaskColumn::Backlog;
            task.retry_at = if task.retry_policy.retry_delay_secs > 0 {
                Some(now + task.retry_policy.retry_delay_secs)
            } else {
                None
            };
        } else if task.has_recurring_schedule() {
            task.column = TaskColumn::Backlog;
            task.retry_at = None;
            task.run_at = next_scheduled_run_at(task.schedule_frequency, task.run_at, now);
        } else {
            task.column = TaskColumn::Failed;
            task.retry_at = None;
        }
    }
}

/// 由 agent 明确上报完成时收尾当前任务。重复任务会保留任务定义并安排下一次，
/// 单次任务保持既有的待审查流转。
pub fn mark_task_done_in_file(file: &mut TaskFile, task_id: &str, now: u64) -> bool {
    let Some(task_index) = file.tasks.iter().position(|task| task.id == task_id) else {
        return false;
    };
    let current_run_id = file.tasks[task_index].current_run_id.clone();
    if let Some(run_id) = current_run_id
        && let Some(run_index) = file
            .runs
            .iter()
            .position(|run| run.id == run_id && run.status.is_active())
    {
        let task = &mut file.tasks[task_index];
        let run = &mut file.runs[run_index];
        run.status = TaskRunStatus::Completed;
        run.finished_at = Some(now);
        advance_task_after_success(task, now);
        return true;
    }

    let task = &mut file.tasks[task_index];
    advance_task_after_success(task, now);
    true
}

/// 任务此刻是否可被**系统**自动取跑（待办 + `auto_run`；依赖满足 + 重试冷却过；
/// 指定时间任务须已到期）。人手点「运行」不走此判断。
pub fn is_auto_runnable(t: &Task, tasks: &[Task], now: u64) -> bool {
    if !t.auto_run || !t.column.is_todo() {
        return false;
    }
    if !t.dependencies_met(tasks) {
        return false;
    }
    if let Some(at) = t.retry_at {
        if now < at {
            return false;
        }
    }
    match t.kind {
        TaskKind::Once => true,
        TaskKind::Scheduled => t.run_at.map(|at| at <= now).unwrap_or(false),
    }
}

/// 该项目是否已有执行中任务（同 cwd 串行 worker）。
pub fn has_running_for_cwd(file: &TaskFile, cwd: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    file.tasks
        .iter()
        .any(|t| t.column.is_active() && t.project_cwd.trim_end_matches('/') == cwd)
}

/// 原子选取下一条可自动执行的任务下标：同 cwd 无 active、依赖满足、重试冷却过、
/// `auto_run`、待办，FIFO（`created_at` 升序）。返回 `Some(usize)`。
pub fn pick_claimable(tasks: &[Task], prefer_cwd: &str, now: u64) -> Option<usize> {
    let prefer = prefer_cwd.trim_end_matches('/');
    if prefer.is_empty() {
        return None;
    }
    if tasks
        .iter()
        .any(|t| t.column.is_active() && t.project_cwd.trim_end_matches('/') == prefer)
    {
        return None;
    }
    let mut idxs: Vec<usize> = tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.project_cwd.trim_end_matches('/') == prefer && is_auto_runnable(t, tasks, now))
        .map(|(i, _)| i)
        .collect();
    idxs.sort_by_key(|&i| tasks[i].created_at);
    idxs.first().copied()
}

/// 为任务创建一次执行尝试。开跑即清重试冷却；旧 active Run 以「被新执行替代」收尾。
pub fn begin_run_in_file(
    file: &mut TaskFile,
    task_id: &str,
    launch: &str,
    channel: TaskChannel,
    now: u64,
) -> Option<TaskRun> {
    let attempt = file
        .runs
        .iter()
        .filter(|run| run.task_id == task_id)
        .map(|run| run.attempt)
        .max()
        .unwrap_or(0)
        + 1;

    for run in file
        .runs
        .iter_mut()
        .filter(|run| run.task_id == task_id && run.status.is_active())
    {
        run.status = TaskRunStatus::Failed;
        run.error = Some("被新的执行尝试替代".into());
        run.finished_at = Some(now);
    }

    let run = TaskRun {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        attempt,
        channel,
        launch: launch.to_string(),
        session_id: None,
        status: TaskRunStatus::Starting,
        error: None,
        created_at: now,
        started_at: None,
        finished_at: None,
    };
    let task = file.tasks.iter_mut().find(|task| task.id == task_id)?;
    task.column = TaskColumn::Running;
    task.current_run_id = Some(run.id.clone());
    task.retry_at = None;
    task.updated_at = now;
    file.runs.push(run.clone());
    Some(run)
}

/// 执行现场启动失败：保留失败 Run，按重试策略回待办（冷却）或落 Failed 列。
/// 返回受影响任务的项目 cwd（Some）用于触发续跑/重试。
pub fn mark_run_failed_in_file(
    file: &mut TaskFile,
    task_id: &str,
    run_id: &str,
    error: &str,
    now: u64,
) -> Option<String> {
    let cwd = file
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| t.project_cwd.clone());
    let Some(run) = file
        .runs
        .iter_mut()
        .find(|run| run.id == run_id && run.task_id == task_id)
    else {
        return None;
    };
    let Some(task) = file.tasks.iter_mut().find(|task| task.id == task_id) else {
        return None;
    };
    apply_failure_to_task(task, run, error, now);
    cwd
}

/// agent 回合失败（结构化 phase=Failed 边沿）：把该会话绑定的活跃任务按重试策略处理。
/// 语义对齐 `mark_session_done_in_file`，返回 `Some(cwd)`。
pub fn mark_session_failed_in_file(
    file: &mut TaskFile,
    session_id: &str,
    error: &str,
    now: u64,
) -> Option<String> {
    let mut cwd: Option<String> = None;
    for t in &mut file.tasks {
        if t.session_id.as_deref() != Some(session_id) {
            continue;
        }
        if !t.column.is_active() {
            continue;
        }
        if let Some(run_id) = t.current_run_id.clone()
            && let Some(run) = file.runs.iter_mut().find(|run| {
                run.id == run_id
                    && run.session_id.as_deref() == Some(session_id)
                    && run.status.is_active()
            })
        {
            apply_failure_to_task(t, run, error, now);
        }
        if cwd.is_none() {
            cwd = Some(t.project_cwd.clone());
        }
    }
    cwd
}

/// 终端 agent 停转（完成一轮）时：单次任务进入待审查，重复任务安排下一次执行。
/// 返回 `Some(project_cwd)` 表示确实收尾了至少一条任务。
pub fn mark_session_done_in_file(
    file: &mut TaskFile,
    session_id: &str,
    now: u64,
) -> Option<String> {
    let mut done_cwd: Option<String> = None;
    for task_index in 0..file.tasks.len() {
        if file.tasks[task_index].session_id.as_deref() != Some(session_id) {
            continue;
        }
        if matches!(
            file.tasks[task_index].column,
            TaskColumn::Running | TaskColumn::Waiting
        ) {
            let run_id = file.tasks[task_index].current_run_id.clone();
            if let Some(run_id) = run_id
                && let Some(run_index) = file.runs.iter().position(|run| {
                    run.id == run_id
                        && run.session_id.as_deref() == Some(session_id)
                        && run.status.is_active()
                })
            {
                let task = &mut file.tasks[task_index];
                let run = &mut file.runs[run_index];
                run.status = TaskRunStatus::Completed;
                run.finished_at = Some(now);
                advance_task_after_success(task, now);
            } else {
                advance_task_after_success(&mut file.tasks[task_index], now);
            }
            if done_cwd.is_none() {
                done_cwd = Some(file.tasks[task_index].project_cwd.clone());
            }
        }
    }
    done_cwd
}

/// 已到期、可自动执行的指定时间任务 id（按 `run_at` 升序），依赖未满足的不领取。
pub fn due_scheduled_ids_in_file(file: &TaskFile, now: u64) -> Vec<String> {
    let mut due: Vec<(u64, String)> = file
        .tasks
        .iter()
        .filter(|t| t.is_due(now) && t.dependencies_met(&file.tasks))
        .map(|t| (t.run_at.unwrap_or(0), t.id.clone()))
        .collect();
    due.sort_by_key(|(at, _)| *at);
    due.into_iter().map(|(_, id)| id).collect()
}

/// 重试冷却已到、可重新取跑的任务 id（按 `retry_at` 升序）。
pub fn due_retry_ids_in_file(file: &TaskFile, now: u64) -> Vec<String> {
    let mut due: Vec<(u64, String)> = file
        .tasks
        .iter()
        .filter(|t| {
            t.retry_at.is_some_and(|at| at <= now)
                && t.auto_run
                && t.column.is_todo()
                && t.dependencies_met(&file.tasks)
        })
        .map(|t| (t.retry_at.unwrap_or(0), t.id.clone()))
        .collect();
    due.sort_by_key(|(at, _)| *at);
    due.into_iter().map(|(_, id)| id).collect()
}

/// 任务的执行历史（attempt 降序）。
pub fn runs_for_task_in_file(file: &TaskFile, task_id: &str) -> Vec<TaskRun> {
    let mut runs: Vec<_> = file
        .runs
        .iter()
        .filter(|run| run.task_id == task_id)
        .cloned()
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.attempt));
    runs
}

/// 开跑时交给 agent 的首包：**只用 body**；body 空时才回退 title。
pub fn task_prompt(task: &Task) -> String {
    let body = task.body.trim();
    if !body.is_empty() {
        body.to_string()
    } else {
        task.title.trim().to_string()
    }
}

/// 解析本地时间字符串 → Unix 秒。支持 `YYYY-MM-DD HH:MM` / `YYYY-MM-DD HH:MM:SS`。
pub fn parse_local_datetime(s: &str) -> Option<u64> {
    use chrono::{Local, NaiveDateTime, TimeZone};
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .ok()?;
    let local = Local.from_local_datetime(&naive).single()?;
    Some(local.timestamp().max(0) as u64)
}

/// 从首包生成侧栏标题：首行非空，最长 40 字。
pub fn title_from_prompt(prompt: &str) -> String {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("未命名任务");
    if first.chars().count() > 40 {
        format!("{}…", first.chars().take(40).collect::<String>())
    } else {
        first.to_string()
    }
}

/// shell 单引号包裹（路径 / 内联短 prompt）。
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 交互启动：在 base launch 后追加 `"$(cat prompt)"` 作为 **CLI 首包参数**。
/// **不是** `claude -p` 无头批跑。
pub fn build_launch_with_prompt(base_launch: &str, prompt_path: &Path) -> String {
    let cat = format!(
        "\"$(cat {})\"",
        shell_single_quote(&prompt_path.display().to_string())
    );
    let base = base_launch.trim();
    if base.is_empty() {
        format!("claude {cat}")
    } else {
        format!("{base} {cat}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(cwd: &str, title: &str) -> Task {
        Task::new(cwd.into(), title.into(), "body".into())
    }

    #[test]
    fn model_json_roundtrip() {
        let mut t = task("/p", "t");
        t.column = TaskColumn::Running;
        t.kind = TaskKind::Scheduled;
        t.schedule_frequency = TaskScheduleFrequency::Hourly;
        t.run_at = Some(1_700_000_000);
        t.depends_on = vec!["dep".into()];
        t.retry_policy = TaskRetryPolicy { max_attempts: 3, retry_delay_secs: 60, remix_on_retry: true };
        let run = TaskRun {
            id: "r1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("s".into()),
            status: TaskRunStatus::Completed,
            error: None,
            created_at: 1,
            started_at: Some(2),
            finished_at: Some(3),
        };
        let file = TaskFile { tasks: vec![t.clone()], runs: vec![run] };
        let json = serde_json::to_string(&file).unwrap();
        let back: TaskFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tasks[0].depends_on, vec!["dep".to_string()]);
        assert_eq!(back.runs[0].channel, TaskChannel::Pty);
        assert_eq!(back.runs[0].status, TaskRunStatus::Completed);
        // 旧数据缺字段 → 默认值
        let old = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x"}]}"#;
        let old: TaskFile = serde_json::from_str(old).unwrap();
        assert!(old.tasks[0].depends_on.is_empty());
        assert!(old.tasks[0].auto_run);
        assert_eq!(old.tasks[0].schedule_frequency, TaskScheduleFrequency::Once);
    }

    #[test]
    fn legacy_task_agent_fields_are_discarded() {
        let old = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x","launch":"copilot","channel":{"acp":{"agent":"copilot"}}}]}"#;
        let file: TaskFile = serde_json::from_str(old).unwrap();
        let saved = serde_json::to_value(file).unwrap();
        let task = &saved["tasks"][0];
        assert!(task.get("launch").is_none());
        assert!(task.get("channel").is_none());
    }

    #[test]
    fn pick_claimable_respects_deps_and_retry() {
        let now = 1_000u64;
        let mut dep = task("/p", "dep");
        dep.column = TaskColumn::Backlog;
        dep.auto_run = false; // 只是阻塞条件，自身不参与候选
        let mut a = task("/p", "1");
        a.created_at = 10;
        a.depends_on = vec![dep.id.clone()];
        let mut b = task("/p", "2");
        b.created_at = 20;
        let tasks = vec![dep, a, b];
        // 依赖未满足的「1」被跳过，取「2」
        assert_eq!(pick_claimable(&tasks, "/p", now).map(|i| tasks[i].title.as_str()), Some("2"));

        // 重试冷却未过 → 不可取
        let mut c = task("/p", "3");
        c.retry_at = Some(now + 10);
        c.created_at = 5;
        let tasks = vec![c.clone()];
        assert!(pick_claimable(&tasks, "/p", now).is_none());
        let mut c2 = c;
        c2.retry_at = Some(now);
        assert!(pick_claimable(&[c2], "/p", now).is_some());
    }

    #[test]
    fn begin_run_clears_retry_and_marks_running() {
        let mut t = task("/p", "t");
        t.retry_at = Some(1_000);
        let mut file = TaskFile { tasks: vec![t.clone()], runs: Vec::new() };
        let run = begin_run_in_file(&mut file, &t.id, "claude", TaskChannel::Pty, 500).unwrap();
        assert_eq!(run.attempt, 1);
        assert_eq!(file.tasks[0].column, TaskColumn::Running);
        assert!(file.tasks[0].retry_at.is_none());
        assert_eq!(file.tasks[0].current_run_id.as_deref(), Some(run.id.as_str()));
    }

    #[test]
    fn apply_failure_retries_within_limit_then_exhausts() {
        let mut t = task("/p", "t");
        t.retry_policy = TaskRetryPolicy { max_attempts: 2, retry_delay_secs: 0, remix_on_retry: false };
        t.column = TaskColumn::Running;
        t.session_id = Some("s".into());
        let mut run = TaskRun {
            id: "r1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("s".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
        };
        t.current_run_id = Some(run.id.clone());
        apply_failure_to_task(&mut t, &mut run, "boom", 100);
        assert_eq!(t.column, TaskColumn::Backlog); // 未超限 → 回待办
        assert!(t.session_id.is_none());

        // 第二次失败（attempt=2 == max）→ 落 Failed
        run.attempt = 2;
        t.column = TaskColumn::Running;
        t.session_id = Some("s".into());
        t.current_run_id = Some(run.id.clone());
        apply_failure_to_task(&mut t, &mut run, "boom", 200);
        assert_eq!(t.column, TaskColumn::Failed);
    }

    #[test]
    fn mark_session_done_and_failed() {
        let mut t = task("/p", "t");
        t.column = TaskColumn::Running;
        t.session_id = Some("s1".into());
        let mut file = TaskFile { tasks: vec![t.clone()], runs: Vec::new() };
        let _run = begin_run_in_file(&mut file, &t.id, "claude", TaskChannel::Pty, 100).unwrap();
        // 绑会话
        file.tasks[0].session_id = Some("s1".into());
        file.runs[0].session_id = Some("s1".into());
        let cwd = mark_session_done_in_file(&mut file, "s1", 200).unwrap();
        assert_eq!(cwd, "/p");
        assert_eq!(file.tasks[0].column, TaskColumn::Review);
        assert_eq!(file.runs[0].status, TaskRunStatus::Completed);
        assert_eq!(file.runs[0].finished_at, Some(200));
    }

    #[test]
    fn recurring_task_completion_returns_to_backlog_at_next_occurrence() {
        let mut t = task("/p", "hourly");
        t.kind = TaskKind::Scheduled;
        t.schedule_frequency = TaskScheduleFrequency::Hourly;
        t.run_at = Some(100);
        let mut file = TaskFile {
            tasks: vec![t.clone()],
            runs: Vec::new(),
        };
        let run = begin_run_in_file(&mut file, &t.id, "claude", TaskChannel::Pty, 100).unwrap();
        file.tasks[0].session_id = Some("s1".into());
        file.runs[0].session_id = Some("s1".into());
        file.runs[0].status = TaskRunStatus::Running;

        assert_eq!(
            mark_session_done_in_file(&mut file, "s1", 3_700),
            Some("/p".into())
        );
        assert_eq!(file.runs[0].id, run.id);
        assert_eq!(file.runs[0].status, TaskRunStatus::Completed);
        assert_eq!(file.tasks[0].column, TaskColumn::Backlog);
        assert!(file.tasks[0].session_id.is_none());
        assert_eq!(file.tasks[0].run_at, Some(7_300));
    }

    #[test]
    fn recurring_task_failure_advances_after_retries_are_exhausted() {
        let mut t = task("/p", "hourly");
        t.kind = TaskKind::Scheduled;
        t.schedule_frequency = TaskScheduleFrequency::Hourly;
        t.run_at = Some(100);
        t.column = TaskColumn::Running;
        t.current_run_id = Some("r1".into());
        let mut run = TaskRun {
            id: "r1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("s1".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 100,
            started_at: Some(100),
            finished_at: None,
        };

        apply_failure_to_task(&mut t, &mut run, "boom", 3_700);
        assert_eq!(run.status, TaskRunStatus::Failed);
        assert_eq!(t.column, TaskColumn::Backlog);
        assert_eq!(t.run_at, Some(7_300));
        assert!(t.retry_at.is_none());
    }

    #[test]
    fn recurring_schedule_advances_without_catching_up_missed_runs() {
        assert_eq!(
            next_scheduled_run_at(TaskScheduleFrequency::Hourly, Some(100), 3_700),
            Some(7_300)
        );
        assert_eq!(
            next_scheduled_run_at(TaskScheduleFrequency::Hourly, Some(7_300), 1_000),
            Some(7_300)
        );

        let now = now_secs();
        let daily_anchor = now - 86_400;
        let next = next_scheduled_run_at(TaskScheduleFrequency::Daily, Some(daily_anchor), now)
            .expect("daily schedules have a next occurrence");
        assert!(next > now);
        use chrono::{Local, TimeZone};
        let anchor = Local.timestamp_opt(daily_anchor as i64, 0).single().unwrap();
        let next = Local.timestamp_opt(next as i64, 0).single().unwrap();
        assert_eq!(next.time(), anchor.time());
    }

    #[test]
    fn due_ids_respect_retry_cooldown_and_deps() {
        let now = 1_000u64;
        // 指定时间到期 + 依赖满足 → 出现
        let mut dep = task("/p", "dep");
        dep.column = TaskColumn::Done;
        let mut s = task("/p", "s");
        s.kind = TaskKind::Scheduled;
        s.run_at = Some(900);
        s.depends_on = vec![dep.id.clone()];
        let mut file = TaskFile { tasks: vec![dep, s], runs: Vec::new() };
        let due = due_scheduled_ids_in_file(&file, now);
        assert_eq!(due.len(), 1);

        // 重试冷却到期
        let mut r = task("/p", "r");
        r.retry_at = Some(now);
        file.tasks.push(r);
        let retry = due_retry_ids_in_file(&file, now);
        assert_eq!(retry.len(), 1);
    }

    #[test]
    fn launch_with_prompt_uses_cat_not_dash_p() {
        let p = Path::new("/tmp/prompt.txt");
        let cmd = build_launch_with_prompt("claude --dangerously-skip-permissions", p);
        assert!(cmd.contains("\"$(cat "));
        assert!(!cmd.contains(" -p "));
    }

    #[test]
    fn prompt_falls_back_to_title_when_body_empty() {
        let mut t = task("/p", "only title");
        t.body = String::new();
        assert_eq!(task_prompt(&t), "only title");
    }
}
