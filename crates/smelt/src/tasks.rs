//! 本地任务：侧栏统一查看与开跑，**全部走交互终端**（不 `-p` 无头批跑）。
//!
//! - 总览只做会话监控；任务列表在左侧「任务」分组
//! - 开跑 = 新开侧栏终端 + `launch "首包"`（CLI 启动参数，**不**模拟粘贴/回车）
//! - 已有会话续聊才用 paste + 裸 `\r`（见 `send_text_and_submit`）
//! - 见 docs/local-tasks.md

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::*;
use serde::{Deserialize, Serialize};

use crate::settings::{active_launch_entries, icon_for_launch_command};
use crate::terminal_view::TerminalView;
use crate::{Workspace, new_sid};

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

    pub fn color(self) -> u32 {
        match self {
            Self::Running | Self::Waiting => crate::ui_theme::blue(),
            Self::Backlog | Self::Ready => crate::ui_theme::text_muted(),
            Self::Review => crate::ui_theme::yellow(),
            Self::Failed => crate::ui_theme::red(),
            Self::Done => crate::ui_theme::green(),
        }
    }

    /// 侧栏 / 总览排序（越小越靠前）。
    pub fn sidebar_rank(self) -> u8 {
        match self {
            Self::Running | Self::Waiting => 0,
            Self::Review | Self::Failed => 1,
            Self::Backlog | Self::Ready => 2,
            Self::Done => 3,
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

    /// 状态下拉可选的三态（写入 store 用规范化值）。
    pub fn ui_choices() -> [TaskColumn; 4] {
        [Self::Backlog, Self::Running, Self::Review, Self::Done]
    }
}

/// 执行通道。`Pty` = 交互终端（startup-arg 首包）；`Acp` = ACP 结构化对话
/// （建独立 ACP 会话 + 首包 prompt）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskChannel {
    /// 交互终端（默认）。旧数据只有这一种形态。
    Pty,
    /// ACP 结构化对话。payload 记录接哪家 agent 与可选 workspace profile。
    Acp {
        /// `AcpAgentKind::id()`：claude/copilot/codex/grok。空 = 走默认 claude。
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
    /// 兼容旧数据（`"pty"`）与手改的裸 `"acp"` 字符串：后者映射成 `Acp{默认}`，
    /// 否则 load_json 会把整份文件当损坏回退清空。
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
    fn is_active(self) -> bool {
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
    /// 第一阶段只接 PTY；枚举先保留 ACP 形态。
    pub channel: TaskChannel,
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

/// 任务类型：普通（手动运行）/ 单次定时（到点自动 `run_task`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 手动「运行」才开跑。
    #[default]
    Once,
    /// 到 `run_at` 后由 Workspace 扫描器自动开跑（单次，不循环）。
    Scheduled,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "普通",
            Self::Scheduled => "定时",
        }
    }
}

/// 失败自动重试策略（任务级）。`max_attempts=1` = 不重试；`0` = 无限。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRetryPolicy {
    /// 最多尝试次数（含首次）。1 = 不重试；0 = 无限。
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// 失败 → 下次重试间隔秒数。0 = 立即回待办队列。
    #[serde(default)]
    pub retry_delay_secs: u64,
    /// 重试时是否换 provider（Remix）。当前只记录，不改 launch。
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

/// 本地任务。
///
/// 字段分工（给 UI / agent / 自循环时别混）：
/// - `title`：**给人看**的侧栏名；可空，创建时用首包首行生成
/// - `body`：**给 agent 的首包**（唯一写入 launch 启动参数的内容）
/// - `project_cwd`：在哪个项目目录开终端
/// - `launch`：base 启动命令（不含首包拼接）
/// - `session_id`：执行体（smeltd 会话）
/// - `kind` / `run_at`：普通 vs 单次定时
/// - `auto_run`：是否允许系统自动开跑（完成续跑 / 定时扫描）；手动点「运行」始终可以
/// - `depends_on`：前置任务 id，全部 Done 才允许执行
/// - `retry_policy` / `retry_at`：失败自动重试策略与当前冷却到点时刻
/// - `channel`：执行通道（PTY / ACP）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// 侧栏展示名（人类可读）。
    pub title: String,
    /// Agent 首包 prompt（开跑时进 CLI 参数；不是标题的复述）。
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub column: TaskColumn,
    /// 绑定的项目目录（绝对路径）。
    pub project_cwd: String,
    /// 已开终端的 smeltd session id。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 当前/最近一次执行。旧 tasks.json 缺少该字段时自动为空。
    #[serde(default)]
    pub current_run_id: Option<String>,
    /// 快捷启动 base 命令（如 `claude --dangerously-skip-permissions`）。
    #[serde(default)]
    pub launch: Option<String>,
    /// 普通 / 单次定时。缺省 = 普通（兼容旧 tasks.json）。
    #[serde(default)]
    pub kind: TaskKind,
    /// 计划开跑时间（Unix 秒，本地语义写入）。仅 `kind = Scheduled` 有意义。
    #[serde(default)]
    pub run_at: Option<u64>,
    /// 是否可被系统自动执行（完成边沿续跑、定时扫描）。
    /// `false` = 只等人点「运行」。缺省 true（兼容旧数据与排队续跑预期）。
    #[serde(default = "default_true")]
    pub auto_run: bool,
    /// 前置任务 id，全部 Done 才允许被 claim/手动运行。旧数据缺省空。
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 失败自动重试策略。旧数据缺省 = 不重试。
    #[serde(default)]
    pub retry_policy: TaskRetryPolicy,
    /// 重试冷却到点时刻（Unix 秒）。None = 立即可 claim。开跑/领用即清。
    #[serde(default)]
    pub retry_at: Option<u64>,
    /// 执行通道（PTY / ACP）。旧数据缺省 = PTY。
    #[serde(default)]
    pub channel: TaskChannel,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn default_true() -> bool {
    true
}

impl Task {
    pub fn new(project_cwd: String, title: String, body: String, launch: Option<String>) -> Self {
        let now = now_secs();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            body,
            column: TaskColumn::Backlog,
            project_cwd,
            session_id: None,
            current_run_id: None,
            launch,
            kind: TaskKind::Once,
            run_at: None,
            auto_run: true,
            depends_on: Vec::new(),
            retry_policy: TaskRetryPolicy::default(),
            retry_at: None,
            channel: TaskChannel::Pty,
            created_at: now,
            updated_at: now,
        }
    }

    /// 定时任务是否已到点（`run_at <= now`）且允许自动执行。
    pub fn is_due(&self, now: u64) -> bool {
        self.auto_run
            && self.kind == TaskKind::Scheduled
            && self.column.is_todo()
            && self.run_at.map(|at| at <= now).unwrap_or(false)
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 把一次失败落到 Run + Task：
/// - Run 记 Failed（保留 error / finished_at）
/// - 若该 Run 仍是当前 run：任务回待办（未超限，冷却内不可 claim）或落 Failed 列（超限）
/// - 保留 `current_run_id` 指向失败 Run，供历史面板展示；重试的 `begin_run` 会覆盖它
fn apply_failure_to_task(task: &mut Task, run: &mut TaskRun, error: &str, now: u64) {
    run.status = TaskRunStatus::Failed;
    run.error = Some(error.to_string());
    run.finished_at = Some(now);
    if task.current_run_id.as_deref() == Some(run.id.as_str()) {
        task.session_id = None;
        task.updated_at = now;
        if task.retry_policy.allows_retry(run.attempt) {
            // 未超限：回待办，冷却后由 due_retry_ids / claim 重新取跑。
            task.column = TaskColumn::Backlog;
            task.retry_at = if task.retry_policy.retry_delay_secs > 0 {
                Some(now + task.retry_policy.retry_delay_secs)
            } else {
                None
            };
        } else {
            // 超限：落 Failed 列，不再自动跑。
            task.column = TaskColumn::Failed;
            task.retry_at = None;
        }
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

/// 展示用短时间（本地）：`7/15 18:30`。
pub fn format_run_at_short(secs: u64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .map(|dt| dt.format("%m/%d %H:%M").to_string())
        .unwrap_or_else(|| secs.to_string())
}

/// 输入框默认值：约一小时后（本地 `YYYY-MM-DD HH:MM`）。
pub fn default_run_at_input() -> String {
    use chrono::{Duration, Local};
    (Local::now() + Duration::hours(1))
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// 编辑弹窗回填用：Unix 秒 → 输入框格式（本地 `YYYY-MM-DD HH:MM`，可被 [`parse_local_datetime`] 读回）。
pub fn format_run_at_input(secs: u64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(default_run_at_input)
}

fn tasks_global_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("tasks.json"))
}

/// 开跑时交给 agent 的首包：**只用 body**；旧数据 body 空时才回退 title。
/// 不再把 title 拼进 prompt（标题是侧栏标签，不是指令）。
pub fn task_prompt(task: &Task) -> String {
    let body = task.body.trim();
    if !body.is_empty() {
        body.to_string()
    } else {
        task.title.trim().to_string()
    }
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

fn tasks_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("tasks"))
}

/// 把首包 prompt 落到磁盘，供 `$(cat …)` 塞进 launch（多行/引号安全）。
fn write_prompt_file(task_id: &str, prompt: &str) -> Option<PathBuf> {
    let dir = tasks_dir()?.join("prompts");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{task_id}.txt"));
    std::fs::write(&path, prompt).ok()?;
    Some(path)
}

/// 交互启动：在 base launch 后追加 `"$(cat prompt)"` 作为 **CLI 首包参数**。
///
/// 对齐 vibeyard `pendingPromptTrigger: 'startup-arg'`（`claude "…"`），
/// **不是** `claude -p` 无头批跑。agent 起来即带第一条用户消息，无需 PTY 回车。
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

fn project_label(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

/// 终端右键「新建任务」时写入；`open_new_task_modal` 消费后清空。
#[derive(Default, Clone)]
pub struct NewTaskPrefill {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

impl Global for NewTaskPrefill {}

// ===================== TaskStore（全局）=====================
//
// TaskStore 持有**进程级内存缓存**（render 读它不碰 socket），写操作同步发给
// smeltd（权威落盘 `~/.smelt/tasks.json`），smeltd 不可用时降级直接写本地文件。
// 这样 GUI 与 `smelt-task` CLI 看到同一份任务：agent 自循环塞的任务，GUI 通过
// 30s tick 的 `task_list` 刷新可见。

/// 进程级任务缓存。`RwLock` 让 render 读（读锁，微秒级）与写（短暂写锁）共存。
static TASK_CACHE: OnceLock<RwLock<TaskFile>> = OnceLock::new();

fn task_cache() -> &'static RwLock<TaskFile> {
    TASK_CACHE.get_or_init(|| RwLock::new(crate::json_store::load_json(tasks_global_path())))
}

/// 序列化执行通道成 op 字段（对齐 smeltd `parse_channel`）。
fn channel_json(channel: &TaskChannel) -> serde_json::Value {
    match channel {
        TaskChannel::Pty => serde_json::json!({ "channel": "pty" }),
        TaskChannel::Acp { agent, profile_id } => serde_json::json!({
            "channel": "acp",
            "agent": agent,
            "profile_id": profile_id,
        }),
    }
}

pub struct TaskStore;

impl TaskStore {
    pub fn load() -> TaskFile {
        task_cache().read().unwrap().clone()
    }

    /// 降级路径（smeltd 不可用）直接落盘本地文件。
    pub fn save(file: &TaskFile) {
        crate::json_store::save_json(tasks_global_path(), file);
    }

    /// 在缓存上执行闭包，替换回缓存，返回闭包结果 + 最新文件快照。
    fn mutate<X>(f: impl FnOnce(&mut TaskFile) -> X) -> (X, TaskFile) {
        let mut file = task_cache().read().unwrap().clone();
        let out = f(&mut file);
        *task_cache().write().unwrap() = file.clone();
        (out, file)
    }

    /// 改完缓存后同步 smeltd（权威落盘）；失败降级本地落盘，保证至少一份持久化。
    fn persist(file: &TaskFile, op: serde_json::Value) {
        if smelt_core::task::request_task_op(op).is_err() {
            Self::save(file);
        }
    }

    /// 从 smeltd 拉全量任务刷新缓存（30s tick 调，让 `smelt-task` 塞的任务可见）。
    /// 拉不到（smeltd 不可用）就保持缓存不动。
    pub fn refresh_from_smeltd() {
        let Ok(resp) = smelt_core::task::request_task_op(serde_json::json!({ "op": "task_list" }))
        else {
            return;
        };
        if let Ok(file) = serde_json::from_value::<TaskFile>(resp["file"].clone()) {
            *task_cache().write().unwrap() = file;
        }
    }

    pub fn upsert(task: Task) {
        let (_, file) = Self::mutate(|f| {
            if let Some(slot) = f.tasks.iter_mut().find(|t| t.id == task.id) {
                *slot = task.clone();
            } else {
                f.tasks.insert(0, task.clone());
            }
        });
        Self::persist(&file, serde_json::json!({ "op": "task_add", "task": task }));
    }

    pub fn remove(id: &str) {
        let (_, file) = Self::mutate(|f| {
            f.tasks.retain(|t| t.id != id);
            f.runs.retain(|run| run.task_id != id);
        });
        Self::persist(&file, serde_json::json!({ "op": "task_remove", "id": id }));
        // 任务删了，落盘的首包 prompt 文件也得一起删，不然 `tasks/prompts/` 里
        // 会一直攒遗留文件（历史上就是这么攒出来的）。
        if let Some(dir) = tasks_dir() {
            let _ = std::fs::remove_file(dir.join("prompts").join(format!("{id}.txt")));
        }
    }

    pub fn get(id: &str) -> Option<Task> {
        Self::load().tasks.into_iter().find(|t| t.id == id)
    }

    pub fn update<F: FnOnce(&mut Task)>(id: &str, f: F) -> Option<Task> {
        let (out, file) = Self::mutate(|file| {
            let task = file.tasks.iter_mut().find(|t| t.id == id)?;
            f(task);
            task.updated_at = now_secs();
            Some(task.clone())
        });
        let Some(task) = out else {
            return None;
        };
        Self::persist(&file, serde_json::json!({ "op": "task_update", "task": task.clone() }));
        Some(task)
    }

    /// 为任务创建一次 PTY 执行尝试。若上一次仍显示活跃，先以“被新执行替代”收尾，
    /// 避免一个 Task 悬挂多个权威 Run。
    pub fn begin_pty_run(task_id: &str, launch: &str) -> Option<TaskRun> {
        Self::begin_run(task_id, launch, TaskChannel::Pty)
    }

    /// 为任务创建一次 ACP 执行尝试（记录接哪家 agent 与可选 workspace profile）。
    pub fn begin_acp_run(
        task_id: &str,
        launch: &str,
        agent: &str,
        profile_id: Option<String>,
    ) -> Option<TaskRun> {
        Self::begin_run(
            task_id,
            launch,
            TaskChannel::Acp { agent: agent.to_string(), profile_id },
        )
    }

    /// 为任务创建一次执行尝试（channel 由调用方指定）。开跑即清重试冷却。
    /// 若上一次仍显示活跃，先以“被新执行替代”收尾，避免一个 Task 悬挂多个权威 Run。
    fn begin_run(task_id: &str, launch: &str, channel: TaskChannel) -> Option<TaskRun> {
        let now = now_secs();
        // 优先走 smeltd：它原子创建 run 并落盘（权威）。
        let mut op = serde_json::json!({
            "op": "task_begin_run",
            "task_id": task_id,
            "launch": launch,
        });
        op["channel"] = channel_json(&channel);
        if let Ok(resp) = smelt_core::task::request_task_op(op) {
            let run: TaskRun = serde_json::from_value(resp["run"].clone()).ok()?;
            Self::mutate(|f| {
                if let Some(task) = f.tasks.iter_mut().find(|task| task.id == task_id) {
                    task.column = TaskColumn::Running;
                    task.current_run_id = Some(run.id.clone());
                    task.retry_at = None;
                    task.updated_at = now;
                }
                f.runs.push(run.clone());
            });
            return Some(run);
        }
        // 降级：本地造 run + 落盘。
        let (run, file) = Self::mutate(|f| {
            let attempt = f
                .runs
                .iter()
                .filter(|run| run.task_id == task_id)
                .map(|run| run.attempt)
                .max()
                .unwrap_or(0)
                + 1;
            for run in f
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
                channel: channel.clone(),
                launch: launch.to_string(),
                session_id: None,
                status: TaskRunStatus::Starting,
                error: None,
                created_at: now,
                started_at: None,
                finished_at: None,
            };
            let task = f.tasks.iter_mut().find(|task| task.id == task_id)?;
            task.column = TaskColumn::Running;
            task.current_run_id = Some(run.id.clone());
            task.retry_at = None;
            task.updated_at = now;
            f.runs.push(run.clone());
            Some(run)
        });
        if run.is_some() {
            Self::save(&file);
        }
        run
    }

    /// PTY 成功创建后，把 Run 与稳定 session id 绑定。
    pub fn mark_run_started(task_id: &str, run_id: &str, session_id: &str) -> bool {
        let (ok, file) = Self::mutate(|file| {
            let now = now_secs();
            let Some(run) = file
                .runs
                .iter_mut()
                .find(|run| run.id == run_id && run.task_id == task_id)
            else {
                return false;
            };
            run.status = TaskRunStatus::Running;
            run.session_id = Some(session_id.to_string());
            run.started_at = Some(now);
            let Some(task) = file.tasks.iter_mut().find(|task| task.id == task_id) else {
                return false;
            };
            task.session_id = Some(session_id.to_string());
            task.current_run_id = Some(run_id.to_string());
            task.column = TaskColumn::Running;
            task.updated_at = now;
            true
        });
        if ok {
            Self::persist(
                &file,
                serde_json::json!({
                    "op": "task_attach_session",
                    "task_id": task_id,
                    "run_id": run_id,
                    "session_id": session_id,
                }),
            );
        }
        ok
    }

    /// 执行现场启动失败：保留失败 Run，按重试策略回待办（冷却）或落 Failed 列。
    /// 返回受影响任务的项目 cwd（Some）用于触发续跑/重试。
    pub fn mark_run_failed(task_id: &str, run_id: &str, error: impl Into<String>) -> Option<String> {
        let err = error.into();
        let (cwd, file) = Self::mutate(|file| {
            let now = now_secs();
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
            apply_failure_to_task(task, run, &err, now);
            cwd
        });
        if cwd.is_some() {
            Self::persist(
                &file,
                serde_json::json!({
                    "op": "task_run_failed",
                    "task_id": task_id,
                    "run_id": run_id,
                    "error": err,
                }),
            );
        }
        cwd
    }

    /// agent 回合失败（结构化 phase=Failed 边沿）：把该会话绑定的活跃任务按重试策略处理。
    /// 语义对齐 `mark_session_done`，返回 `Some(cwd)` 表示确实收尾了至少一条任务。
    pub fn mark_session_failed(session_id: &str, error: &str) -> Option<String> {
        let (cwd, file) = Self::mutate(|file| {
            let mut cwd: Option<String> = None;
            let now = now_secs();
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
        });
        if cwd.is_some() {
            Self::persist(
                &file,
                serde_json::json!({ "op": "task_session_failed", "session_id": session_id, "error": error }),
            );
        }
        cwd
    }

    pub fn runs_for_task(task_id: &str) -> Vec<TaskRun> {
        let mut runs: Vec<_> = Self::load()
            .runs
            .into_iter()
            .filter(|run| run.task_id == task_id)
            .collect();
        runs.sort_by_key(|run| std::cmp::Reverse(run.attempt));
        runs
    }

    /// 终端 agent 停转（完成一轮）时：把当前 Run 标 Completed，Task 进入待审查。
    /// 返回 `Some(project_cwd)` 表示确实收尾了至少一条任务（用于触发自动续跑）。
    pub fn mark_session_done(session_id: &str) -> Option<String> {
        let (done_cwd, file) = Self::mutate(|file| {
            let mut done_cwd: Option<String> = None;
            let now = now_secs();
            for t in &mut file.tasks {
                if t.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                if matches!(t.column, TaskColumn::Running | TaskColumn::Waiting) {
                    t.column = TaskColumn::Review;
                    t.updated_at = now;
                    if let Some(run_id) = t.current_run_id.as_deref()
                        && let Some(run) = file.runs.iter_mut().find(|run| {
                            run.id == run_id
                                && run.session_id.as_deref() == Some(session_id)
                                && run.status.is_active()
                        })
                    {
                        run.status = TaskRunStatus::Completed;
                        run.finished_at = Some(now);
                    }
                    if done_cwd.is_none() {
                        done_cwd = Some(t.project_cwd.clone());
                    }
                }
            }
            done_cwd
        });
        if done_cwd.is_some() {
            Self::persist(
                &file,
                serde_json::json!({ "op": "task_session_done", "session_id": session_id }),
            );
        }
        done_cwd
    }

    /// 该项目是否已有执行中任务（同 cwd 串行 worker）。
    pub fn has_running_for_cwd(cwd: &str) -> bool {
        let cwd = cwd.trim_end_matches('/');
        Self::load()
            .tasks
            .iter()
            .any(|t| t.column.is_active() && t.project_cwd.trim_end_matches('/') == cwd)
    }

    /// 任务此刻是否可被**系统**自动取跑（待办 + `auto_run`；依赖满足 + 重试冷却过；
    /// 定时须已到期）。人手点「运行」不走此判断。
    fn is_auto_runnable(t: &Task, tasks: &[Task], now: u64) -> bool {
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

    /// 选择下一条**可自动执行**的待办。真正的 Running 状态由 begin_pty_run
    /// 与执行记录一起写入，避免领取后启动失败留下幽灵 Running。
    ///
    /// - 只取 `prefer_cwd` 同项目（串行续跑）
    /// - 仅 `auto_run == true`、依赖满足、重试冷却已过 且可跑
    /// - 该 cwd 已有 Running/Waiting 则不领
    /// - FIFO：`created_at` 升序
    pub fn claim_next_runnable(prefer_cwd: &str) -> Option<String> {
        let prefer = prefer_cwd.trim_end_matches('/');
        if prefer.is_empty() {
            return None;
        }
        let file = Self::load();
        let now = now_secs();
        if file
            .tasks
            .iter()
            .any(|t| t.column.is_active() && t.project_cwd.trim_end_matches('/') == prefer)
        {
            return None;
        }
        let mut idxs: Vec<usize> = file
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.project_cwd.trim_end_matches('/') == prefer
                    && Self::is_auto_runnable(t, &file.tasks, now)
            })
            .map(|(i, _)| i)
            .collect();
        idxs.sort_by_key(|&i| file.tasks[i].created_at);
        idxs.first().map(|&idx| file.tasks[idx].id.clone())
    }

    /// 已到期、可自动执行的单次定时任务 id（按 `run_at` 升序）。
    /// 依赖未满足的定时任务不会被领取。
    pub fn due_scheduled_ids() -> Vec<String> {
        let now = now_secs();
        let file = Self::load();
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
    pub fn due_retry_ids() -> Vec<String> {
        let now = now_secs();
        let file = Self::load();
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
}

// ===================== Workspace =====================

impl Workspace {
    /// 侧栏会话里出现过的项目 cwd 列表（去重，保序）。
    pub fn known_project_cwds(&self, cx: &App) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.sessions {
            if let Some(c) = s.cwd(cx) {
                if !out.iter().any(|x| x == &c) {
                    out.push(c);
                }
            }
        }
        if out.is_empty() {
            if let Ok(p) = std::env::current_dir() {
                out.push(p.display().to_string());
            }
        }
        out
    }

    /// 新建任务时绑定的项目；无则取第一个 known。
    pub fn task_bind_cwd(&self, cx: &App) -> Option<String> {
        if let Some(c) = &self.task_bind_project {
            if !c.is_empty() {
                return Some(c.clone());
            }
        }
        self.known_project_cwds(cx).into_iter().next()
    }

    /// 新建任务选用的 launch；无则取启动项第一项。
    pub fn task_bind_launch_cmd(&self, cx: &App) -> Option<String> {
        if let Some(c) = &self.task_bind_launch {
            if !c.trim().is_empty() {
                return Some(c.clone());
            }
        }
        active_launch_entries(cx).first().map(|e| e.command.clone())
    }

    pub fn set_task_bind_project(&mut self, cwd: String, cx: &mut Context<Self>) {
        self.task_bind_project = Some(cwd);
        cx.notify();
    }

    pub fn set_task_bind_launch(&mut self, command: String, cx: &mut Context<Self>) {
        self.task_bind_launch = Some(command);
        // 手动选 Agent 时改回「新开终端」
        self.task_bind_session = None;
        cx.notify();
    }

    /// 从指定终端打开新建任务：项目/session 预填，开跑时注入该终端（保留上下文）。
    pub fn open_new_task_for_terminal(
        &mut self,
        pane: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = pane.read(cx).session_id().to_string();
        let cwd = pane.read(cx).cwd();
        self.task_bind_session = Some(sid);
        if let Some(c) = cwd {
            self.task_bind_project = Some(c);
        }
        // 已有终端路径不强制 Agent 启动项
        self.open_new_task_modal(window, cx);
    }

    pub fn ensure_task_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.task_title_input.is_some() {
            return;
        }
        // body = 首包（主字段）；title = 可选侧栏名
        let body = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(4, 12)
                .placeholder("写给 agent 的第一条指令…")
        });
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("留空则用指令首行"));
        let run_at =
            cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD HH:MM（本地时间）"));
        self._task_title_sub = None;
        self.task_body_input = Some(body);
        self.task_title_input = Some(title);
        self.task_run_at_input = Some(run_at);
    }

    pub fn set_task_kind(&mut self, kind: TaskKind, window: &mut Window, cx: &mut Context<Self>) {
        self.task_kind = kind;
        if kind == TaskKind::Scheduled {
            if let Some(input) = &self.task_run_at_input {
                let cur = input.read(cx).value().to_string();
                if cur.trim().is_empty() {
                    let def = default_run_at_input();
                    input.update(cx, |s, cx| s.set_value(def, window, cx));
                }
            }
        }
        cx.notify();
    }

    pub fn open_new_task_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_task_inputs(window, cx);
        // 明确进入「新建」模式（可能从上一次编辑残留）
        self.task_editing = None;
        // 终端右键预填（若有）
        if let Some(pre) = cx.try_global::<NewTaskPrefill>() {
            let pre = pre.clone();
            if pre.session_id.is_some() || pre.cwd.is_some() {
                self.task_bind_session = pre.session_id;
                if let Some(c) = pre.cwd {
                    self.task_bind_project = Some(c);
                }
            }
            *cx.default_global::<NewTaskPrefill>() = NewTaskPrefill::default();
        }
        // 未预填 session 时：默认当前会话项目、新开终端。
        if self.task_bind_session.is_none() {
            if let Some(c) = self.cur().and_then(|s| s.cwd(cx)) {
                self.task_bind_project = Some(c);
            } else if self.task_bind_project.is_none() {
                self.task_bind_project = self.known_project_cwds(cx).into_iter().next();
            }
        }
        if self.task_bind_launch.is_none() {
            self.task_bind_launch = active_launch_entries(cx).first().map(|e| e.command.clone());
        }
        // 每次打开：默认普通 + 可自动执行，清空文案；焦点落在首包。
        self.task_kind = TaskKind::Once;
        self.task_auto_run = true;
        self.task_show_advanced = false;
        if let Some(input) = &self.task_body_input {
            input.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.focus(window, cx);
            });
        }
        if let Some(input) = &self.task_title_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if let Some(input) = &self.task_run_at_input {
            input.update(cx, |s, cx| s.set_value(default_run_at_input(), window, cx));
        }
        self.show_new_task_modal = true;
        cx.notify();
    }

    pub fn close_new_task_modal(&mut self, cx: &mut Context<Self>) {
        self.show_new_task_modal = false;
        self.task_bind_session = None;
        self.task_editing = None;
        self.task_kind = TaskKind::Once;
        self.task_auto_run = true;
        cx.notify();
    }

    /// 打开「编辑任务」弹窗：复用新建弹窗，预填已有任务的全部字段。
    /// 提交走 [`Self::save_task_from_inputs`]（upsert），不新建、不触发运行。
    pub fn open_edit_task_modal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_task_inputs(window, cx);
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        self.task_editing = Some(id.to_string());
        // 编辑不涉及「注入哪个终端」——清掉会话绑定，避免保存被当成开跑上下文。
        self.task_bind_session = None;
        self.task_bind_project = Some(task.project_cwd.clone());
        self.task_bind_launch = task.launch.clone();
        self.task_kind = task.kind;
        self.task_auto_run = task.auto_run;
        self.task_channel_acp = matches!(&task.channel, TaskChannel::Acp { .. });
        if let TaskChannel::Acp { agent, .. } = &task.channel {
            if !agent.is_empty() {
                self.task_acp_agent = agent.clone();
            }
        }

        if let Some(input) = &self.task_body_input {
            let body = task.body.clone();
            input.update(cx, |s, cx| {
                s.set_value(body, window, cx);
                s.focus(window, cx);
            });
        }
        if let Some(input) = &self.task_title_input {
            let title = task.title.clone();
            input.update(cx, |s, cx| s.set_value(title, window, cx));
        }
        if let Some(input) = &self.task_run_at_input {
            let val = task
                .run_at
                .map(format_run_at_input)
                .unwrap_or_else(default_run_at_input);
            input.update(cx, |s, cx| s.set_value(val, window, cx));
        }
        self.show_new_task_modal = true;
        cx.notify();
    }

    /// 编辑弹窗「保存」：把输入写回 [`TaskStore`]（不新建、不开跑）。
    /// 校验同新建：首包必填；定时须合法时间。校验不过则保持弹窗。
    pub fn save_task_from_inputs(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.task_editing.clone() else {
            return;
        };
        let Some(cwd) = self.task_bind_cwd(cx) else {
            return;
        };
        let body = self
            .task_body_input
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default();
        if body.trim().is_empty() {
            return;
        }
        let title_in = self
            .task_title_input
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default()
            .trim()
            .to_string();
        let kind = self.task_kind;
        // 定时任务恒为自动执行；普通任务跟弹窗开关。
        let auto_run = kind == TaskKind::Scheduled || self.task_auto_run;
        let run_at = if kind == TaskKind::Scheduled {
            let raw = self
                .task_run_at_input
                .as_ref()
                .map(|s| s.read(cx).value().to_string())
                .unwrap_or_default();
            let Some(at) = parse_local_datetime(&raw) else {
                return;
            };
            Some(at)
        } else {
            None
        };
        let title = if title_in.is_empty() {
            title_from_prompt(&body)
        } else {
            title_in
        };
        let launch = self.task_bind_launch_cmd(cx);
        let channel = if self.task_channel_acp {
            TaskChannel::Acp { agent: self.task_acp_agent.clone(), profile_id: None }
        } else {
            TaskChannel::Pty
        };
        TaskStore::update(&id, |t| {
            t.title = title;
            t.body = body;
            t.kind = kind;
            t.run_at = run_at;
            t.auto_run = auto_run;
            t.project_cwd = cwd;
            t.launch = launch;
            t.channel = channel;
        });
        self.close_new_task_modal(cx);
    }

    /// 从弹窗创建任务。`run` 时：有 `task_bind_session` → 注入该终端；否则新开终端。
    /// 定时且 `run_at` 仍在未来：只入库，等扫描器到点再 `run_task`。
    pub fn create_task_from_inputs(
        &mut self,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.task_bind_cwd(cx) else {
            return;
        };
        let body = self
            .task_body_input
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default();
        let title_in = self
            .task_title_input
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default()
            .trim()
            .to_string();
        // 必填：首包。标题可选。
        if body.trim().is_empty() {
            return;
        }
        let kind = self.task_kind;
        // 定时本身就是到点自动跑 → 强制 auto_run；普通任务跟弹窗开关。
        let auto_run = kind == TaskKind::Scheduled || self.task_auto_run;
        let run_at = if kind == TaskKind::Scheduled {
            let raw = self
                .task_run_at_input
                .as_ref()
                .map(|s| s.read(cx).value().to_string())
                .unwrap_or_default();
            let Some(at) = parse_local_datetime(&raw) else {
                // 时间非法：不创建（保持弹窗，用户改完再提交）
                return;
            };
            Some(at)
        } else {
            None
        };
        let title = if title_in.is_empty() {
            title_from_prompt(&body)
        } else {
            title_in
        };
        let launch = self.task_bind_launch_cmd(cx);
        // 清掉绑定，避免下次侧栏新建仍绑旧终端
        let sid = self.task_bind_session.take();
        let mut task = Task::new(cwd, title, body, launch);
        task.kind = kind;
        task.run_at = run_at;
        task.auto_run = auto_run;
        if self.task_channel_acp {
            task.channel = TaskChannel::Acp {
                agent: self.task_acp_agent.clone(),
                profile_id: None,
            };
        }
        let id = task.id.clone();
        self.task_selected = Some(id.clone());
        TaskStore::upsert(task);
        if let Some(input) = &self.task_title_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if let Some(input) = &self.task_body_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.show_new_task_modal = false;
        self.task_kind = TaskKind::Once;
        self.task_auto_run = true;

        // 定时且未到点：只创建，扫描器稍后开跑。
        let schedule_only =
            kind == TaskKind::Scheduled && run_at.map(|at| at > now_secs()).unwrap_or(true);
        let should_run = run && !schedule_only;

        if let Some(sid) = sid {
            self.assign_task_to_session(&id, &sid, should_run, window, cx);
        } else if should_run {
            // 走 run_task：按通道路由（终端新开 / ACP 对话），并守依赖。
            self.run_task(&id, window, cx);
        } else {
            cx.notify();
        }
    }

    /// 后台扫描：到期定时任务 → 复用 [`Self::run_task`]。
    /// 同 cwd 已有执行中任务时跳过（串行，留给完成边沿续跑）。
    pub fn tick_scheduled_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先刷缓存：让 `smelt-task`（agent 自循环）塞进来的任务对 GUI 可见。
        TaskStore::refresh_from_smeltd();
        self.reconcile_tasks(cx);
        let mut ids = TaskStore::due_scheduled_ids();
        ids.extend(TaskStore::due_retry_ids());
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return;
        }
        for id in ids {
            let Some(t) = TaskStore::get(&id) else {
                continue;
            };
            if t.retry_at.is_some_and(|at| at <= now_secs()) {
                eprintln!("[tasks] retry due id={}", id);
            } else if !t.is_due(now_secs()) {
                continue;
            }
            if TaskStore::has_running_for_cwd(&t.project_cwd) {
                continue;
            }
            eprintln!("[tasks] scheduled due id={} run_at={:?}", id, t.run_at);
            self.run_task(&id, window, cx);
        }
    }

    /// agent 会话刚从 Running→Idle 且收尾了绑定任务：
    /// 同项目 claim 下一条 **auto_run** 待办并 `run_task`（全局始终尝试，闸门在任务字段）。
    pub fn on_session_task_idle(
        &mut self,
        session_id: &str,
        done_cwd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = if done_cwd.trim().is_empty() {
            self.cwd_for_session(session_id, cx).unwrap_or_default()
        } else {
            done_cwd.to_string()
        };
        if cwd.trim().is_empty() {
            return;
        }
        let Some(id) = TaskStore::claim_next_runnable(&cwd) else {
            return;
        };
        eprintln!("[tasks] auto_run after session={session_id} → next={id} cwd={cwd}");
        self.run_task(&id, window, cx);
    }

    /// 对账：`Running` 但绑定会话已不存在（GUI 崩溃后 smeltd 会话也丢了）的任务
    /// 标失败，避免「永远卡 Running、阻塞同 cwd 串行」的坏状态。会话既可能在
    /// 本 GUI（还没 reattach），也可能在 smeltd（活会话），两者都不在才算死。
    fn reconcile_tasks(&mut self, cx: &mut Context<Self>) {
        let mut known: HashSet<String> = HashSet::new();
        for sess in &self.sessions {
            match &sess.kind {
                crate::SessionKind::Term { .. } => {
                    for leaf in sess.term_leaves() {
                        known.insert(leaf.read(cx).session_id().to_string());
                    }
                }
                crate::SessionKind::Acp(view) => {
                    known.insert(view.read(cx).session_id().to_string());
                }
            }
        }
        if let Ok(alive) = smelt_core::session_control::list_sessions() {
            known.extend(alive);
        }
        let stale: Vec<(String, String)> = TaskStore::load()
            .tasks
            .iter()
            .filter(|t| {
                t.column.is_active()
                    && t.session_id
                        .as_ref()
                        .is_some_and(|sid| !known.contains(sid))
            })
            .filter_map(|t| {
                t.current_run_id
                    .clone()
                    .map(|rid| (t.id.clone(), rid))
            })
            .collect();
        for (tid, run_id) in stale {
            eprintln!("[tasks] reconcile: 任务 {tid} 绑定的会话已不存在，标记失败");
            TaskStore::mark_run_failed(&tid, &run_id, "绑定的会话已丢失");
        }
    }

    pub fn set_task_auto_run(&mut self, on: bool, cx: &mut Context<Self>) {
        self.task_auto_run = on;
        cx.notify();
    }

    /// 弹窗切执行通道：false=终端，true=ACP 对话。
    pub fn set_task_channel_acp(&mut self, on: bool, cx: &mut Context<Self>) {
        self.task_channel_acp = on;
        cx.notify();
    }

    /// 弹窗切 ACP 通道接哪家 agent（`AcpAgentKind::id()`）。
    pub fn set_task_acp_agent(&mut self, agent: &str, cx: &mut Context<Self>) {
        self.task_acp_agent = agent.to_string();
        cx.notify();
    }

    /// 按 smeltd session id 查终端 cwd。
    fn cwd_for_session(&self, session_id: &str, cx: &App) -> Option<String> {
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in leaves {
                if leaf.read(cx).session_id() == session_id {
                    return leaf.read(cx).cwd();
                }
            }
        }
        None
    }

    /// 新建任务弹窗。
    ///
    /// 默认新开终端；从终端右键进入时预绑该 session，开跑 = 键入+回车（沿用上下文）。
    pub fn render_new_task_modal(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted, border) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Workspace::modal_accent_colors(false);

        let Some(title_in) = self.task_title_input.as_ref() else {
            return div();
        };
        let Some(body_in) = self.task_body_input.as_ref() else {
            return div();
        };

        let projects = self.known_project_cwds(cx);
        let cur_proj = self.task_bind_cwd(cx).unwrap_or_default();
        let proj_btn_label = if cur_proj.is_empty() {
            "当前 / 默认".into()
        } else {
            project_label(&cur_proj)
        };

        let launches = active_launch_entries(cx);
        let cur_launch_cmd = self.task_bind_launch_cmd(cx).unwrap_or_default();
        let agent_btn_label = launches
            .iter()
            .find(|e| e.command == cur_launch_cmd)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| {
                if cur_launch_cmd.is_empty() {
                    "默认启动项".into()
                } else {
                    cur_launch_cmd.clone()
                }
            });
        let agent_icon = if cur_launch_cmd.is_empty() {
            IconName::Bot
        } else {
            icon_for_launch_command(&cur_launch_cmd)
        };

        let editing = self.task_editing.is_some();
        let on_existing = self.task_bind_session.is_some();
        let is_scheduled = self.task_kind == TaskKind::Scheduled;
        let auto_run = self.task_auto_run || is_scheduled;
        let channel_acp = self.task_channel_acp;
        let acp_agent_btn = smelt_core::agent_kind::AcpAgentKind::from_id(&self.task_acp_agent)
            .map(|a| a.label().to_string())
            .unwrap_or_else(|| "Claude Code".into());
        let exec_hint = if is_scheduled {
            "到点后自动新开终端开跑（单次）；也可提前点「运行」"
        } else if auto_run {
            "可自动执行：前一条做完 / 队列有空时系统会接着跑；也可手动「运行」"
        } else if on_existing {
            "仅手动：不会被完成续跑取走；运行 = 键入指令并回车进当前终端"
        } else {
            "仅手动：点「运行」才开终端；不会被系统自动取走"
        };
        let primary_label = if editing {
            "保存"
        } else if is_scheduled {
            "创建定时"
        } else {
            "创建并运行"
        };

        let field_label = |text: &str| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child(text.to_string())
        };

        let e = cx.entity().clone();
        let e2 = e.clone();
        let e3 = e.clone();
        // 主区：项目（高频，默认当前项目）。
        let project_row = v_flex()
            .gap_1()
            .child(field_label("项目 · 可选"))
            .child(
                Button::new("task-pick-project")
                    .label(proj_btn_label)
                    .icon(IconName::Folder)
                    .small()
                    .w_full()
                    .dropdown_menu({
                        let projects = projects.clone();
                        let e = e.clone();
                        move |menu, _window, _cx| {
                            let mut menu = menu;
                            if projects.is_empty() {
                                return menu.item(
                                    PopupMenuItem::new("暂无项目（先打开终端）")
                                        .disabled(true),
                                );
                            }
                            for p in &projects {
                                let cwd = p.clone();
                                let e = e.clone();
                                let label = project_label(p);
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, _, cx| {
                                        let cwd = cwd.clone();
                                        e.update(cx, |ws, cx| {
                                            ws.set_task_bind_project(cwd, cx);
                                        });
                                    },
                                ));
                            }
                            menu
                        }
                    }),
            );
        // 高级区：Agent（启动命令）。当前终端时忽略。
        let agent_row = v_flex()
            .gap_1()
            .opacity(if on_existing { 0.45 } else { 1. })
            .child(field_label(if on_existing {
                "Agent · 当前终端时忽略"
            } else {
                "Agent · 可选"
            }))
            .child(
                Button::new("task-pick-agent")
                    .label(agent_btn_label)
                    .icon(agent_icon)
                    .small()
                    .w_full()
                    .dropdown_menu({
                        let launches = launches.clone();
                        move |menu, _window, _cx| {
                            let mut menu = menu;
                            if launches.is_empty() {
                                return menu.item(
                                    PopupMenuItem::new("设置里暂无启动项").disabled(true),
                                );
                            }
                            for entry in &launches {
                                let label = entry.label.clone();
                                let command = entry.command.clone();
                                let e = e2.clone();
                                let icon = icon_for_launch_command(&command);
                                menu = menu.item(
                                    PopupMenuItem::new(label).icon(icon).on_click(
                                        move |_, _, cx| {
                                            let command = command.clone();
                                            e.update(cx, |ws, cx| {
                                                ws.set_task_bind_launch(command, cx);
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        }
                    }),
            );

        // 类型：普通 / 定时
        let kind_row = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("task-kind-once")
                    .label("普通")
                    .small()
                    .when(self.task_kind == TaskKind::Once, |b| b.primary())
                    .when(self.task_kind != TaskKind::Once, |b| b.ghost())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_task_kind(TaskKind::Once, window, cx);
                    })),
            )
            .child(
                Button::new("task-kind-scheduled")
                    .label("定时")
                    .small()
                    .when(is_scheduled, |b| b.primary())
                    .when(!is_scheduled, |b| b.ghost())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_task_kind(TaskKind::Scheduled, window, cx);
                    })),
            )
            .child(div().text_xs().text_color(muted).child(if is_scheduled {
                "单次 · 到点自动开跑"
            } else {
                "普通待办"
            }));

        // 任务级：是否允许系统自动执行（定时强制开）
        let auto_row = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("task-auto-run-on")
                    .label("可自动执行")
                    .small()
                    .when(auto_run, |b| b.primary())
                    .when(!auto_run, |b| b.ghost())
                    .disabled(is_scheduled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_task_auto_run(true, cx);
                    })),
            )
            .child(
                Button::new("task-auto-run-off")
                    .label("仅手动")
                    .small()
                    .when(!auto_run, |b| b.primary())
                    .when(auto_run, |b| b.ghost())
                    .disabled(is_scheduled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_task_auto_run(false, cx);
                    })),
            )
            .child(div().text_xs().text_color(muted).child(if is_scheduled {
                "定时任务默认自动"
            } else if auto_run {
                "完成续跑 / 队列会取它"
            } else {
                "只等人点运行"
            }));

        // 执行通道：终端（新开交互终端 + 首包指令）/ ACP 对话（结构化消息流）。
        let channel_row = h_flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("task-channel-terminal")
                    .label("终端")
                    .small()
                    .when(!channel_acp, |b| b.primary())
                    .when(channel_acp, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_task_channel_acp(false, cx);
                    })),
            )
            .child(
                Button::new("task-channel-acp")
                    .label("ACP 对话")
                    .small()
                    .when(channel_acp, |b| b.primary())
                    .when(!channel_acp, |b| b.ghost())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_task_channel_acp(true, cx);
                    })),
            )
            .child(div().text_xs().text_color(muted).child(if channel_acp {
                "对话通道：结构化消息流"
            } else {
                "终端通道：交互终端 + 首包指令"
            }));

        // ACP 通道时选 agent（默认 claude）。
        let e_agent = e3.clone();
        let acp_agent_row = v_flex()
            .gap_1()
            .child(field_label("Agent · ACP 对话接哪家"))
            .child(
                Button::new("task-acp-agent")
                    .label(acp_agent_btn)
                    .small()
                    .w_full()
                    .dropdown_menu(move |menu, _window, _cx| {
                        let mut menu = menu;
                        for agent in smelt_core::agent_kind::AcpAgentKind::ALL {
                            let label = agent.label().to_string();
                            let id = agent.id().to_string();
                            let e = e_agent.clone();
                            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                let id = id.clone();
                                e.update(cx, |ws, cx| ws.set_task_acp_agent(&id, cx));
                            }));
                        }
                        menu
                    }),
            );

        // 高级选项折叠区：通道/类型/自动执行/Agent/定时时间都收进来。
        // 新建默认折叠（只留「指令 + 项目」主字段）；编辑模式恒展开（改已有配置）。
        let advanced_open = self.task_show_advanced || editing;
        let advanced_toggle = div()
            .id("task-advanced-toggle")
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .text_xs()
            .text_color(muted)
            .hover(|d| d.text_color(fg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.task_show_advanced = !this.task_show_advanced;
                    cx.notify();
                }),
            )
            .child(
                Icon::new(if advanced_open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(14.)),
            )
            .child(div().child("高级选项"));
        let advanced_section = || {
            v_flex()
                .gap_3()
                .px_3()
                .py_3()
                .rounded_lg()
                .bg(rgb(crate::ui_theme::bg_rail()))
                .child(v_flex().gap_1().child(field_label("执行通道")).child(channel_row))
                .when(channel_acp, |d| d.child(acp_agent_row))
                .child(v_flex().gap_1().child(field_label("类型")).child(kind_row))
                .when(is_scheduled, |d| {
                    let run_at_in = self.task_run_at_input.as_ref();
                    d.child(
                        v_flex()
                            .gap_1()
                            .child(field_label("执行时间 · 本地（YYYY-MM-DD HH:MM）"))
                            .children(run_at_in.map(|i| Input::new(i))),
                    )
                })
                .child(
                    v_flex()
                        .gap_1()
                        .child(field_label("自动执行"))
                        .child(auto_row),
                )
                .child(agent_row)
                .when(editing, |d| {
                    d.child(
                        v_flex()
                            .gap_1()
                            .child(field_label("侧栏标题 · 可选"))
                            .child(Input::new(title_in)),
                    )
                })
                .child(div().text_xs().text_color(muted).child(exec_hint))
        };

        let content = v_flex()
            .gap_3()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child(if editing {
                        "编辑任务"
                    } else if on_existing {
                        "新建任务 · 当前终端"
                    } else {
                        "新建任务"
                    }),
            )
            .when(on_existing, |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(crate::ui_theme::tint(crate::ui_theme::blue(), 0x18))
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::blue()))
                        .child("已绑定侧栏选中的终端，运行会把指令发进该会话。"),
                )
            })
            // 主字段：指令（唯一必填，最显眼）
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("指令 · 给 agent 的首包（必填）"))
                    .child(Input::new(body_in)),
            )
            // 主字段：项目（默认当前项目）
            .child(project_row)
            // 高级选项：编辑恒展开（advanced_open 已含 editing）；新建点「高级选项」展开
            .when(!editing, |d| d.child(advanced_toggle))
            .when(advanced_open, |d| d.child(advanced_section()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_1()
                    .border_t_1()
                    .border_color(border)
                    .child(Workspace::modal_button(
                        "cancel-new-task",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.close_new_task_modal(cx),
                        cx,
                    ))
                    // 编辑模式无「仅创建」——保存即写回。
                    .when(!editing, |d| {
                        d.child(Workspace::modal_button(
                            "create-only-task",
                            "仅创建",
                            neutral_bg,
                            neutral_hover,
                            fg,
                            |this, _, window, cx| this.create_task_from_inputs(false, window, cx),
                            cx,
                        ))
                    })
                    .child(if editing {
                        Workspace::modal_button(
                            "confirm-new-task",
                            primary_label,
                            tint,
                            hover,
                            accent_text,
                            |this, _, window, cx| this.save_task_from_inputs(window, cx),
                            cx,
                        )
                    } else {
                        Workspace::modal_button(
                            "confirm-new-task",
                            primary_label,
                            tint,
                            hover,
                            accent_text,
                            |this, _, window, cx| this.create_task_from_inputs(true, window, cx),
                            cx,
                        )
                    }),
            );
        Workspace::modal_shell(500., false, content, cx)
    }

    pub fn delete_task(&mut self, id: &str, cx: &mut Context<Self>) {
        TaskStore::remove(id);
        if self.task_selected.as_deref() == Some(id) {
            self.task_selected = None;
        }
        cx.notify();
    }

    /// 直接设为指定列（任务卡片状态下拉用）。
    pub fn set_task_column(&mut self, id: &str, col: TaskColumn, cx: &mut Context<Self>) {
        TaskStore::update(id, |t| t.column = col);
        cx.notify();
    }

    /// 任务总览 pill：点同一状态再点一次回到「全部」。
    pub fn set_task_column_filter(&mut self, col: Option<TaskColumn>, cx: &mut Context<Self>) {
        self.task_column_filter = if self.task_column_filter == col {
            None
        } else {
            col
        };
        cx.notify();
    }

    /// 主区任务总览：任务专属信息层级。
    pub fn render_tasks_page(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (fg, muted, border) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border)
        };
        let soft_bg: Hsla = crate::ui_theme::overlay(0x0d).into();
        let card_bg = rgb(crate::ui_theme::bg_card());
        let card_border = crate::ui_theme::overlay(0x12);

        let mut all = TaskStore::load().tasks;
        all.sort_by_key(|t| (t.column.sidebar_rank(), std::cmp::Reverse(t.updated_at)));
        let n_all = all.len();
        let n_run = all.iter().filter(|t| t.column.is_active()).count();
        let n_todo = all.iter().filter(|t| t.column.is_todo()).count();
        let n_done = all.iter().filter(|t| t.column == TaskColumn::Done).count();
        if let Some(f) = self.task_column_filter {
            all.retain(|t| match f {
                TaskColumn::Running | TaskColumn::Waiting => t.column.is_active(),
                TaskColumn::Backlog | TaskColumn::Ready => t.column.is_todo(),
                TaskColumn::Review => t.column == TaskColumn::Review,
                TaskColumn::Failed => t.column == TaskColumn::Failed,
                TaskColumn::Done => t.column == TaskColumn::Done,
            });
        }

        let pill =
            |id: &'static str, text: String, col: Option<TaskColumn>, color: Hsla, bg: Hsla| {
                // 「全部」仅在无筛选时高亮
                let active = if col.is_none() {
                    self.task_column_filter.is_none()
                } else {
                    self.task_column_filter == col
                };
                div()
                    .id(id)
                    .px(px(12.))
                    .py(px(5.))
                    .rounded_full()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if active {
                        color
                    } else {
                        Hsla::from(rgba(0x00000000))
                    })
                    .bg(if active { bg } else { soft_bg })
                    .text_sm()
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if active { color } else { muted })
                    .hover(|s| s.bg(bg).text_color(color))
                    .child(text)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.set_task_column_filter(col, cx);
                        }),
                    )
            };

        let c_blue: Hsla = rgb(crate::ui_theme::blue()).into();
        let c_gray: Hsla = rgb(crate::ui_theme::text_muted()).into();
        let c_green: Hsla = rgb(crate::ui_theme::green()).into();
        let blue_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::blue(), 0x28).into();
        let gray_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::text_muted(), 0x28).into();
        let green_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::green(), 0x28).into();

        let summary = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(pill("tp-all", format!("{n_all} 任务"), None, fg, soft_bg))
            .child(pill(
                "tp-run",
                format!("{n_run} 执行中"),
                Some(TaskColumn::Running),
                c_blue,
                blue_tint,
            ))
            .child(pill(
                "tp-todo",
                format!("{n_todo} 待办"),
                Some(TaskColumn::Backlog),
                c_gray,
                gray_tint,
            ))
            .child(pill(
                "tp-done",
                format!("{n_done} 完成"),
                Some(TaskColumn::Done),
                c_green,
                green_tint,
            ));

        let header = div()
            .px_6()
            .pt_5()
            .pb_4()
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xl().font_bold().text_color(fg).child("任务总览"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("点状态徽章可改状态 · 终端右键可绑当前会话新建"),
                            ),
                    )
                    .child(
                        Button::new("tasks-page-new")
                            .label("新建任务")
                            .icon(IconName::Plus)
                            .small()
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_task_modal(window, cx);
                            })),
                    ),
            )
            .child(summary);

        let body = if all.is_empty() {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .py_20()
                .child(
                    div()
                        .size(px(56.))
                        .rounded_full()
                        .bg(soft_bg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::Bot).size(px(28.)).text_color(muted)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(fg)
                        .font_weight(FontWeight::MEDIUM)
                        .child(if n_all == 0 {
                            "还没有任务"
                        } else {
                            "这个筛选下是空的"
                        }),
                )
                .child(div().text_xs().text_color(muted).child(if n_all == 0 {
                    "新建一条，或在终端右键「新建任务」"
                } else {
                    "换个状态 pill，或清除筛选看全部"
                }))
                .when(n_all == 0, |d| {
                    d.child(
                        Button::new("tasks-empty-new")
                            .label("新建任务")
                            .primary()
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_task_modal(window, cx);
                            })),
                    )
                })
        } else {
            let mut grid = div().flex().flex_wrap().gap_4();
            for task in &all {
                grid = grid.child(self.render_task_overview_card(
                    task,
                    card_bg,
                    card_border,
                    fg,
                    muted,
                    cx,
                ));
            }
            grid
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("tasks-overview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_5()
                    .child(body),
            )
    }

    fn render_task_overview_card(
        &self,
        task: &Task,
        card_bg: impl Into<Hsla>,
        card_border: impl Into<Hsla>,
        fg: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let card_bg = card_bg.into();
        let card_border = card_border.into();
        let id = task.id.clone();
        let id_run = id.clone();
        let id_col = id.clone();
        let id_edit = id.clone();
        let id_del = id.clone();
        let title = task.title.clone();
        let proj = project_label(&task.project_cwd);
        let col = task.column;
        // ACP 等待态覆盖通用的「执行中」：让人一眼看到任务卡在等人批/等人答。
        let status_label = if col.is_active()
            && let Some(sid) = task.session_id.as_ref()
        {
            self.acp_waiting_label(sid, cx).unwrap_or(col.label())
        } else {
            col.label()
        };
        let col_color: Hsla = rgb(col.color()).into();
        // 徽章底 = 该列语义色的低透明度版本，与 `col.color()` 同源
        let col_tint: Hsla = crate::ui_theme::tint(col.color(), 0x22).into();
        let body_prev = {
            let t = task.body.trim();
            if t.is_empty() {
                String::new()
            } else if t.chars().count() > 96 {
                format!("{}…", t.chars().take(96).collect::<String>())
            } else {
                t.to_string()
            }
        };
        let has_session = task.session_id.is_some();
        let primary: Option<&'static str> = if col.is_todo() {
            Some("运行")
        } else if col == TaskColumn::Failed {
            Some("重试")
        } else if has_session {
            Some("打开")
        } else if col.is_active() {
            Some("运行")
        } else {
            None
        };
        let schedule_label = if task.kind == TaskKind::Scheduled {
            task.run_at.map(|at| {
                let when = format_run_at_short(at);
                let kind = TaskKind::Scheduled.label();
                if col.is_todo() && at <= now_secs() {
                    format!("{kind} · 已到期 {when}")
                } else if col.is_todo() {
                    format!("{kind} · {when}")
                } else {
                    format!("{kind} · 计划 {when}")
                }
            })
        } else {
            None
        };
        // 待办且可自动执行时标一下（定时已有徽章可省略）
        let auto_label = if task.kind == TaskKind::Once && col.is_todo() {
            Some(if task.auto_run { "自动" } else { "手动" })
        } else {
            None
        };
        let runs = TaskStore::runs_for_task(&task.id);
        let run_label = runs.first().map(|run| {
            if runs.len() == 1 {
                format!("第 1 次 · {}", run.status.label())
            } else {
                format!("第 {} 次 · {}", run.attempt, run.status.label())
            }
        });
        let e_status = cx.entity().clone();
        let id_status = id_col.clone();

        // 状态徽章可点：下拉改状态（不占操作行）
        let status_badge = Button::new(SharedString::from(format!("tc-st-{id}")))
            .label(status_label)
            .xsmall()
            .ghost()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for c in TaskColumn::ui_choices() {
                    let e = e_status.clone();
                    let tid = id_status.clone();
                    let label = c.label();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let tid = tid.clone();
                        e.update(cx, |ws, cx| {
                            ws.set_task_column(&tid, c, cx);
                        });
                    }));
                }
                menu
            });

        // 不画「选中描边」：task_selected 会让某张卡永久亮一圈边，像坏了一样。
        // 卡片只靠 hover 反馈。
        div()
            .id(SharedString::from(format!("task-card-{id}")))
            .w(px(300.))
            .p_4()
            .rounded(px(18.))
            .border_1()
            .border_color(card_border)
            .bg(card_bg)
            .shadow_sm()
            .hover(|d| {
                d.border_color(col_color)
                    .shadow_lg()
                    .bg(rgb(crate::ui_theme::bg_hover()))
            })
            .flex()
            .flex_col()
            .gap_3()
            // 标题：状态点 + 名
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .size(px(9.))
                            .rounded_full()
                            .bg(col_color)
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .text_color(fg)
                            .child(title),
                    ),
            )
            // 元信息：状态徽章 · 项目
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        // 包一层 tint 底，让 ghost 下拉看起来像徽章而不是灰按钮
                        div().rounded_full().bg(col_tint).child(status_badge),
                    )
                    .when(schedule_label.is_some(), |d| {
                        let lab = schedule_label.clone().unwrap_or_default();
                        d.child(
                            div()
                                .rounded_full()
                                .px_2()
                                .py_1()
                                .bg(crate::ui_theme::tint(crate::ui_theme::purple(), 0x22))
                                .text_xs()
                                .text_color(rgb(crate::ui_theme::purple()))
                                .child(lab),
                        )
                    })
                    .when(auto_label.is_some(), |d| {
                        let lab = auto_label.unwrap_or("");
                        let (bg, fg): (Hsla, Hsla) = if task.auto_run {
                            (
                                crate::ui_theme::tint(crate::ui_theme::green(), 0x22).into(),
                                rgb(crate::ui_theme::green()).into(),
                            )
                        } else {
                            (
                                crate::ui_theme::tint(crate::ui_theme::text_muted(), 0x22).into(),
                                muted,
                            )
                        };
                        d.child(
                            div()
                                .rounded_full()
                                .px_2()
                                .py_1()
                                .bg(bg)
                                .text_xs()
                                .text_color(fg)
                                .child(lab),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(if has_session {
                                format!("{proj} · 已绑会话")
                            } else {
                                proj
                            }),
                    ),
            )
            .when(run_label.is_some(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(run_label.unwrap_or_default()),
                )
            })
            // 指令摘要（与会话预览同款深底）
            .when(!body_prev.is_empty(), |d| {
                d.child(
                    div()
                        .p_2()
                        .rounded_lg()
                        .bg(rgb(crate::ui_theme::bg_rail()))
                        .text_xs()
                        .text_color(muted)
                        .line_clamp(3)
                        .child(body_prev),
                )
            })
            // 操作：主操作 + 删除（不再把状态塞进这一行）
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(primary.map(|label| {
                        Button::new(SharedString::from(format!("tc-run-{id}")))
                            .label(label)
                            .small()
                            .primary()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.primary_task_action(&id_run, window, cx);
                            }))
                    }))
                    .child(
                        Button::new(SharedString::from(format!("tc-edit-{id}")))
                            .label("编辑")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_edit_task_modal(&id_edit, window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("tc-del-{id}")))
                            .label("删除")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_task(&id_del, cx);
                            })),
                    ),
            )
    }

    /// 绑到指定终端；`inject` 时键入首包并回车（当前终端上下文执行）。
    pub fn assign_task_to_session(
        &mut self,
        id: &str,
        sid: &str,
        inject: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        let prompt = task_prompt(&task);

        let mut found: Option<(usize, Entity<TerminalView>)> = None;
        for i in 0..self.sessions.len() {
            for leaf in self.sessions[i].term_leaves() {
                if leaf.read(cx).session_id() == sid {
                    found = Some((i, leaf));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((ix, leaf)) = found else {
            eprintln!("[tasks] assign: 找不到会话 {sid}，回退新开终端");
            if inject {
                self.run_task_in_terminal(id, window, cx);
            } else {
                cx.notify();
            }
            return;
        };

        let cwd = leaf.read(cx).cwd();
        let run = if inject {
            let Some(run) =
                TaskStore::begin_pty_run(id, task.launch.as_deref().unwrap_or("existing-session"))
            else {
                return;
            };
            Some(run)
        } else {
            None
        };
        TaskStore::update(id, |t| {
            if let Some(c) = cwd {
                t.project_cwd = c;
            }
        });
        if let Some(run) = run {
            TaskStore::mark_run_started(id, &run.id, sid);
        } else if !inject {
            TaskStore::update(id, |t| t.session_id = Some(sid.to_string()));
        }

        self.activate(ix, window, cx);
        self.sessions[ix].set_active_term(leaf.clone());

        if inject && !prompt.is_empty() {
            leaf.update(cx, |tv, cx| {
                tv.send_text_and_submit(&prompt, cx);
            });
        }

        self.focus_active(window, cx);
        cx.notify();
    }

    /// 按 session_id 聚焦已有侧栏终端；找到返回 true。
    /// ACP 等待子态文案：绑定会话正在等批准 / 等选择 → 「等你批准」/「等你选择」；
    /// 否则 None（任务仍显示通用列状态）。
    fn acp_waiting_label(&self, sid: &str, cx: &mut Context<Self>) -> Option<&'static str> {
        for sess in &self.sessions {
            if let crate::SessionKind::Acp(view) = &sess.kind {
                if view.read(cx).session_id() != sid {
                    continue;
                }
                return match view.read(cx).phase() {
                    smelt_core::acp_session::AcpPhase::AwaitingApproval => Some("等你批准"),
                    smelt_core::acp_session::AcpPhase::AwaitingChoice => Some("等你选择"),
                    _ => None,
                };
            }
        }
        None
    }

    pub fn focus_session_by_id(
        &mut self,
        sid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for i in 0..self.sessions.len() {
            for leaf in self.sessions[i].term_leaves() {
                if leaf.read(cx).session_id() == sid {
                    self.activate(i, window, cx);
                    self.sessions[i].set_active_term(leaf);
                    self.focus_active(window, cx);
                    cx.notify();
                    return true;
                }
            }
        }
        false
    }

    /// 卡片主按钮：待办 → [`Self::run_task`]；已跑过 → 聚焦会话。
    pub fn primary_task_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.task_selected = Some(id.to_string());
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        if task.column.is_todo() || task.column == TaskColumn::Failed {
            self.run_task(id, window, cx);
            return;
        }
        if let Some(sid) = task.session_id.as_ref() {
            if self.focus_session_by_id(sid, window, cx) {
                return;
            }
        }
        // 执行中但会话已丢 → 再新开（按通道路由：ACP 走对话会话）
        if task.column.is_active() {
            if matches!(task.channel, TaskChannel::Acp { .. }) {
                self.run_task_in_acp(id, window, cx);
            } else {
                self.run_task_in_terminal(id, window, cx);
            }
        }
        cx.notify();
    }

    /// 执行任务：有绑定且仍存活的会话 → 注入该终端；否则新开终端。
    pub fn run_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        // 手动运行也守依赖：前置任务没完成就拉起会堵死 FIFO。
        let all = TaskStore::load();
        if !task.dependencies_met(&all.tasks) {
            window.push_notification(Notification::error("前置任务未完成，无法运行"), cx);
            return;
        }
        // ACP 通道：建独立对话会话（有存活 ACP 会话也不注入，任务是独立现场）。
        if matches!(task.channel, TaskChannel::Acp { .. }) {
            return self.run_task_in_acp(id, window, cx);
        }
        if let Some(sid) = task.session_id.clone() {
            // 会话还在：把首包打进该终端（右键新建仅创建后的「开跑」）
            let mut alive = false;
            for sess in &self.sessions {
                let leaves = sess.term_leaves();
                if leaves.iter().any(|l| l.read(cx).session_id() == sid) {
                    alive = true;
                    break;
                }
            }
            if alive {
                self.assign_task_to_session(id, &sid, true, window, cx);
                return;
            }
        }
        self.run_task_in_terminal(id, window, cx);
    }

    /// 在侧栏**新开终端**跑任务：`base_launch + "首包"` 编进 smeltd launch（startup-arg）。
    ///
    /// **不**往 PTY 粘贴/回车——agent 进程启动即带第一条用户消息（可自循环调度）。
    pub fn run_task_in_terminal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        let cwd = if task.project_cwd.trim().is_empty() {
            None
        } else {
            Some(task.project_cwd.clone())
        };
        let entries = active_launch_entries(cx);
        let base_launch = task
            .launch
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| entries.first().map(|e| e.command.clone()))
            .unwrap_or_else(|| "claude".into());
        let label = task.title.clone();
        let prompt = task_prompt(&task);

        // 有首包 → 写文件后拼进 launch；无首包 → 只起空 agent。
        let launch_cmd = if prompt.trim().is_empty() {
            base_launch.clone()
        } else if let Some(path) = write_prompt_file(&task.id, &prompt) {
            build_launch_with_prompt(&base_launch, &path)
        } else {
            // 落盘失败：内联单引号（多行可能不完美，但强于静默失败）
            format!("{base_launch} {}", shell_single_quote(&prompt))
        };

        eprintln!("[tasks] run launch={launch_cmd}");

        let Some(run) = TaskStore::begin_pty_run(id, &base_launch) else {
            return;
        };
        let sid = new_sid();
        // 同 add_session_with_launch：FFI 回调栈上 panic = abort 整个 app，
        // spawn 失败就不起任务终端，留日志。
        let terminal =
            match crate::terminal::Terminal::spawn(24, 80, cwd.as_deref(), &sid, Some(&launch_cmd))
            {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[tasks] 任务终端启动失败（{cwd:?}）：{e:#}");
                    TaskStore::mark_run_failed(id, &run.id, format!("{e:#}"));
                    return;
                }
            };
        let view = cx.new(|cx| {
            TerminalView::from_terminal(
                cx,
                terminal,
                cwd.clone(),
                sid.clone(),
                Some(base_launch.as_str()),
                Some(label.as_str()),
            )
        });
        self.sessions.push(crate::Session::single(view.clone()));
        self.active_session = self.sessions.len() - 1;
        // 存 base（不含 prompt 拼接），再跑时重新拼首包；执行现场归 TaskRun。
        TaskStore::update(id, |t| {
            t.launch = Some(base_launch);
        });
        TaskStore::mark_run_started(id, &run.id, &sid);

        self.save_state(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 在**对话通道**跑任务：建一个独立 ACP 会话（接任务指定的 agent），握手完成后
    /// 自动发首包 prompt（`pending_initial_prompt` 机制）。sid 固定 `acp-<uuid>`，
    /// 供完成/失败边沿按 sid 回查任务。
    pub fn run_task_in_acp(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        let TaskChannel::Acp { agent, profile_id } = &task.channel else {
            return;
        };
        let agent_kind = smelt_core::agent_kind::AcpAgentKind::from_id(agent)
            .unwrap_or(smelt_core::agent_kind::AcpAgentKind::Claude);
        let base_launch = task
            .launch
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| agent_kind.default_cmd());
        let cwd = if task.project_cwd.trim().is_empty() {
            None
        } else {
            Some(task.project_cwd.clone())
        };

        let Some(run) = TaskStore::begin_acp_run(id, &base_launch, agent_kind.id(), profile_id.clone())
        else {
            return;
        };
        let sid = format!("acp-{}", uuid::Uuid::new_v4());

        let request = crate::acp_view::AcpHandoffRequest {
            source: None,
            cwd: cwd.clone(),
            agent: agent_kind,
            launch: smelt_core::agent_kind::AcpLaunchSpec::from_command(base_launch.clone()),
            refresh_launch_from_settings: false,
            profile_id: profile_id.clone(),
            config_values: Vec::new(),
            prompt: task_prompt(&task),
        };
        let view = cx.new(|cx| {
            crate::acp_view::AcpView::start_with_handoff_sid(window, cx, request, Some(sid.clone()))
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(crate::Session {
            ui_id: crate::next_session_ui_id(),
            kind: crate::SessionKind::Acp(view),
            last_updated_at: crate::unix_now_secs(),
            custom_title: Some(task.title.clone()),
            remote_owned: false,
            _acp_persist_sub,
            ui_state: crate::SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        self.active_session = self.sessions.len() - 1;
        // 存 base（重跑时重新拼首包），并把 Run 绑到固定 acp-* sid。
        TaskStore::update(id, |t| {
            t.launch = Some(base_launch);
        });
        TaskStore::mark_run_started(id, &run.id, &sid);

        self.save_state(cx);
        self.focus_active(window, cx);
        cx.notify();
    }
}

// ===================== 测试 =====================

#[cfg(test)]
mod task_model_tests {
    use super::{
        Task, TaskChannel, TaskColumn, TaskFile, TaskKind, TaskRetryPolicy, TaskRun, TaskRunStatus,
        TaskStore, build_launch_with_prompt, parse_local_datetime, shell_single_quote,
    };
    use std::path::Path;

    fn project_key(cwd: &str) -> String {
        let p = Path::new(cwd);
        let s = std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned();
        s.chars()
            .map(|c| match c {
                '/' | '\\' | ':' => '-',
                c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' => c,
                _ => '_',
            })
            .collect()
    }

    #[test]
    fn project_key_is_filesystem_safe() {
        let k = project_key("/Users/foo/bar baz");
        assert!(!k.contains('/'));
        assert!(!k.is_empty());
    }

    #[test]
    fn task_file_json_roundtrip() {
        let mut t = Task::new(
            "/tmp/p".into(),
            "t1".into(),
            "body".into(),
            Some("claude".into()),
        );
        t.column = TaskColumn::Running;
        t.kind = TaskKind::Scheduled;
        t.run_at = Some(1_700_000_000);
        t.depends_on = vec!["dep-1".into()];
        t.retry_policy = TaskRetryPolicy { max_attempts: 5, retry_delay_secs: 30, remix_on_retry: true };
        t.retry_at = Some(1_700_000_100);
        t.channel = TaskChannel::Acp { agent: "claude".into(), profile_id: Some("p1".into()) };
        let run = TaskRun {
            id: "run-1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("sid-1".into()),
            status: TaskRunStatus::Completed,
            error: None,
            created_at: 1,
            started_at: Some(2),
            finished_at: Some(3),
        };
        t.current_run_id = Some(run.id.clone());
        let file = TaskFile {
            tasks: vec![t],
            runs: vec![run],
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        let back: TaskFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tasks[0].title, "t1");
        assert_eq!(back.tasks[0].kind, TaskKind::Scheduled);
        assert_eq!(back.tasks[0].run_at, Some(1_700_000_000));
        assert_eq!(back.tasks[0].current_run_id.as_deref(), Some("run-1"));
        assert_eq!(back.runs[0].status, TaskRunStatus::Completed);
        assert_eq!(back.runs[0].attempt, 1);
        assert_eq!(back.tasks[0].depends_on, vec!["dep-1".to_string()]);
        assert_eq!(back.tasks[0].retry_policy.max_attempts, 5);
        assert_eq!(back.tasks[0].retry_at, Some(1_700_000_100));
        assert_eq!(
            back.tasks[0].channel,
            TaskChannel::Acp { agent: "claude".into(), profile_id: Some("p1".into()) }
        );
        assert_eq!(TaskColumn::Ready.label(), "待办");
        assert_eq!(TaskColumn::Waiting.label(), "执行中");
    }

    #[test]
    fn old_json_defaults_kind_to_once() {
        let json = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x"}]}"#;
        let back: TaskFile = serde_json::from_str(json).unwrap();
        assert_eq!(back.tasks[0].kind, TaskKind::Once);
        assert!(back.tasks[0].run_at.is_none());
        assert!(back.tasks[0].current_run_id.is_none());
        assert!(back.runs.is_empty());
    }

    #[test]
    fn old_json_defaults_new_fields() {
        let json = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x"}]}"#;
        let back: TaskFile = serde_json::from_str(json).unwrap();
        let t = &back.tasks[0];
        assert!(t.depends_on.is_empty());
        assert_eq!(t.retry_policy, TaskRetryPolicy::default());
        assert!(t.retry_at.is_none());
        assert_eq!(t.channel, TaskChannel::Pty);
    }

    #[test]
    fn channel_serde_old_pty_string() {
        // 旧数据：裸 "pty"
        let json = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x","channel":"pty"}]}"#;
        let back: TaskFile = serde_json::from_str(json).unwrap();
        assert_eq!(back.tasks[0].channel, TaskChannel::Pty);
    }

    #[test]
    fn channel_serde_acp_object_roundtrip() {
        let ch = TaskChannel::Acp { agent: "codex".into(), profile_id: None };
        let json = serde_json::to_string(&ch).unwrap();
        let back: TaskChannel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskChannel::Acp { agent: "codex".into(), profile_id: None });
    }

    #[test]
    fn channel_serde_hand_edited_acp_string() {
        // 手改 tasks.json 写裸 "acp" → 映射成 Acp{默认}，防止 load_json 整体回退清空
        let json = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x","channel":"acp"}]}"#;
        let back: TaskFile = serde_json::from_str(json).unwrap();
        assert_eq!(
            back.tasks[0].channel,
            TaskChannel::Acp { agent: String::new(), profile_id: None }
        );
    }

    #[test]
    fn scheduled_is_due_when_past() {
        let mut t = Task::new("/x".into(), "t".into(), "b".into(), None);
        t.kind = TaskKind::Scheduled;
        t.run_at = Some(100);
        assert!(t.is_due(100));
        assert!(t.is_due(200));
        assert!(!t.is_due(99));
        t.column = TaskColumn::Running;
        assert!(!t.is_due(200));
    }

    #[test]
    fn format_run_at_input_roundtrips_to_minute() {
        // 编辑弹窗回填：format→parse 应还原到同一分钟（分精度，丢秒）。
        let secs = 1_800_000_000u64; // 整分（可被 60 整除）
        let s = super::format_run_at_input(secs);
        let back = parse_local_datetime(&s).expect("parse back");
        assert_eq!(back, secs - secs % 60);
    }

    #[test]
    fn parse_local_datetime_accepts_minute_precision() {
        let at = parse_local_datetime("2030-01-15 18:30").expect("parse");
        assert!(at > 1_800_000_000);
        assert!(parse_local_datetime("").is_none());
        assert!(parse_local_datetime("not-a-date").is_none());
    }

    #[test]
    fn is_auto_runnable_skips_future_scheduled_and_manual() {
        let now = 1_000u64;
        let once = Task::new("/p".into(), "a".into(), "b".into(), None);
        assert!(TaskStore::is_auto_runnable(&once, &[], now));

        let mut manual = Task::new("/p".into(), "a".into(), "b".into(), None);
        manual.auto_run = false;
        assert!(!TaskStore::is_auto_runnable(&manual, &[], now));

        let mut sched = Task::new("/p".into(), "a".into(), "b".into(), None);
        sched.kind = TaskKind::Scheduled;
        sched.run_at = Some(2_000);
        assert!(!TaskStore::is_auto_runnable(&sched, &[], now));
        sched.run_at = Some(500);
        assert!(TaskStore::is_auto_runnable(&sched, &[], now));
        sched.auto_run = false;
        assert!(!TaskStore::is_auto_runnable(&sched, &[], now));
    }

    #[test]
    fn claim_next_is_fifo_same_cwd_auto_only() {
        let mut a = Task::new("/proj".into(), "1".into(), "b1".into(), None);
        a.created_at = 10;
        let mut b = Task::new("/proj".into(), "2".into(), "b2".into(), None);
        b.created_at = 5;
        b.auto_run = false; // 更早但不自动 → 跳过
        let mut c = Task::new("/proj".into(), "3".into(), "b3".into(), None);
        c.created_at = 20;
        let list = vec![a, b, c];
        // claim_next_runnable 读磁盘，这里复刻它的筛选逻辑（同 cwd + auto_runnable + FIFO）。
        let mut filtered: Vec<&Task> = list
            .iter()
            .filter(|t| t.project_cwd == "/proj" && TaskStore::is_auto_runnable(t, &list, 999))
            .collect();
        filtered.sort_by_key(|t| t.created_at);
        assert_eq!(filtered[0].title, "1");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn is_auto_runnable_respects_retry_at() {
        let now = 1_000u64;
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        // 重试冷却未到 → 不可取跑
        t.retry_at = Some(now + 10);
        assert!(!TaskStore::is_auto_runnable(&t, &[], now));
        // 冷却已到 → 可取跑
        t.retry_at = Some(now);
        assert!(TaskStore::is_auto_runnable(&t, &[], now));
        t.retry_at = None;
        assert!(TaskStore::is_auto_runnable(&t, &[], now));
    }

    #[test]
    fn is_auto_runnable_respects_deps() {
        let now = 1_000u64;
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into(), None);
        dep.column = TaskColumn::Running;
        let t = Task::new("/p".into(), "a".into(), "b".into(), None);
        let tasks = vec![t.clone()];
        // 空依赖 → 可跑
        assert!(TaskStore::is_auto_runnable(&t, &tasks, now));
        // 依赖未完成 → 不可跑
        let mut t2 = t.clone();
        t2.depends_on = vec![dep.id.clone()];
        let tasks2 = vec![dep, t2.clone()];
        assert!(!TaskStore::is_auto_runnable(&t2, &tasks2, now));
    }

    #[test]
    fn claim_next_skips_blocked_by_deps_then_fifo() {
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into(), None);
        dep.column = TaskColumn::Backlog; // 未完成
        dep.auto_run = false; // 它只是阻塞条件，自身不参与候选
        let mut a = Task::new("/p".into(), "1".into(), "b1".into(), None);
        a.created_at = 10;
        a.depends_on = vec![dep.id.clone()];
        let mut b = Task::new("/p".into(), "2".into(), "b2".into(), None);
        b.created_at = 20;
        let list = vec![dep, a, b];
        let mut filtered: Vec<&Task> = list
            .iter()
            .filter(|t| t.project_cwd == "/p" && TaskStore::is_auto_runnable(t, &list, 999))
            .collect();
        filtered.sort_by_key(|t| t.created_at);
        // 依赖未满足的「1」被跳过，取「2」
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "2");
    }

    #[test]
    fn old_json_defaults_auto_run_true() {
        let json = r#"{"tasks":[{"id":"a","title":"t","body":"b","project_cwd":"/x"}]}"#;
        let back: TaskFile = serde_json::from_str(json).unwrap();
        assert!(back.tasks[0].auto_run);
    }

    #[test]
    fn dependencies_met_all_done() {
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into(), None);
        dep.column = TaskColumn::Done;
        let t = Task::new("/p".into(), "a".into(), "b".into(), None);
        let mut with_dep = t.clone();
        with_dep.depends_on = vec![dep.id.clone()];
        let tasks = vec![dep, with_dep.clone()];
        assert!(with_dep.dependencies_met(&tasks));
    }

    #[test]
    fn dependency_pending_or_running_blocks() {
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into(), None);
        dep.column = TaskColumn::Backlog;
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.depends_on = vec![dep.id.clone()];
        let tasks = vec![dep.clone(), t.clone()];
        assert!(!t.dependencies_met(&tasks));
        dep.column = TaskColumn::Running;
        let tasks = vec![dep, t.clone()];
        assert!(!t.dependencies_met(&tasks));
    }

    #[test]
    fn dependency_deleted_treated_met() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.depends_on = vec!["已删除的id".into()];
        let tasks = vec![t.clone()];
        assert!(t.dependencies_met(&tasks));
    }

    #[test]
    fn self_dependency_met() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.depends_on = vec![t.id.clone()];
        assert!(t.dependencies_met(&[t.clone()]));
    }

    #[test]
    fn apply_failure_retries_within_limit() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.retry_policy = TaskRetryPolicy { max_attempts: 3, retry_delay_secs: 0, remix_on_retry: false };
        t.column = TaskColumn::Running;
        t.session_id = Some("sid".into());
        let mut run = TaskRun {
            id: "run-1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("sid".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
        };
        t.current_run_id = Some(run.id.clone());
        super::apply_failure_to_task(&mut t, &mut run, "boom", 100);
        assert_eq!(run.status, TaskRunStatus::Failed);
        assert_eq!(run.error.as_deref(), Some("boom"));
        // 未超限 → 回待办，无冷却（delay=0），保留 current_run_id，清 session
        assert_eq!(t.column, TaskColumn::Backlog);
        assert!(t.retry_at.is_none());
        assert_eq!(t.current_run_id.as_deref(), Some("run-1"));
        assert!(t.session_id.is_none());
    }

    #[test]
    fn apply_failure_delay_sets_retry_at() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.retry_policy = TaskRetryPolicy { max_attempts: 3, retry_delay_secs: 60, remix_on_retry: false };
        t.column = TaskColumn::Running;
        let mut run = TaskRun {
            id: "run-1".into(),
            task_id: t.id.clone(),
            attempt: 1,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("sid".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
        };
        t.current_run_id = Some(run.id.clone());
        super::apply_failure_to_task(&mut t, &mut run, "boom", 100);
        assert_eq!(t.column, TaskColumn::Backlog);
        assert_eq!(t.retry_at, Some(160));
    }

    #[test]
    fn apply_failure_exhausted_goes_failed_column() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into(), None);
        t.retry_policy = TaskRetryPolicy { max_attempts: 3, retry_delay_secs: 0, remix_on_retry: false };
        t.column = TaskColumn::Running;
        let mut run = TaskRun {
            id: "run-3".into(),
            task_id: t.id.clone(),
            attempt: 3,
            channel: TaskChannel::Pty,
            launch: "claude".into(),
            session_id: Some("sid".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
        };
        t.current_run_id = Some(run.id.clone());
        super::apply_failure_to_task(&mut t, &mut run, "boom", 100);
        // 已用尝试 == max → 落 Failed 列，不再自动跑
        assert_eq!(t.column, TaskColumn::Failed);
        assert!(t.retry_at.is_none());
        assert!(t.session_id.is_none());
    }

    #[test]
    fn retry_policy_allows_retry_boundaries() {
        use super::TaskRetryPolicy;
        let no_retry = TaskRetryPolicy::default();
        assert!(!no_retry.allows_retry(1)); // max=1 → 第一次尝试后不再重试
        let infinite = TaskRetryPolicy { max_attempts: 0, retry_delay_secs: 0, remix_on_retry: false };
        assert!(infinite.allows_retry(1));
        assert!(infinite.allows_retry(100));
        let three = TaskRetryPolicy { max_attempts: 3, retry_delay_secs: 0, remix_on_retry: false };
        assert!(three.allows_retry(1));
        assert!(three.allows_retry(2));
        assert!(!three.allows_retry(3));
    }

    #[test]
    fn task_prompt_is_body_only() {
        let t = Task::new(
            "/x".into(),
            "侧栏标题".into(),
            "真正给 agent 的指令".into(),
            None,
        );
        assert_eq!(super::task_prompt(&t), "真正给 agent 的指令");
    }

    #[test]
    fn task_prompt_falls_back_to_title_when_body_empty() {
        let t = Task::new("/x".into(), "only title".into(), String::new(), None);
        assert_eq!(super::task_prompt(&t), "only title");
    }

    #[test]
    fn title_from_prompt_takes_first_line() {
        assert_eq!(super::title_from_prompt("第一行\n第二行"), "第一行");
    }

    #[test]
    fn shell_single_quote_escapes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn launch_with_prompt_appends_cat_not_dash_p() {
        let p = Path::new("/tmp/prompt.txt");
        let cmd = build_launch_with_prompt("claude --dangerously-skip-permissions", p);
        assert!(cmd.starts_with("claude --dangerously-skip-permissions "));
        assert!(cmd.contains("\"$(cat "));
        assert!(!cmd.contains(" -p "));
        assert!(!cmd.contains("-p "));
    }

    #[test]
    fn empty_base_defaults_to_claude() {
        let p = Path::new("/tmp/p.txt");
        let cmd = build_launch_with_prompt("", p);
        assert!(cmd.starts_with("claude "));
        assert!(cmd.contains("\"$(cat "));
    }
}
