//! 会话列表（S1）：视图 [`crate::contract::LIST`]，按机器、项目分组列出会话，「对话」一段
//! 放跟智能体的对话。旧宿主的 `session_list/*`、`sidebar_order.rs`、`conversations.rs` 的
//! 侧栏部分搬到这里（本目录下的文件都归 S1）。
//!
//! - 本文件：侧栏算好的样子（[`Sidebar`]），和排序用的纯函数。只在事件里算（
//!   `Sessions::rebuild_sidebar`），render 只读它。
//! - [`group`]：按状态、时间分组。
//! - [`machine`]：机器行的系统图标和连接指示。
//! - [`view`]：列表视图，行内改名、拖拽、从访达拖进目录的界面状态。
//! - [`rows`]：各种行怎么画。
//! - [`menus`]：机器、项目、工作集、会话的菜单，「筛选与分组」菜单，和它们的动作。
//!
//! 项目里的会话按项目（默认）、状态或时间分组；按项目时可以藏起没有会话的项目。只挂着本机
//! 时机器不分段，最上面是「项目」标题；挂了远程机器按机器分段，机器名就是段标题。没有键盘
//! 上下移动、字母跳转、分段拖动高度、Cmd+1..9、归档区、额度、设置按钮（按 11-16 的决定删了
//! 或归别的插件）。

mod actions;
mod group;
mod machine;
mod menus;
mod rows;
mod view;

use std::collections::HashMap;

use chrono::{DateTime, Local};
use gpui::{App, SharedString};
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{WorkspaceConversation, WorkspaceDirectory};
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::model::{
    self, ConversationKey, Fold, Pin, Placement, Sessions, SidebarGrouping, SidebarUi,
};
use crate::worksets::{MergeSummary, WorksetCheck, WorksetKey};

pub(crate) use group::Bucket;
pub(crate) use view::build_view;

use machine::MachineLink;

/// 侧栏算好的样子：从目录、会话状态、机器表和 [`Sessions::ui`] 现算的行。存在
/// [`Sessions::sidebar`]，render 只读它；`Sessions::rebuild_sidebar` 重算。选中哪行不在
/// 这里（选中变了不重算），画的时候对着 `Sessions::selection` 看。
#[derive(Default)]
pub(crate) struct Sidebar {
    /// 「对话」一段；一段跟智能体的对话都没有时是 `None`，不画这一段。
    conversations: Option<ConversationsSection>,
    machines: Vec<MachineSection>,
    order: Vec<ConversationKey>,
    /// 项目里的会话按什么分组。只有按项目时会话、项目能拖着换位置（别的分法顺序是算出来的）。
    grouping: SidebarGrouping,
    /// 按项目分组时藏起没有会话的项目（[`project_visible`]）。
    hide_empty: bool,
    /// 挂了远程机器：按机器分段，每台机器一个标题；只有本机时最上面是「项目」标题。
    segmented: bool,
}

/// 「对话」一段：所有机器上跟智能体的对话，置顶的在前，其余按最近动静排。
struct ConversationsSection {
    collapsed: bool,
    /// 折起来时标在标题上的状态。
    status: AgentStatus,
    rows: Vec<SessionRow>,
}

/// 一台机器：标题行和它下面的项目。
struct MachineSection {
    key: RuntimeKey,
    name: SharedString,
    os_icon: SharedString,
    link: MachineLink,
    /// 工作区目录收到了没有。没收到（还没连上过）不说「没有项目」。
    received: bool,
    /// 分段时折起来了（只画标题行）。只有本机、不分段时不折。
    collapsed: bool,
    projects: Vec<ProjectGroup>,
    /// 这台机器上项目从上到下的根目录（拖项目时整份写回 `project_order`）。
    roots: Vec<String>,
    /// 这台机器上项目里的会话从上到下（拖会话、置顶时整份写回 `conversation_order`）。
    conversation_ids: Vec<String>,
    /// 按状态、时间分组时这台机器上的组；按项目时空着，画 [`Self::projects`]。
    buckets: Vec<BucketGroup>,
}

/// 按状态或时间分出来的一组：项目里的会话打散了重新分，每行带上它来自哪个项目。
struct BucketGroup {
    bucket: Bucket,
    collapsed: bool,
    /// 折起来时标在标题上的状态。
    status: AgentStatus,
    rows: Vec<SessionRow>,
}

/// 一个项目组：打开的项目，或者不在任何项目里的会话按工作目录自成的一组。
struct ProjectGroup {
    machine: RuntimeKey,
    root: String,
    name: SharedString,
    /// 是打开的项目（目录的 `projects` 里有）；不是的话关掉它只是关掉下面的会话。
    opened: bool,
    pinned: bool,
    collapsed: bool,
    status: AgentStatus,
    /// 项目根上的分支（从会话状态里工作目录就是根的那格读），不是 git 仓库是 `None`。
    branch: Option<SharedString>,
    /// 纳入项目的仓库数。
    repositories: usize,
    /// 下面的会话数，含工作集里的。
    sessions: usize,
    items: Vec<ProjectItem>,
}

enum ProjectItem {
    Workset(WorksetGroup),
    Session(SessionRow),
}

/// 项目里的一个工作集（多仓库工作树）。
struct WorksetGroup {
    machine: RuntimeKey,
    id: String,
    task: SharedString,
    /// 工作树的位置（新建会话时预选它）。
    path: String,
    tooltip: SharedString,
    pinned: bool,
    collapsed: bool,
    status: AgentStatus,
    sessions: Vec<SessionRow>,
    /// 各仓库：名字和 worktree 目录（「创建 PR」按仓库开）。
    repositories: Vec<(String, String)>,
    /// 上一次问到的合并情况；正在问时为真。
    merge: MergeSummary,
    checking: bool,
}

/// 一行会话。
#[derive(Clone)]
struct SessionRow {
    key: ConversationKey,
    /// 领头那一格的守护会话 id（对话的，或第一格终端的），「复制会话 ID」用。
    session_id: Option<String>,
    title: SharedString,
    icon: RowIcon,
    status: AgentStatus,
    pinned: bool,
    /// 最近一次动静（unix 秒）；守护里还没有它时用目录里的更新时间。
    activity: u64,
    /// 在跑的这一轮从什么时候开始（unix 秒）；不在一轮里是 0。
    turn_started_at: u64,
    /// 机器眼下用不了时行尾写的字。
    unavailable: Option<&'static str>,
    /// 悬停的说明：完整标题（窄侧栏里常被截断）；「对话」一段里远程机器的会话再带上机器名。
    tooltip: SharedString,
    /// 在哪一组里（拖会话只能在同一组里换位置）。
    group: GroupKey,
    /// 按状态、时间分组时行上补的来源：项目标题不在了，项目名、分支、工作树写到行里。按项目
    /// 分组、「对话」一段里是 `None`。
    origin: Option<RowOrigin>,
}

/// 会话来自哪个项目（按状态、时间分组时画在行里）。
#[derive(Clone)]
struct RowOrigin {
    project: SharedString,
    branch: Option<SharedString>,
    /// 在工作树里的：工作树的名字和悬停说明。
    workset: Option<(SharedString, SharedString)>,
}

/// 会话所在的组。
#[derive(Clone, Debug, PartialEq, Eq)]
enum GroupKey {
    Conversations,
    Project {
        machine: RuntimeKey,
        root: String,
        workset: Option<String>,
    },
}

/// 行首的图标。
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowIcon {
    /// 跟智能体的对话：名字的第一个字，悬停看名字。
    Agent {
        mark: SharedString,
        name: SharedString,
    },
    /// 哪家 CLI / ACP 的单色图标（资源路径），悬停看是哪家。
    Provider {
        path: SharedString,
        label: SharedString,
    },
    /// 认不出是哪家的终端。
    Terminal,
    /// 认不出是哪家的对话。
    Conversation,
}

impl Sidebar {
    /// 按眼下的数据算一遍。只读内存（目录、会话状态都是宿主的内存缓存），不碰库。
    pub(crate) fn build(sessions: &Sessions, cx: &App) -> Self {
        let host = api::host(cx);
        let connections = host.connections(cx);
        let mut agent_rows = Vec::new();
        let mut machines = Vec::new();
        let no_agents = HashMap::new();
        // 按时间分组的日界按这一刻算；过了零点要等下一次重算（目录、状态一变就重算）才挪组。
        let now = Local::now();
        for machine in host.machines(cx) {
            let link = machine::link(
                &machine,
                connections
                    .remotes
                    .iter()
                    .find(|connection| connection.machine == machine.key),
            );
            let os_icon = machine::os_icon(&machine.key, &connections).into();
            let directory = host.workspace_directory(&machine.key, cx);
            let mut section = MachineSection {
                key: machine.key.clone(),
                name: machine.name.clone().into(),
                os_icon,
                link,
                received: directory.is_some(),
                collapsed: false,
                projects: Vec::new(),
                roots: Vec::new(),
                conversation_ids: Vec::new(),
                buckets: Vec::new(),
            };
            if let Some(directory) = directory {
                let input = MachineInput {
                    machine: &machine.key,
                    style: host.path_style(&machine.key, cx),
                    directory: &directory,
                    states: &host.session_states(&machine.key, cx),
                    agents: sessions.agent_names.get(&machine.key).unwrap_or(&no_agents),
                    unavailable: section.link.unavailable,
                    remote_name: (!matches!(machine.key, RuntimeKey::Local))
                        .then(|| machine.name.clone()),
                    checks: &sessions.workset_checks,
                };
                fill_machine(&mut section, &input, &sessions.ui, &mut agent_rows);
                section.buckets = bucket_groups(&section.key, &section.projects, &sessions.ui, now);
            }
            machines.push(section);
        }
        let conversations = (!agent_rows.is_empty()).then(|| {
            agent_rows.sort_by(|a: &SessionRow, b| b.activity.cmp(&a.activity));
            let rows = pinned_first(agent_rows, |row| row.pinned);
            ConversationsSection {
                collapsed: sessions.ui.is_collapsed(&Fold::Conversations),
                status: AgentStatus::highest(rows.iter().map(|row| row.status)),
                rows,
            }
        });
        let segmented = machines
            .iter()
            .any(|machine| !matches!(machine.key, RuntimeKey::Local));
        if segmented {
            for machine in &mut machines {
                machine.collapsed = sessions.ui.collapsed_machines.contains(&machine.key);
            }
        }
        let mut sidebar = Self {
            conversations,
            machines,
            order: Vec::new(),
            grouping: sessions.ui.grouping,
            hide_empty: sessions.ui.hide_empty_projects,
            segmented,
        };
        sidebar.order = sidebar.visible_order();
        sidebar
    }

    /// 列表上看得见的会话，从上到下（折起来的不算）。上一段 / 下一段按它走。
    pub(crate) fn order(&self) -> &[ConversationKey] {
        &self.order
    }

    fn visible_order(&self) -> Vec<ConversationKey> {
        let mut order = Vec::new();
        if let Some(section) = self.conversations.as_ref().filter(|s| !s.collapsed) {
            order.extend(section.rows.iter().map(|row| row.key.clone()));
        }
        for machine in self.machines.iter().filter(|m| !m.collapsed) {
            if self.grouping != SidebarGrouping::Project {
                for bucket in machine.buckets.iter().filter(|b| !b.collapsed) {
                    order.extend(bucket.rows.iter().map(|row| row.key.clone()));
                }
                continue;
            }
            // 藏起来的空项目里没有会话，不用另外跳过。
            for project in machine.projects.iter().filter(|p| !p.collapsed) {
                for item in &project.items {
                    match item {
                        ProjectItem::Session(row) => order.push(row.key.clone()),
                        ProjectItem::Workset(workset) if !workset.collapsed => {
                            order.extend(workset.sessions.iter().map(|row| row.key.clone()))
                        }
                        ProjectItem::Workset(_) => {}
                    }
                }
            }
        }
        order
    }

    fn machine(&self, key: &RuntimeKey) -> Option<&MachineSection> {
        self.machines.iter().find(|machine| machine.key == *key)
    }

    /// 那一组里的会话，从上到下。
    fn group_rows(&self, group: &GroupKey) -> Vec<&SessionRow> {
        match group {
            GroupKey::Conversations => self
                .conversations
                .iter()
                .flat_map(|section| section.rows.iter())
                .collect(),
            GroupKey::Project {
                machine,
                root,
                workset,
            } => {
                let Some(project) = self
                    .machine(machine)
                    .and_then(|m| m.projects.iter().find(|p| p.root == *root))
                else {
                    return Vec::new();
                };
                project
                    .items
                    .iter()
                    .flat_map(|item| match (item, workset) {
                        (ProjectItem::Session(row), None) => vec![row],
                        (ProjectItem::Workset(w), Some(id)) if w.id == *id => {
                            w.sessions.iter().collect()
                        }
                        _ => Vec::new(),
                    })
                    .collect()
            }
        }
    }
}

/// 算一台机器要用的输入。
struct MachineInput<'a> {
    machine: &'a RuntimeKey,
    style: PathStyle,
    directory: &'a WorkspaceDirectory,
    states: &'a [DaemonSessionState],
    /// 智能体定义 id → 名字。
    agents: &'a HashMap<String, String>,
    unavailable: Option<&'static str>,
    /// 远程机器的名字（「对话」一段里的会话要标出是哪台机器的）。
    remote_name: Option<String>,
    /// 各棵工作树上一次问到的合并情况。
    checks: &'a HashMap<WorksetKey, WorksetCheck>,
}

/// 把一台机器的目录摆成项目组；跟智能体的对话放进 `agent_rows`（「对话」一段）。
fn fill_machine(
    section: &mut MachineSection,
    input: &MachineInput,
    ui: &SidebarUi,
    agent_rows: &mut Vec<SessionRow>,
) {
    let machine_ui = ui.machine(input.machine);
    let empty = Vec::new();
    let manual_sessions = machine_ui.map_or(&empty, |ui| &ui.conversation_order);
    let manual_projects = machine_ui.map_or(&empty, |ui| &ui.project_order);

    // 项目：打开的项目按目录的顺序，再接上只靠会话工作目录撑起来的组。
    let mut roots: Vec<String> = input.directory.projects.clone();
    let mut members: HashMap<String, Vec<(&WorkspaceConversation, Option<String>)>> =
        HashMap::new();
    for conversation in &input.directory.conversations {
        match model::placement(input.style, input.directory, conversation) {
            Placement::Hidden => {}
            Placement::Conversations => {
                let mut row = session_row(conversation, input, ui, GroupKey::Conversations);
                if let Some(name) = &input.remote_name {
                    row.tooltip = format!("{} · {name}", row.title).into();
                }
                agent_rows.push(row);
            }
            Placement::Project { root, workset } => {
                if !roots.contains(&root) {
                    roots.push(root.clone());
                }
                members
                    .entry(root)
                    .or_default()
                    .push((conversation, workset));
            }
        }
    }
    let roots = manual_first(roots, manual_projects, |root| root.as_str());
    let roots = pinned_first(roots, |root| {
        ui.is_pinned(&Pin::Project {
            machine: input.machine.clone(),
            root: root.clone(),
        })
    });

    for root in roots {
        let members = members.remove(&root).unwrap_or_default();
        let group = project_group(&root, members, manual_sessions, input, ui);
        section
            .conversation_ids
            .extend(group.items.iter().flat_map(|item| match item {
                ProjectItem::Session(row) => vec![row.key.id.clone()],
                ProjectItem::Workset(w) => {
                    w.sessions.iter().map(|row| row.key.id.clone()).collect()
                }
            }));
        section.roots.push(root);
        section.projects.push(group);
    }
}

fn project_group(
    root: &str,
    members: Vec<(&WorkspaceConversation, Option<String>)>,
    manual_sessions: &[String],
    input: &MachineInput,
    ui: &SidebarUi,
) -> ProjectGroup {
    let machine = input.machine;
    let members = manual_first(members, manual_sessions, |(c, _)| c.id.as_str());
    let rows_of = |workset: Option<&str>| -> Vec<SessionRow> {
        let rows: Vec<SessionRow> = members
            .iter()
            .filter(|(_, w)| w.as_deref() == workset)
            .map(|(conversation, _)| {
                session_row(
                    conversation,
                    input,
                    ui,
                    GroupKey::Project {
                        machine: machine.clone(),
                        root: root.to_string(),
                        workset: workset.map(str::to_string),
                    },
                )
            })
            .collect();
        pinned_first(rows, |row: &SessionRow| row.pinned)
    };

    // 工作集：置顶的在普通会话前面，其余在后面；各自按目录里的位置。
    let mut worksets: Vec<_> = input
        .directory
        .worksets
        .iter()
        .filter(|workset| workset.project_root == root)
        .collect();
    worksets.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    let workset_groups: Vec<WorksetGroup> = worksets
        .into_iter()
        .map(|workset| {
            let sessions = rows_of(Some(&workset.id));
            let repositories: Vec<&str> = workset
                .repositories
                .iter()
                .map(|repository| repository.name.as_str())
                .collect();
            let check = input.checks.get(&WorksetKey {
                machine: machine.clone(),
                id: workset.id.clone(),
            });
            let merge_lines = match check.and_then(|check| check.last.as_ref()) {
                Some((_, Ok(result))) => crate::worksets::describe(result),
                Some((_, Err(error))) => vec![format!("检查合并状态失败：{error}")],
                None => Vec::new(),
            };
            let mut tooltip = format!(
                "工作树：{}\n分支：{}\n仓库：{}",
                workset.task,
                workset.branch,
                repositories.join("、")
            );
            for line in &merge_lines {
                tooltip.push('\n');
                tooltip.push_str(line);
            }
            WorksetGroup {
                machine: machine.clone(),
                id: workset.id.clone(),
                task: workset.task.clone().into(),
                path: workset.task_root(input.style),
                tooltip: tooltip.into(),
                pinned: ui.is_pinned(&Pin::Workset {
                    machine: machine.clone(),
                    id: workset.id.clone(),
                }),
                collapsed: ui.is_collapsed(&Fold::Workset {
                    machine: machine.clone(),
                    id: workset.id.clone(),
                }),
                status: AgentStatus::highest(sessions.iter().map(|row| row.status)),
                sessions,
                repositories: workset
                    .repositories
                    .iter()
                    .map(|repository| (repository.name.clone(), repository.worktree.clone()))
                    .collect(),
                merge: MergeSummary::of(check),
                checking: check.is_some_and(|check| check.running),
            }
        })
        .collect();
    let (pinned_worksets, other_worksets): (Vec<_>, Vec<_>) =
        workset_groups.into_iter().partition(|w| w.pinned);
    let mut items: Vec<ProjectItem> = pinned_worksets
        .into_iter()
        .map(ProjectItem::Workset)
        .collect();
    items.extend(rows_of(None).into_iter().map(ProjectItem::Session));
    items.extend(other_worksets.into_iter().map(ProjectItem::Workset));

    let statuses = items.iter().flat_map(|item| match item {
        ProjectItem::Session(row) => vec![row.status],
        ProjectItem::Workset(w) => w.sessions.iter().map(|row| row.status).collect(),
    });
    let status = AgentStatus::highest(statuses);
    let clean_root = input.style.clean(root);
    let branch = (!root.is_empty())
        .then(|| {
            input.states.iter().find_map(|state| {
                let cwd = state.cwd.as_deref()?;
                (input.style.clean(cwd) == clean_root)
                    .then(|| state.branch.clone())
                    .flatten()
            })
        })
        .flatten()
        .filter(|branch| !branch.is_empty())
        .map(SharedString::from);
    ProjectGroup {
        machine: machine.clone(),
        root: root.to_string(),
        name: project_name(input.style, root).into(),
        opened: input.directory.projects.iter().any(|p| p == root),
        pinned: ui.is_pinned(&Pin::Project {
            machine: machine.clone(),
            root: root.to_string(),
        }),
        collapsed: ui.is_collapsed(&Fold::Project {
            machine: machine.clone(),
            root: root.to_string(),
        }),
        status,
        branch,
        repositories: input
            .directory
            .repositories
            .iter()
            .filter(|repository| repository.project_root == root)
            .count(),
        sessions: members.len(),
        items,
    }
}

fn session_row(
    conversation: &WorkspaceConversation,
    input: &MachineInput,
    ui: &SidebarUi,
    group: GroupKey,
) -> SessionRow {
    let key = ConversationKey {
        machine: input.machine.clone(),
        id: conversation.id.clone(),
    };
    let states = input.states;
    // 状态推送里没有它（守护重启后还没跑过、离线）时用守护记下的最近一轮结束时间。
    let activity = match model::last_activity(conversation, states) {
        0 => conversation.last_updated_at.max(0) as u64,
        at => at,
    };
    let status = model::status(conversation, states);
    // 只有在跑时行尾才写「这一轮跑了多久」；停下来的格里留着的开始时间不算。
    let turn_started_at = if status == AgentStatus::Running {
        model::session_ids(conversation)
            .filter_map(|id| states.iter().find(|state| state.id == id))
            .map(|state| state.turn_started_at)
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let title: SharedString = model::title(conversation, states).into();
    SessionRow {
        pinned: ui.is_pinned(&Pin::Conversation(key.clone())),
        session_id: model::session_ids(conversation).next().map(str::to_string),
        key,
        tooltip: title.clone(),
        title,
        icon: row_icon(conversation, states, input.agents),
        status,
        activity,
        turn_started_at,
        unavailable: input.unavailable,
        group,
        origin: None,
    }
}

/// 按状态、时间分组时这台机器上的组：项目里的会话（含工作树里的）按项目从上到下摊平，带上
/// 来源，再分组；组里置顶的在前。按项目分组时是空的。
fn bucket_groups(
    machine: &RuntimeKey,
    projects: &[ProjectGroup],
    ui: &SidebarUi,
    now: DateTime<Local>,
) -> Vec<BucketGroup> {
    if ui.grouping == SidebarGrouping::Project {
        return Vec::new();
    }
    let mut rows: Vec<SessionRow> = Vec::new();
    for project in projects {
        let origin = |workset: Option<&WorksetGroup>| RowOrigin {
            project: project.name.clone(),
            branch: project.branch.clone(),
            workset: workset.map(|w| (w.task.clone(), w.tooltip.clone())),
        };
        for item in &project.items {
            let (members, workset) = match item {
                ProjectItem::Session(row) => (std::slice::from_ref(row), None),
                ProjectItem::Workset(w) => (w.sessions.as_slice(), Some(w)),
            };
            rows.extend(members.iter().map(|row| {
                let mut row = row.clone();
                row.origin = Some(origin(workset));
                row
            }));
        }
    }
    let keys: Vec<(AgentStatus, u64)> = rows.iter().map(|row| (row.status, row.activity)).collect();
    let mut slots: Vec<Option<SessionRow>> = rows.into_iter().map(Some).collect();
    group::buckets(ui.grouping, &keys, now)
        .into_iter()
        .map(|(bucket, ixs)| {
            let members: Vec<SessionRow> =
                ixs.into_iter().filter_map(|ix| slots[ix].take()).collect();
            let members = pinned_first(members, |row| row.pinned);
            BucketGroup {
                bucket,
                collapsed: ui.collapsed_buckets.contains(&(machine.clone(), bucket)),
                status: AgentStatus::highest(members.iter().map(|row| row.status)),
                rows: members,
            }
        })
        .collect()
}

/// 按项目分组、「显示：有会话」时这个项目露不露出来：有会话或工作树的、置顶的、选中的照常
/// 显示。选中不在 [`Sidebar`] 里（选中变了不重算），所以画的时候判断。
fn project_visible(hide_empty: bool, has_items: bool, pinned: bool, selected: bool) -> bool {
    !hide_empty || has_items || pinned || selected
}

fn row_icon(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
    agents: &HashMap<String, String>,
) -> RowIcon {
    if let Some(id) = model::agent_of(conversation) {
        let name = agents
            .get(id)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| id.to_string());
        return RowIcon::Agent {
            mark: agent_mark(&name).into(),
            name: name.into(),
        };
    }
    if let Some(acp) = &conversation.acp {
        return ConversationAgentKind::from_id(&acp.agent)
            .map(|kind| RowIcon::Provider {
                path: kind.icon_asset().into(),
                label: kind.label().into(),
            })
            .unwrap_or(RowIcon::Conversation);
    }
    let first = conversation.terminals.first();
    let provider = first
        .and_then(|terminal| terminal.id.as_deref())
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.provider.as_deref())
        .and_then(TerminalAgentKind::from_id)
        .or_else(|| {
            first
                .and_then(|terminal| terminal.launch_cmd.as_deref())
                .and_then(TerminalAgentKind::from_command_prefix)
        });
    provider
        .map(|kind| RowIcon::Provider {
            path: kind.icon_asset().into(),
            label: kind.label().into(),
        })
        .unwrap_or(RowIcon::Terminal)
}

/// 智能体头像上的字：名字的第一个字。
fn agent_mark(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(String::from)
        .unwrap_or_else(|| "智".to_string())
}

/// 项目组的名字：根目录的最后一段；没有工作目录的那组叫「未指定目录」。
fn project_name(style: PathStyle, root: &str) -> String {
    if root.is_empty() {
        return "未指定目录".to_string();
    }
    style
        .file_name(root)
        .filter(|name| !name.is_empty())
        .unwrap_or(root)
        .to_string()
}

// ---- 排序 ----

/// 手动排过的在前（按手动顺序），其余按原来的顺序接在后面。
fn manual_first<T>(items: Vec<T>, manual: &[String], key: impl Fn(&T) -> &str) -> Vec<T> {
    let mut ranked: Vec<(usize, usize, T)> = items
        .into_iter()
        .enumerate()
        .map(|(ix, item)| {
            let rank = manual
                .iter()
                .position(|m| m == key(&item))
                .unwrap_or(usize::MAX);
            (rank, ix, item)
        })
        .collect();
    ranked.sort_by_key(|(rank, ix, _)| (*rank, *ix));
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

/// 置顶的提到前面，两边各自的相对顺序不变。
fn pinned_first<T>(items: Vec<T>, pinned: impl Fn(&T) -> bool) -> Vec<T> {
    let (mut head, tail): (Vec<T>, Vec<T>) = items.into_iter().partition(|item| pinned(item));
    head.extend(tail);
    head
}

/// 把 `from` 挪到 `to` 的前面或后面。没动返回 `false`。
fn move_near(items: &mut Vec<String>, from: &str, to: &str, before: bool) -> bool {
    if from == to {
        return false;
    }
    let Some(from_ix) = items.iter().position(|item| item == from) else {
        return false;
    };
    let item = items.remove(from_ix);
    let Some(to_ix) = items.iter().position(|item| item == to) else {
        items.insert(from_ix, item);
        return false;
    };
    let at = if before { to_ix } else { to_ix + 1 };
    items.insert(at, item);
    at != from_ix
}

/// 在整份顺序 `all` 里，把 `group` 那几项按 `group` 的新顺序换进它们原来占的位置。不在
/// `all` 里的接在最后。
fn splice_group(all: &[String], group: &[String]) -> Vec<String> {
    let mut next = group.iter().filter(|id| all.contains(id));
    let mut out: Vec<String> = all
        .iter()
        .map(|id| {
            if group.contains(id) {
                next.next().cloned().unwrap_or_else(|| id.clone())
            } else {
                id.clone()
            }
        })
        .collect();
    out.extend(group.iter().filter(|id| !all.contains(id)).cloned());
    out
}

/// 置顶状态改了之后，把 `item` 挪到组里置顶区和其余之间的交界：新置顶的排在置顶区最后，
/// 取消置顶的排在其余的最前。不挪的话它会落在按原顺序算出的位置，点完要满屏找。
/// `order` 是组里眼下从上到下的顺序，`pinned` 是改过之后的置顶判断。
fn move_to_pin_boundary(
    order: &[String],
    item: &str,
    pinned: impl Fn(&str) -> bool,
) -> Vec<String> {
    let rest: Vec<&String> = order.iter().filter(|id| *id != item).collect();
    let (head, tail): (Vec<&String>, Vec<&String>) = rest.into_iter().partition(|id| pinned(id));
    head.into_iter()
        .cloned()
        .chain(std::iter::once(item.to_string()))
        .chain(tail.into_iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manual_order_goes_first_and_the_rest_keeps_its_order() {
        let items = strings(&["a", "b", "c", "d"]);
        let manual = strings(&["c", "x", "a"]);
        assert_eq!(
            manual_first(items, &manual, |s| s.as_str()),
            strings(&["c", "a", "b", "d"])
        );
    }

    #[test]
    fn pinned_first_is_stable() {
        let items = strings(&["a", "b", "c", "d"]);
        assert_eq!(
            pinned_first(items, |s| s == "c" || s == "a"),
            strings(&["a", "c", "b", "d"])
        );
    }

    #[test]
    fn move_near_before_and_after() {
        let mut items = strings(&["a", "b", "c", "d"]);
        assert!(move_near(&mut items, "d", "b", true));
        assert_eq!(items, strings(&["a", "d", "b", "c"]));
        assert!(move_near(&mut items, "a", "c", false));
        assert_eq!(items, strings(&["d", "b", "c", "a"]));
        assert!(!move_near(&mut items, "b", "c", true), "已经在那儿了");
        assert!(!move_near(&mut items, "b", "b", true));
        assert!(!move_near(&mut items, "z", "b", true));
        assert_eq!(items, strings(&["d", "b", "c", "a"]));
    }

    #[test]
    fn splice_group_keeps_other_groups_in_place() {
        let all = strings(&["a1", "b1", "a2", "b2", "a3"]);
        let group = strings(&["a3", "a1", "a2"]);
        assert_eq!(
            splice_group(&all, &group),
            strings(&["a3", "b1", "a1", "b2", "a2"])
        );
    }

    #[test]
    fn pinning_lands_on_the_boundary() {
        let order = strings(&["p1", "p2", "a", "b", "c"]);
        // 置顶 c：排在 p1、p2 之后。
        let pinned = |id: &str| matches!(id, "p1" | "p2" | "c");
        assert_eq!(
            move_to_pin_boundary(&order, "c", pinned),
            strings(&["p1", "p2", "c", "a", "b"])
        );
        // 取消置顶 p1：排在其余的最前。
        let pinned = |id: &str| id == "p2";
        assert_eq!(
            move_to_pin_boundary(&order, "p1", pinned),
            strings(&["p2", "p1", "a", "b", "c"])
        );
    }

    fn row(id: &str, status: AgentStatus, pinned: bool, workset: Option<&str>) -> SessionRow {
        SessionRow {
            key: ConversationKey {
                machine: RuntimeKey::Local,
                id: id.to_string(),
            },
            session_id: None,
            title: id.to_string().into(),
            icon: RowIcon::Terminal,
            status,
            pinned,
            activity: 0,
            turn_started_at: 0,
            unavailable: None,
            tooltip: id.to_string().into(),
            group: GroupKey::Project {
                machine: RuntimeKey::Local,
                root: "/p".to_string(),
                workset: workset.map(str::to_string),
            },
            origin: None,
        }
    }

    fn project(name: &str, branch: Option<&str>, items: Vec<ProjectItem>) -> ProjectGroup {
        ProjectGroup {
            machine: RuntimeKey::Local,
            root: format!("/{name}"),
            name: name.to_string().into(),
            opened: true,
            pinned: false,
            collapsed: false,
            status: AgentStatus::Idle,
            branch: branch.map(|b| b.to_string().into()),
            repositories: 0,
            sessions: 0,
            items,
        }
    }

    fn workset(task: &str, sessions: Vec<SessionRow>) -> ProjectItem {
        ProjectItem::Workset(WorksetGroup {
            machine: RuntimeKey::Local,
            id: task.to_string(),
            task: task.to_string().into(),
            path: String::new(),
            tooltip: format!("工作树：{task}").into(),
            pinned: false,
            collapsed: false,
            status: AgentStatus::Idle,
            sessions,
            repositories: Vec::new(),
            merge: MergeSummary::Unknown,
            checking: false,
        })
    }

    fn ids(rows: &[SessionRow]) -> Vec<&str> {
        rows.iter().map(|row| row.key.id.as_str()).collect()
    }

    /// 两个项目：a（分支 main）里一段会话加一个工作树，b 里一段置顶的、一段要人管的。
    fn two_projects() -> Vec<ProjectGroup> {
        use AgentStatus::*;
        vec![
            project(
                "a",
                Some("main"),
                vec![
                    ProjectItem::Session(row("r1", Idle, false, None)),
                    workset("修复", vec![row("r2", NeedsYou, false, Some("修复"))]),
                ],
            ),
            project(
                "b",
                None,
                vec![
                    ProjectItem::Session(row("r3", Idle, true, None)),
                    ProjectItem::Session(row("r4", NeedsYou, false, None)),
                ],
            ),
        ]
    }

    #[test]
    fn status_grouping_takes_every_project_row_with_its_origin() {
        let mut ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        ui.collapsed_buckets
            .insert((RuntimeKey::Local, Bucket::Idle));
        let groups = bucket_groups(&RuntimeKey::Local, &two_projects(), &ui, Local::now());

        let buckets: Vec<Bucket> = groups.iter().map(|g| g.bucket).collect();
        assert_eq!(buckets, [Bucket::NeedsYou, Bucket::Idle]);
        assert_eq!(ids(&groups[0].rows), ["r2", "r4"]);
        // 组里置顶的在前。
        assert_eq!(ids(&groups[1].rows), ["r3", "r1"]);
        assert_eq!(groups[0].status, AgentStatus::NeedsYou);
        assert!(!groups[0].collapsed);
        assert!(groups[1].collapsed);

        let origin = groups[0].rows[0].origin.as_ref().expect("带来源");
        assert_eq!(origin.project, "a");
        assert_eq!(origin.branch.as_ref().map(|b| b.as_ref()), Some("main"));
        assert_eq!(
            origin.workset.as_ref().map(|(task, _)| task.as_ref()),
            Some("修复")
        );
        let origin = groups[0].rows[1].origin.as_ref().expect("带来源");
        assert_eq!(origin.project, "b");
        assert!(origin.branch.is_none() && origin.workset.is_none());
        // 分组不改会话属于哪个项目（置顶照常按项目里的顺序挪）。
        assert!(matches!(
            &groups[0].rows[0].group,
            GroupKey::Project { workset: Some(w), .. } if w == "修复"
        ));
    }

    #[test]
    fn project_grouping_keeps_project_order() {
        let ui = SidebarUi::default();
        let projects = two_projects();
        assert!(bucket_groups(&RuntimeKey::Local, &projects, &ui, Local::now()).is_empty());

        let sidebar = |grouping, buckets| Sidebar {
            conversations: None,
            machines: vec![MachineSection {
                key: RuntimeKey::Local,
                name: "本机".into(),
                os_icon: SharedString::default(),
                link: MachineLink {
                    light: machine::Light::Good,
                    detail: String::new(),
                    unavailable: None,
                },
                received: true,
                collapsed: false,
                projects: two_projects(),
                roots: Vec::new(),
                conversation_ids: Vec::new(),
                buckets,
            }],
            order: Vec::new(),
            grouping,
            hide_empty: false,
            segmented: false,
        };
        let keys = |order: Vec<ConversationKey>| -> Vec<String> {
            order.into_iter().map(|key| key.id).collect()
        };
        assert_eq!(
            keys(sidebar(SidebarGrouping::Project, Vec::new()).visible_order()),
            ["r1", "r2", "r3", "r4"]
        );

        // 按状态时上一段 / 下一段按组走，折起来的组跳过。
        let mut ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        ui.collapsed_buckets
            .insert((RuntimeKey::Local, Bucket::Idle));
        let buckets = bucket_groups(&RuntimeKey::Local, &projects, &ui, Local::now());
        assert_eq!(
            keys(sidebar(SidebarGrouping::Status, buckets).visible_order()),
            ["r2", "r4"]
        );
    }

    #[test]
    fn empty_projects_hide_only_when_asked_and_not_pinned_or_selected() {
        assert!(project_visible(true, true, false, false));
        assert!(project_visible(true, false, true, false));
        assert!(project_visible(true, false, false, true));
        assert!(!project_visible(true, false, false, false));
        assert!(project_visible(false, false, false, false));
    }

    #[test]
    fn agent_marks_take_the_first_character() {
        assert_eq!(agent_mark("写作助手"), "写");
        assert_eq!(agent_mark("  writer"), "w");
        assert_eq!(agent_mark(""), "智");
    }

    #[test]
    fn project_names_use_the_last_segment() {
        assert_eq!(project_name(PathStyle::Posix, "/a/smelt"), "smelt");
        assert_eq!(project_name(PathStyle::Posix, ""), "未指定目录");
    }
}
