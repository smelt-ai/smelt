//! 本地任务：侧栏统一查看与开跑，**全部走交互终端**（不 `-p` 无头批跑）。
//!
//! - 总览只做会话监控；任务列表在左侧「任务」分组
//! - 默认开跑 = 新开侧栏终端 + `launch "首包"`（CLI 启动参数，**不**模拟粘贴/回车）
//! - 手动可从任务卡的 ACP 菜单新建独立结构化对话；选择仅属于本次 `TaskRun`
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

use crate::settings::{AcpAgentKind, active_launch_entries};
use crate::terminal_view::TerminalView;
use crate::{Workspace, new_sid};

// ===================== 模型 =====================

const MAX_AUTO_TASK_LAUNCHES: usize = 4;

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

/// 任务总览看板的视觉列。旧状态在各自语义列中归并展示，避免任务从总览消失。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskBoardLane {
    Todo,
    Running,
    Blocked,
    Review,
    Done,
}

impl TaskBoardLane {
    const ALL: [Self; 5] = [
        Self::Todo,
        Self::Running,
        Self::Blocked,
        Self::Review,
        Self::Done,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Todo => "待办",
            Self::Running => "执行中",
            Self::Blocked => "遇到阻碍",
            Self::Review => "待确认",
            Self::Done => "已完成",
        }
    }

    fn color(self) -> u32 {
        match self {
            Self::Todo => crate::ui_theme::text_muted(),
            Self::Running => crate::ui_theme::blue(),
            Self::Blocked => crate::ui_theme::red(),
            Self::Review => crate::ui_theme::yellow(),
            Self::Done => crate::ui_theme::green(),
        }
    }

    /// 拖入该看板列时写入的规范状态。旧 ready/waiting 仍会展示在对应列，但不会
    /// 被新操作重新写入。
    fn target_column(self) -> TaskColumn {
        match self {
            Self::Todo => TaskColumn::Backlog,
            Self::Running => TaskColumn::Running,
            Self::Blocked => TaskColumn::Failed,
            Self::Review => TaskColumn::Review,
            Self::Done => TaskColumn::Done,
        }
    }

    fn matches(self, column: TaskColumn) -> bool {
        match self {
            Self::Todo => column.is_todo(),
            Self::Running => column.is_active(),
            Self::Blocked => column == TaskColumn::Failed,
            Self::Review => column == TaskColumn::Review,
            Self::Done => column == TaskColumn::Done,
        }
    }
}

/// 看板卡片拖拽时跟随鼠标的预览。
#[derive(Clone)]
struct TaskDrag {
    id: String,
    title: SharedString,
}

impl Render for TaskDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("task-drag-preview")
            .cursor_grab()
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_xs()
            .text_color(theme.foreground)
            .child(self.title.clone())
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
/// - `session_id`：执行体（smeltd 会话）
/// - `kind` / `run_at`：普通 vs 单次定时
/// - `auto_run`：是否允许系统自动开跑（完成续跑 / 定时扫描）；手动点「运行」始终可以
/// - `depends_on`：前置任务 id，全部 Done 才允许执行
/// - `retry_policy` / `retry_at`：失败自动重试策略与当前冷却到点时刻
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
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

/// 删除确认期间保留用户实际点选的任务快照。
#[derive(Clone, Debug)]
pub(crate) struct TaskDeleteTarget {
    id: String,
    title: String,
    has_active_execution: bool,
}

impl TaskDeleteTarget {
    fn from_task(task: &Task) -> Self {
        Self {
            id: task.id.clone(),
            title: task.title.clone(),
            has_active_execution: task.column.is_active() || task.session_id.is_some(),
        }
    }
}

fn default_true() -> bool {
    true
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
            run_at: None,
            auto_run: true,
            depends_on: Vec::new(),
            retry_policy: TaskRetryPolicy::default(),
            retry_at: None,
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
            // 未超限：回待办，冷却后由自动认领重新取跑。
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

/// 卡片标题已由首包首行生成时，不再把同一行作为摘要重复显示；有后续说明时只显示它。
fn task_card_body_preview(title: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }

    let title = title.trim();
    let details = if title == body {
        String::new()
    } else if title == title_from_prompt(body) {
        let mut found_title_line = false;
        let mut remaining = Vec::new();
        for line in body.lines() {
            if !found_title_line {
                if !line.trim().is_empty() {
                    found_title_line = true;
                }
            } else {
                remaining.push(line);
            }
        }
        remaining.join("\n")
    } else {
        body.to_string()
    };

    let details = details.trim();
    if details.chars().count() > 96 {
        format!("{}…", details.chars().take(96).collect::<String>())
    } else {
        details.to_string()
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

/// 可立即接收任务首包的已打开 ACP 对话。仅用于运行时菜单，不会写进任务。
#[derive(Clone)]
pub(crate) struct AcpTaskTarget {
    pub session_id: String,
    pub label: String,
}

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

    /// 为任务创建一次 ACP 对话执行尝试。agent 是本次运行时选择，不写回 Task。
    pub fn begin_acp_run(
        task_id: &str,
        launch: &str,
        agent: AcpAgentKind,
        profile_id: Option<String>,
    ) -> Option<TaskRun> {
        Self::begin_run(
            task_id,
            launch,
            TaskChannel::Acp {
                agent: agent.id().to_string(),
                profile_id,
            },
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
            TaskKind::Scheduled => t.is_due(now),
        }
    }

    /// Return at most one eligible task project per scan, ordered FIFO.
    pub fn auto_claim_cwds() -> Vec<String> {
        let file = Self::load();
        auto_claim_cwds_from_tasks(&file.tasks, now_secs())
    }

    /// Atomically claim and create a run through smeltd, preventing GUI and
    /// `smelt-task` from launching the same task concurrently.
    pub fn claim_next_runnable(prefer_cwd: &str, launch: &str) -> Option<(Task, TaskRun)> {
        let cwd = prefer_cwd.trim_end_matches('/');
        if cwd.is_empty() {
            return None;
        }
        let response = match smelt_core::task::request_task_op(serde_json::json!({
            "op": "task_claim",
            "cwd": cwd,
            "launch": launch,
        })) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("[tasks] 自动认领请求失败，保留待办等待重试：{error}");
                return None;
            }
        };
        if response["task"].is_null() {
            return None;
        }
        let task: Task = match serde_json::from_value(response["task"].clone()) {
            Ok(task) => task,
            Err(error) => {
                Self::fail_malformed_claim(&response, &error.to_string());
                return None;
            }
        };
        let run: TaskRun = match serde_json::from_value(response["run"].clone()) {
            Ok(run) => run,
            Err(error) => {
                Self::fail_malformed_claim(&response, &error.to_string());
                return None;
            }
        };
        Self::mutate(|file| {
            if let Some(slot) = file.tasks.iter_mut().find(|known| known.id == task.id) {
                *slot = task.clone();
            } else {
                file.tasks.push(task.clone());
            }
            if let Some(slot) = file.runs.iter_mut().find(|known| known.id == run.id) {
                *slot = run.clone();
            } else {
                file.runs.push(run.clone());
            }
        });
        Some((task, run))
    }

    fn fail_malformed_claim(response: &serde_json::Value, error: &str) {
        eprintln!("[tasks] 自动认领响应无法解析：{error}");
        let Some(task_id) = response["task"]["id"].as_str() else {
            return;
        };
        let Some(run_id) = response["run"]["id"].as_str() else {
            return;
        };
        let failure = serde_json::json!({
            "op": "task_run_failed",
            "task_id": task_id,
            "run_id": run_id,
            "error": "自动认领响应无法解析",
        });
        if smelt_core::task::request_task_op(failure).is_ok() {
            Self::refresh_from_smeltd();
        }
    }

}

fn auto_claim_cwds_from_tasks(tasks: &[Task], now: u64) -> Vec<String> {
    let mut candidates: Vec<_> = tasks
        .iter()
        .filter(|task| TaskStore::is_auto_runnable(task, tasks, now))
        .filter_map(|task| {
            let cwd = task.project_cwd.trim_end_matches('/');
            (!cwd.is_empty()).then(|| (task.created_at, cwd.to_string()))
        })
        .collect();
    candidates.sort_by_key(|(created_at, _)| *created_at);
    let mut cwds = Vec::new();
    for (_, cwd) in candidates {
        if !cwds.iter().any(|known| known == &cwd) {
            cwds.push(cwd);
        }
    }
    cwds
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

    pub fn set_task_bind_project(&mut self, cwd: String, cx: &mut Context<Self>) {
        self.task_bind_project = Some(cwd);
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

    /// 从项目 TASK 面板新建：固定项目 cwd，但不预绑任何已有会话。
    pub fn open_new_task_for_project(
        &mut self,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_task_modal(window, cx);
        self.task_bind_session = None;
        self.task_bind_project = Some(cwd);
        cx.notify();
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
                .placeholder("描述要完成的工作…")
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
        self.task_kind = task.kind;
        self.task_auto_run = task.auto_run;

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
        TaskStore::update(&id, |t| {
            t.title = title;
            t.body = body;
            t.kind = kind;
            t.run_at = run_at;
            t.auto_run = auto_run;
            t.project_cwd = cwd;
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
        // 清掉绑定，避免下次侧栏新建仍绑旧终端
        let sid = self.task_bind_session.take();
        let mut task = Task::new(cwd, title, body);
        task.kind = kind;
        task.run_at = run_at;
        task.auto_run = auto_run;
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

        if should_run {
            if let Some(sid) = sid {
                self.assign_task_to_session(&id, &sid, true, window, cx);
            } else {
                self.run_task(&id, window, cx);
            }
        } else {
            // 「仅创建」不预绑当前会话；任务保持与 Agent 无关，直到实际运行。
            cx.notify();
        }
    }

    /// Toggle future automatic claims. Running tasks are not interrupted.
    pub fn set_task_auto_claim_enabled(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task_auto_claim_enabled == enabled {
            return;
        }
        self.task_auto_claim_enabled = enabled;
        self.save_state(cx);
        cx.notify();
        if enabled {
            self.tick_auto_claim_tasks(window, cx);
        }
    }

    /// Reconcile task state, then atomically claim one runnable task per project.
    pub fn tick_auto_claim_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        TaskStore::refresh_from_smeltd();
        self.reconcile_tasks(cx);
        if !self.task_auto_claim_enabled {
            return;
        }
        for cwd in TaskStore::auto_claim_cwds() {
            self.claim_and_launch_next_task(&cwd, window, cx);
        }
    }

    /// A completed task can free its project's serialized automatic queue.
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
        if self.task_auto_claim_enabled {
            self.claim_and_launch_next_task(&cwd, window, cx);
        }
    }

    fn claim_and_launch_next_task(
        &mut self,
        cwd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.task_auto_claim_enabled
            || self.auto_task_launches_inflight >= MAX_AUTO_TASK_LAUNCHES
        {
            return false;
        }
        let base_launch = active_launch_entries(cx)
            .first()
            .map(|entry| entry.command.clone())
            .unwrap_or_else(|| "claude".into());
        let Some((task, run)) = TaskStore::claim_next_runnable(cwd, &base_launch) else {
            return false;
        };
        self.launch_task_in_terminal(task, run, base_launch, false, window, cx);
        true
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

        let editing = self.task_editing.is_some();
        let on_existing = self.task_bind_session.is_some();
        let is_scheduled = self.task_kind == TaskKind::Scheduled;
        let auto_run = self.task_auto_run || is_scheduled;
        let exec_hint = if is_scheduled {
            "到点后按当前默认启动项新开终端（单次）；任务本身不绑定 Agent"
        } else if auto_run {
            "可自动执行：前一条做完 / 队列有空时系统会接着跑；运行时才解析默认启动项"
        } else if on_existing {
            "仅手动：不会被完成续跑取走；运行 = 键入指令并回车进当前终端"
        } else {
            "仅手动：点「运行」时按当前默认启动项开终端；不会被系统自动取走"
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

        // 高级选项折叠区：类型、自动执行和定时时间都收进来。
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
                    .child(field_label("任务说明 · 运行时首包（必填）"))
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

    /// Request deletion without immediately removing the task or its runs.
    pub fn start_delete_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        self.task_delete_target = Some(TaskDeleteTarget::from_task(&task));
        cx.notify();
    }

    pub fn cancel_delete_task(&mut self, cx: &mut Context<Self>) {
        self.task_delete_target = None;
        cx.notify();
    }

    pub fn confirm_delete_task(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.task_delete_target.take() else {
            return;
        };
        self.delete_task(&target.id, cx);
    }

    pub fn render_delete_task_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some(target) = self.task_delete_target.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(true);
        let title = if target.title.trim().is_empty() {
            "未命名任务"
        } else {
            target.title.as_str()
        };
        let content = v_flex()
            .gap_3()
            .child(div().font_bold().text_color(fg).text_lg().child("确定删除这个任务吗？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("将永久删除「{title}」及其运行记录，此操作不可撤销。")),
            )
            .when(target.has_active_execution, |content| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(rgb(crate::ui_theme::red()))
                        .child("任务正在执行或已关联会话；删除不会停止会话。"),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-task",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_task(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-task",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_task(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    fn delete_task(&mut self, id: &str, cx: &mut Context<Self>) {
        self.task_delete_target = None;
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

    /// 将看板卡片移到目标列。落在同一语义列时保留旧 ready/waiting 状态，不制造
    /// 无意义的写盘；跨列时复用状态下拉的更新路径。
    fn move_task_to_board_lane(&mut self, id: &str, lane: TaskBoardLane, cx: &mut Context<Self>) {
        if TaskStore::get(id).is_some_and(|task| !lane.matches(task.column)) {
            self.set_task_column(id, lane.target_column(), cx);
        } else {
            cx.notify();
        }
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
        let card_bg: Hsla = rgb(crate::ui_theme::bg_card()).into();
        let card_border: Hsla = crate::ui_theme::overlay(0x12).into();

        let mut all = TaskStore::load().tasks;
        all.sort_by_key(|t| (t.column.sidebar_rank(), std::cmp::Reverse(t.updated_at)));
        let n_all = all.len();
        let n_todo = all.iter().filter(|t| t.column.is_todo()).count();
        let n_run = all.iter().filter(|t| t.column.is_active()).count();
        let n_failed = all
            .iter()
            .filter(|t| t.column == TaskColumn::Failed)
            .count();
        let n_review = all
            .iter()
            .filter(|t| t.column == TaskColumn::Review)
            .count();
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
        let c_red: Hsla = rgb(crate::ui_theme::red()).into();
        let c_yellow: Hsla = rgb(crate::ui_theme::yellow()).into();
        let c_green: Hsla = rgb(crate::ui_theme::green()).into();
        let blue_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::blue(), 0x28).into();
        let gray_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::text_muted(), 0x28).into();
        let red_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::red(), 0x28).into();
        let yellow_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::yellow(), 0x28).into();
        let green_tint: Hsla = crate::ui_theme::tint(crate::ui_theme::green(), 0x28).into();
        let auto_claim_enabled = self.task_auto_claim_enabled;
        let auto_claim_color = if auto_claim_enabled { c_green } else { c_gray };
        let auto_claim = div()
            .id("tasks-auto-claim-toggle")
            .px_3()
            .py_1()
            .rounded_full()
            .border_1()
            .border_color(auto_claim_color)
            .bg(if auto_claim_enabled { green_tint } else { gray_tint })
            .cursor_pointer()
            .text_sm()
            .text_color(auto_claim_color)
            .hover(|style| style.opacity(0.82))
            .child(if auto_claim_enabled {
                "自动认领中 · 暂停"
            } else {
                "自动认领已暂停 · 恢复"
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    let enabled = !this.task_auto_claim_enabled;
                    this.set_task_auto_claim_enabled(enabled, window, cx);
                    window.push_notification(
                        if enabled {
                            Notification::success("已恢复自动认领")
                        } else {
                            Notification::info("已暂停自动认领；正在执行的任务不会中断")
                        },
                        cx,
                    );
                }),
            );

        let summary = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(pill(
                "tp-all",
                format!("全部 {n_all}"),
                None,
                fg,
                soft_bg,
            ))
            .child(pill(
                "tp-todo",
                format!("待办 {n_todo}"),
                Some(TaskColumn::Backlog),
                c_gray,
                gray_tint,
            ))
            .child(pill(
                "tp-run",
                format!("执行中 {n_run}"),
                Some(TaskColumn::Running),
                c_blue,
                blue_tint,
            ))
            .child(pill(
                "tp-blocked",
                format!("阻碍 {n_failed}"),
                Some(TaskColumn::Failed),
                c_red,
                red_tint,
            ))
            .child(pill(
                "tp-review",
                format!("待确认 {n_review}"),
                Some(TaskColumn::Review),
                c_yellow,
                yellow_tint,
            ))
            .child(pill(
                "tp-done",
                format!("完成 {n_done}"),
                Some(TaskColumn::Done),
                c_green,
                green_tint,
            ))
            .child(auto_claim);

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
                                    .child("拖动卡片到状态列可改状态 · 点状态徽章也可修改"),
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
            let visible_lanes: Vec<_> = TaskBoardLane::ALL
                .into_iter()
                .filter(|lane| match self.task_column_filter {
                    Some(filter) => lane.matches(filter),
                    None => true,
                })
                .collect();
            let board_width = visible_lanes.len() as f32 * 304.
                + visible_lanes.len().saturating_sub(1) as f32 * 16.;
            let mut board = div()
                .flex()
                .items_start()
                .gap_4()
                .min_w(px(board_width));
            for lane in visible_lanes {
                let tasks: Vec<_> = all
                    .iter()
                    .filter(|task| lane.matches(task.column))
                    .collect();
                board = board.child(self.render_task_board_lane(
                    lane,
                    &tasks,
                    card_bg,
                    card_border,
                    fg,
                    muted,
                    cx,
                ));
            }
            board
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
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .px_6()
                    .py_5()
                    .child(body),
            )
    }

    fn render_task_board_lane(
        &self,
        lane: TaskBoardLane,
        tasks: &[&Task],
        card_bg: Hsla,
        card_border: Hsla,
        fg: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let lane_color: Hsla = rgb(lane.color()).into();
        let lane_tint: Hsla = crate::ui_theme::tint(lane.color(), 0x20).into();
        let mut cards = v_flex().gap_3();
        if tasks.is_empty() {
            cards = cards.child(
                div()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(crate::ui_theme::overlay(0x12))
                    .bg(crate::ui_theme::overlay(0x08))
                    .px_3()
                    .py_5()
                    .text_xs()
                    .text_color(muted)
                    .child("暂无任务"),
            );
        } else {
            for task in tasks {
                cards = cards.child(self.render_task_board_card(
                    task,
                    card_bg,
                    card_border,
                    fg,
                    muted,
                    cx,
                ));
            }
        }

        let e_drop = cx.entity().clone();
        div()
            .w(px(304.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .min_h(px(360.))
            .rounded(px(12.))
            .border_1()
            .border_color(crate::ui_theme::overlay(0x10))
            .bg(crate::ui_theme::overlay(0x08))
            .drag_over::<TaskDrag>(move |style, _, _, _| {
                style.border_color(lane_color).bg(lane_tint)
            })
            .on_drop(move |drag: &TaskDrag, _window, cx| {
                let task_id = drag.id.clone();
                e_drop.update(cx, |ws, cx| {
                    ws.move_task_to_board_lane(&task_id, lane, cx);
                });
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(lane_tint)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .size(px(8.))
                                    .rounded_full()
                                    .bg(lane_color)
                                    .flex_shrink_0(),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(fg)
                                    .child(lane.label()),
                            ),
                    )
                    .child(
                        div()
                            .min_w(px(22.))
                            .h(px(22.))
                            .rounded_full()
                            .bg(lane_tint)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_semibold()
                            .text_color(lane_color)
                            .child(tasks.len().to_string()),
                    ),
            )
            .child(cards)
    }

    fn render_task_board_card(
        &self,
        task: &Task,
        card_bg: Hsla,
        card_border: Hsla,
        fg: Hsla,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = task.id.clone();
        let id_run = id.clone();
        let id_acp = id.clone();
        let id_col = id.clone();
        let id_edit = id.clone();
        let id_del = id.clone();
        let title = task.title.clone();
        let drag_title: SharedString = title.clone().into();
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
        let body_prev = task_card_body_preview(&title, &task.body);
        let has_session = task.session_id.is_some();
        let primary: Option<&'static str> = if col.is_todo() {
            Some("终端")
        } else if col == TaskColumn::Failed {
            Some("重试")
        } else if has_session {
            Some("打开")
        } else if col.is_active() {
            Some("终端")
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
        let e_acp = cx.entity().clone();
        let acp_targets = self.idle_acp_task_targets(cx);
        let can_run_in_acp = col.is_todo() || col == TaskColumn::Failed;

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
            .w_full()
            .p_3()
            .rounded(px(10.))
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
            .gap_2()
            // 标题：状态点 + 名
            .child(
                div()
                    .id(SharedString::from(format!("task-card-drag-{id}")))
                    .flex()
                    .items_start()
                    .gap_2()
                    .min_w_0()
                    .cursor_grab()
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("拖动到状态列可改变状态")
                            .build(window, cx)
                    })
                    .on_drag(
                        TaskDrag {
                            id: id.clone(),
                            title: drag_title,
                        },
                        move |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .child(
                        div()
                            .size(px(9.))
                            .rounded_full()
                            .bg(col_color)
                            .mt(px(5.))
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .line_clamp(2)
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
                    .flex_wrap()
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
                        .line_clamp(2)
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
                    .when(can_run_in_acp, |d| {
                        d.child(
                            Button::new(SharedString::from(format!("tc-acp-{id}")))
                                .label("对话")
                                .small()
                                .ghost()
                                .tooltip("在原生对话中执行任务，可选择或复用 Agent 对话")
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let mut menu = menu;
                                    if acp_targets.is_empty() {
                                        menu = menu.item(
                                            PopupMenuItem::new("没有空闲的 ACP 对话")
                                                .disabled(true),
                                        );
                                    } else {
                                        menu = menu.item(
                                            PopupMenuItem::new("发送到空闲的已打开 ACP 对话")
                                                .disabled(true),
                                        );
                                        for target in &acp_targets {
                                            let task_id = id_acp.clone();
                                            let session_id = target.session_id.clone();
                                            let label = target.label.clone();
                                            let e = e_acp.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(label).on_click(
                                                    move |_, window, cx| {
                                                        let task_id = task_id.clone();
                                                        let session_id = session_id.clone();
                                                        e.update(cx, |ws, cx| {
                                                            ws.run_task_in_open_acp(
                                                                &task_id,
                                                                &session_id,
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    },
                                                ),
                                            );
                                        }
                                    }
                                    menu = menu
                                        .separator()
                                        .item(PopupMenuItem::new("新建 ACP 对话").disabled(true));
                                    for agent in AcpAgentKind::ALL {
                                        let task_id = id_acp.clone();
                                        let e = e_acp.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(format!(
                                                "{} ACP 对话",
                                                agent.label()
                                            ))
                                            .on_click(move |_, window, cx| {
                                                let task_id = task_id.clone();
                                                e.update(cx, |ws, cx| {
                                                    ws.run_task_in_acp(
                                                        &task_id, agent, window, cx,
                                                    );
                                                });
                                            }),
                                        );
                                    }
                                    menu
                                }),
                        )
                    })
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
                                this.start_delete_task(&id_del, cx);
                            })),
                    ),
            )
    }

    /// 在指定终端执行任务，并在 `inject` 时键入首包并回车。
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
            let Some(run) = TaskStore::begin_pty_run(id, "existing-session") else {
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

    /// 按 session_id 聚焦已有侧栏终端或 ACP 对话；找到返回 true。
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
            let acp_view = match &self.sessions[i].kind {
                crate::SessionKind::Acp(view) => Some(view.clone()),
                crate::SessionKind::Term { .. } => None,
            };
            if acp_view.is_some_and(|view| view.read(cx).session_id() == sid) {
                self.activate(i, window, cx);
                return true;
            }
        }
        false
    }

    /// 已打开且能立即接收首包的 ACP 对话。忙碌的对话不出现在任务菜单中，避免
    /// 当前回合结束时把尚未发送的任务错误标记为完成。
    pub(crate) fn idle_acp_task_targets(&self, cx: &App) -> Vec<AcpTaskTarget> {
        let active_task_sessions: HashSet<String> = TaskStore::load()
            .tasks
            .into_iter()
            .filter(|task| task.column.is_active())
            .filter_map(|task| task.session_id)
            .collect();
        self.sessions
            .iter()
            .filter_map(|session| {
                let crate::SessionKind::Acp(view) = &session.kind else {
                    return None;
                };
                let (session_id, agent) = {
                    let view = view.read(cx);
                    if !view.can_send_prompt_immediately() {
                        return None;
                    }
                    (view.session_id().to_string(), view.agent_kind())
                };
                if active_task_sessions.contains(&session_id) {
                    return None;
                }
                Some(AcpTaskTarget {
                    session_id,
                    label: format!("{} · {}", agent.short_label(), session.title(cx)),
                })
            })
            .collect()
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
        // 执行中但会话已丢 → 再新开终端。
        if task.column.is_active() {
            self.run_task_in_terminal(id, window, cx);
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
        if self.task_launching.contains(id) {
            window.push_notification(Notification::info("任务终端正在启动"), cx);
            return;
        }
        let entries = active_launch_entries(cx);
        let base_launch = entries
            .first()
            .map(|e| e.command.clone())
            .unwrap_or_else(|| "claude".into());
        let Some(run) = TaskStore::begin_pty_run(id, &base_launch) else {
            return;
        };
        self.launch_task_in_terminal(task, run, base_launch, true, window, cx);
    }

    /// Start an already-created run off the UI thread. Automatic launches do
    /// not take focus, while manual launches retain the previous behavior.
    fn launch_task_in_terminal(
        &mut self,
        task: Task,
        run: TaskRun,
        base_launch: String,
        focus_on_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task_id = task.id.clone();
        let run_id = run.id.clone();
        let cwd = (!task.project_cwd.trim().is_empty()).then(|| task.project_cwd.clone());
        let label = task.title.clone();
        let sid = new_sid();
        let task_id_for_ui = task_id.clone();
        let task_id_bg = task_id.clone();
        let run_id_bg = run_id.clone();
        let task_bg = task.clone();
        let cwd_bg = cwd.clone();
        let sid_bg = sid.clone();
        let base_launch_bg = base_launch.clone();
        let automatic = !focus_on_open;
        let (tx, rx) = smol::channel::bounded(1);
        self.task_launching.insert(task_id.clone());
        if automatic {
            self.auto_task_launches_inflight += 1;
        }

        let spawn = std::thread::Builder::new()
            .name("smelt-spawn-task".into())
            .spawn(move || {
                let prompt = task_prompt(&task_bg);
                let launch_cmd = if prompt.trim().is_empty() {
                    base_launch_bg.clone()
                } else if let Some(path) = write_prompt_file(&task_bg.id, &prompt) {
                    build_launch_with_prompt(&base_launch_bg, &path)
                } else {
                    format!("{base_launch_bg} {}", shell_single_quote(&prompt))
                };
                let result = match crate::terminal::Terminal::spawn(
                    24,
                    80,
                    cwd_bg.as_deref(),
                    &sid_bg,
                    Some(&launch_cmd),
                ) {
                    Ok(terminal) => {
                        let attached =
                            TaskStore::mark_run_started(&task_id_bg, &run_id_bg, &sid_bg);
                        Ok((terminal, attached))
                    }
                    Err(error) => {
                        let message = format!("任务终端启动失败（{cwd_bg:?}）：{error:#}");
                        TaskStore::mark_run_failed(&task_id_bg, &run_id_bg, message.clone());
                        Err(message)
                    }
                };
                let _ = tx.send_blocking(result);
            });
        if let Err(error) = spawn {
            let message = format!("无法创建任务启动线程：{error}");
            TaskStore::mark_run_failed(&task_id, &run_id, message.clone());
            self.task_launching.remove(&task_id);
            if automatic {
                self.auto_task_launches_inflight =
                    self.auto_task_launches_inflight.saturating_sub(1);
            }
            if focus_on_open {
                window.push_notification(Notification::error(message), cx);
            }
            return;
        }

        cx.spawn_in(window, async move |this, cx| {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("任务启动线程意外断开".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.task_launching.remove(&task_id_for_ui);
                if automatic {
                    this.auto_task_launches_inflight =
                        this.auto_task_launches_inflight.saturating_sub(1);
                }
                match result {
                    Ok((terminal, attached)) => {
                        let view = cx.new(|cx| {
                            TerminalView::from_terminal(
                                cx,
                                terminal,
                                cwd.clone(),
                                sid,
                                Some(base_launch.as_str()),
                                Some(label.as_str()),
                            )
                        });
                        this.sessions.push(crate::Session::single(view));
                        this.session_list_revision = this.session_list_revision.wrapping_add(1);
                        if focus_on_open {
                            this.active_session = this.sessions.len() - 1;
                        }
                        this.save_state(cx);
                        if !attached {
                            window.push_notification(
                                Notification::error("任务已删除或变更，终端未关联到任务记录"),
                                cx,
                            );
                        }
                        if focus_on_open {
                            this.focus_active(window, cx);
                        } else if attached && this.task_auto_claim_enabled {
                            this.tick_auto_claim_tasks(window, cx);
                        }
                    }
                    Err(message) if focus_on_open => {
                        window.push_notification(Notification::error(message), cx);
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 在新的 ACP 对话中执行任务。agent 仅是这次执行的运行时选择，会作为
    /// `TaskRun::channel` 快照落盘，不会绑定到 Task。
    pub fn run_task_in_acp(
        &mut self,
        id: &str,
        agent: AcpAgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        let all = TaskStore::load();
        if !task.dependencies_met(&all.tasks) {
            window.push_notification(Notification::error("前置任务未完成，无法运行"), cx);
            return;
        }

        let cwd = (!task.project_cwd.trim().is_empty()).then(|| task.project_cwd.clone());
        let launch = crate::settings::acp_cmd_for(agent, cx);
        let Some(run) = TaskStore::begin_acp_run(id, &launch, agent, None) else {
            return;
        };
        let sid = format!("acp-{}", uuid::Uuid::new_v4());
        let request = crate::acp_view::AcpHandoffRequest {
            source: None,
            cwd: cwd.clone(),
            agent,
            launch: smelt_core::agent_kind::AcpLaunchSpec::from_command(launch),
            refresh_launch_from_settings: true,
            profile_id: None,
            config_values: Vec::new(),
            prompt: task_prompt(&task),
        };
        self.remember_session_project(cwd.as_deref());
        let view = cx.new(|cx| {
            crate::acp_view::AcpView::start_with_handoff_sid(window, cx, request, Some(sid.clone()))
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(crate::Session {
            ui_id: crate::next_session_ui_id(),
            kind: crate::SessionKind::Acp(view),
            last_updated_at: crate::unix_now_secs(),
            custom_title: Some(task.title),
            remote_owned: false,
            _acp_persist_sub,
            ui_state: crate::SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        TaskStore::mark_run_started(id, &run.id, &sid);
        self.activate(self.sessions.len() - 1, window, cx);
    }

    /// 将任务首包发到已打开、空闲的 ACP 对话。执行现场的 agent、profile 与启动
    /// 命令只写进 `TaskRun`，任务本身仍与 agent 无关。
    pub fn run_task_in_open_acp(
        &mut self,
        id: &str,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = TaskStore::get(id) else {
            return;
        };
        let all = TaskStore::load();
        if !task.dependencies_met(&all.tasks) {
            window.push_notification(Notification::error("前置任务未完成，无法运行"), cx);
            return;
        }
        if all.tasks.iter().any(|task| {
            task.id != id
                && task.column.is_active()
                && task.session_id.as_deref() == Some(session_id)
        }) {
            window.push_notification(
                Notification::error("ACP 对话仍有任务在执行，请等它完成后重试"),
                cx,
            );
            return;
        }

        let Some((session_index, view)) = self
            .sessions
            .iter()
            .enumerate()
            .find_map(|(index, session)| match &session.kind {
                crate::SessionKind::Acp(view) if view.read(cx).session_id() == session_id => {
                    Some((index, view.clone()))
                }
                crate::SessionKind::Acp(_) | crate::SessionKind::Term { .. } => None,
            })
        else {
            window.push_notification(Notification::error("ACP 对话已关闭，请重新选择"), cx);
            return;
        };
        let (agent, launch, profile_id, ready) = {
            let view = view.read(cx);
            (
                view.agent_kind(),
                view.launch_spec().command,
                view.profile_id().map(str::to_string),
                view.can_send_prompt_immediately(),
            )
        };
        if !ready {
            window.push_notification(
                Notification::error("ACP 对话正在处理其他消息，请等它空闲后重试"),
                cx,
            );
            return;
        }

        let Some(run) = TaskStore::begin_acp_run(id, &launch, agent, profile_id) else {
            return;
        };
        if !TaskStore::mark_run_started(id, &run.id, session_id) {
            TaskStore::mark_run_failed(id, &run.id, "无法关联 ACP 对话");
            window.push_notification(Notification::error("无法关联 ACP 对话，请重试"), cx);
            return;
        }
        let prompt = task_prompt(&task);
        let sent = view.update(cx, |view, cx| view.try_send_prompt_immediately(prompt, cx));
        if !sent {
            TaskStore::mark_run_failed(id, &run.id, "ACP 对话发送首包失败");
            window.push_notification(
                Notification::error("ACP 对话已不在空闲状态，任务未发送"),
                cx,
            );
            return;
        }
        self.activate(session_index, window, cx);
    }

}

// ===================== 测试 =====================

#[cfg(test)]
mod task_model_tests {
    use super::{
        Task, TaskBoardLane, TaskChannel, TaskColumn, TaskFile, TaskKind, TaskRetryPolicy, TaskRun,
        TaskRunStatus, TaskStore, auto_claim_cwds_from_tasks, build_launch_with_prompt,
        parse_local_datetime, shell_single_quote,
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
    fn task_board_lanes_cover_each_task_column_once() {
        for column in [
            TaskColumn::Backlog,
            TaskColumn::Ready,
            TaskColumn::Running,
            TaskColumn::Waiting,
            TaskColumn::Review,
            TaskColumn::Failed,
            TaskColumn::Done,
        ] {
            assert_eq!(
                TaskBoardLane::ALL
                    .into_iter()
                    .filter(|lane| lane.matches(column))
                    .count(),
                1,
                "{column:?} must appear in exactly one board lane"
            );
        }
    }

    #[test]
    fn task_board_lanes_write_canonical_target_columns() {
        assert_eq!(TaskBoardLane::Todo.target_column(), TaskColumn::Backlog);
        assert_eq!(TaskBoardLane::Running.target_column(), TaskColumn::Running);
        assert_eq!(TaskBoardLane::Blocked.target_column(), TaskColumn::Failed);
        assert_eq!(TaskBoardLane::Review.target_column(), TaskColumn::Review);
        assert_eq!(TaskBoardLane::Done.target_column(), TaskColumn::Done);
    }

    #[test]
    fn task_file_json_roundtrip() {
        let mut t = Task::new(
            "/tmp/p".into(),
            "t1".into(),
            "body".into(),
        );
        t.column = TaskColumn::Running;
        t.kind = TaskKind::Scheduled;
        t.run_at = Some(1_700_000_000);
        t.depends_on = vec!["dep-1".into()];
        t.retry_policy = TaskRetryPolicy { max_attempts: 5, retry_delay_secs: 30, remix_on_retry: true };
        t.retry_at = Some(1_700_000_100);
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
        assert_eq!(back.runs[0].channel, TaskChannel::Pty);
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
    }

    #[test]
    fn channel_serde_old_pty_string() {
        // 旧数据：裸 "pty"
        let back: TaskChannel = serde_json::from_str(r#""pty""#).unwrap();
        assert_eq!(back, TaskChannel::Pty);
    }

    #[test]
    fn channel_serde_acp_object_roundtrip() {
        let ch = TaskChannel::Acp { agent: "codex".into(), profile_id: None };
        let json = serde_json::to_string(&ch).unwrap();
        let back: TaskChannel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskChannel::Acp { agent: "codex".into(), profile_id: None });
    }

    #[test]
    fn acp_agent_is_recorded_on_the_run_not_the_task() {
        let task = Task::new("/tmp/p".into(), "t1".into(), "body".into());
        let run = TaskRun {
            id: "run-1".into(),
            task_id: task.id.clone(),
            attempt: 1,
            channel: TaskChannel::Acp {
                agent: "codex".into(),
                profile_id: Some("workspace-1".into()),
            },
            launch: "codex app-server".into(),
            session_id: Some("acp-run-1".into()),
            status: TaskRunStatus::Running,
            error: None,
            created_at: 1,
            started_at: Some(1),
            finished_at: None,
        };
        let value = serde_json::to_value(TaskFile {
            tasks: vec![task],
            runs: vec![run],
        })
        .unwrap();

        assert!(value["tasks"][0].get("channel").is_none());
        assert!(value["tasks"][0].get("launch").is_none());
        assert_eq!(value["runs"][0]["channel"]["acp"]["agent"], "codex");
        assert_eq!(
            value["runs"][0]["channel"]["acp"]["profile_id"],
            "workspace-1"
        );
        assert_eq!(value["runs"][0]["launch"], "codex app-server");
    }

    #[test]
    fn channel_serde_hand_edited_acp_string() {
        // 旧执行记录里的裸 "acp" → 映射成 Acp{默认}，防止 load_json 整体回退清空。
        let back: TaskChannel = serde_json::from_str(r#""acp""#).unwrap();
        assert_eq!(
            back,
            TaskChannel::Acp { agent: String::new(), profile_id: None }
        );
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
    fn scheduled_is_due_when_past() {
        let mut t = Task::new("/x".into(), "t".into(), "b".into());
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
        let once = Task::new("/p".into(), "a".into(), "b".into());
        assert!(TaskStore::is_auto_runnable(&once, &[], now));

        let mut manual = Task::new("/p".into(), "a".into(), "b".into());
        manual.auto_run = false;
        assert!(!TaskStore::is_auto_runnable(&manual, &[], now));

        let mut sched = Task::new("/p".into(), "a".into(), "b".into());
        sched.kind = TaskKind::Scheduled;
        sched.run_at = Some(2_000);
        assert!(!TaskStore::is_auto_runnable(&sched, &[], now));
        sched.run_at = Some(500);
        assert!(TaskStore::is_auto_runnable(&sched, &[], now));
        sched.auto_run = false;
        assert!(!TaskStore::is_auto_runnable(&sched, &[], now));
    }

    #[test]
    fn auto_claim_projects_are_fifo_and_unique() {
        let now = 1_000u64;
        let mut first = Task::new("/first/".into(), "first".into(), "b".into());
        first.created_at = 10;
        let mut duplicate = Task::new("/first".into(), "duplicate".into(), "b".into());
        duplicate.created_at = 20;
        let mut second = Task::new("/second".into(), "second".into(), "b".into());
        second.created_at = 30;
        let mut manual = Task::new("/ignored".into(), "manual".into(), "b".into());
        manual.created_at = 1;
        manual.auto_run = false;
        let mut future = Task::new("/future".into(), "future".into(), "b".into());
        future.created_at = 2;
        future.kind = TaskKind::Scheduled;
        future.run_at = Some(now + 1);

        let projects = auto_claim_cwds_from_tasks(
            &[second, manual, duplicate, future, first],
            now,
        );

        assert_eq!(projects, vec!["/first", "/second"]);
    }

    #[test]
    fn claim_next_is_fifo_same_cwd_auto_only() {
        let mut a = Task::new("/proj".into(), "1".into(), "b1".into());
        a.created_at = 10;
        let mut b = Task::new("/proj".into(), "2".into(), "b2".into());
        b.created_at = 5;
        b.auto_run = false; // 更早但不自动 → 跳过
        let mut c = Task::new("/proj".into(), "3".into(), "b3".into());
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
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
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
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into());
        dep.column = TaskColumn::Running;
        let t = Task::new("/p".into(), "a".into(), "b".into());
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
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into());
        dep.column = TaskColumn::Backlog; // 未完成
        dep.auto_run = false; // 它只是阻塞条件，自身不参与候选
        let mut a = Task::new("/p".into(), "1".into(), "b1".into());
        a.created_at = 10;
        a.depends_on = vec![dep.id.clone()];
        let mut b = Task::new("/p".into(), "2".into(), "b2".into());
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
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into());
        dep.column = TaskColumn::Done;
        let t = Task::new("/p".into(), "a".into(), "b".into());
        let mut with_dep = t.clone();
        with_dep.depends_on = vec![dep.id.clone()];
        let tasks = vec![dep, with_dep.clone()];
        assert!(with_dep.dependencies_met(&tasks));
    }

    #[test]
    fn dependency_pending_or_running_blocks() {
        let mut dep = Task::new("/p".into(), "dep".into(), "b".into());
        dep.column = TaskColumn::Backlog;
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
        t.depends_on = vec![dep.id.clone()];
        let tasks = vec![dep.clone(), t.clone()];
        assert!(!t.dependencies_met(&tasks));
        dep.column = TaskColumn::Running;
        let tasks = vec![dep, t.clone()];
        assert!(!t.dependencies_met(&tasks));
    }

    #[test]
    fn dependency_deleted_treated_met() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
        t.depends_on = vec!["已删除的id".into()];
        let tasks = vec![t.clone()];
        assert!(t.dependencies_met(&tasks));
    }

    #[test]
    fn self_dependency_met() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
        t.depends_on = vec![t.id.clone()];
        assert!(t.dependencies_met(&[t.clone()]));
    }

    #[test]
    fn apply_failure_retries_within_limit() {
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
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
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
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
        let mut t = Task::new("/p".into(), "a".into(), "b".into());
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
        );
        assert_eq!(super::task_prompt(&t), "真正给 agent 的指令");
    }

    #[test]
    fn task_prompt_falls_back_to_title_when_body_empty() {
        let t = Task::new("/x".into(), "only title".into(), String::new());
        assert_eq!(super::task_prompt(&t), "only title");
    }

    #[test]
    fn title_from_prompt_takes_first_line() {
        assert_eq!(super::title_from_prompt("第一行\n第二行"), "第一行");
    }

    #[test]
    fn task_card_preview_omits_duplicated_title() {
        assert_eq!(super::task_card_body_preview("同一任务", "同一任务"), "");
        assert_eq!(
            super::task_card_body_preview("同一任务", "同一任务\n补充说明"),
            "补充说明"
        );
        assert_eq!(
            super::task_card_body_preview("手工标题", "完整任务说明"),
            "完整任务说明"
        );

        let long = "a".repeat(41);
        assert_eq!(
            super::task_card_body_preview(&super::title_from_prompt(&long), &long),
            ""
        );
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
