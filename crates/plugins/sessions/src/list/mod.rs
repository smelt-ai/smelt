//! 会话列表（S1）：视图 [`crate::contract::LIST`]，按项目分组列出会话，「对话」一段
//! 放跟智能体的对话。旧宿主的 `session_list/*`、`sidebar_order.rs`、`conversations.rs` 的
//! 侧栏部分搬到这里（本目录下的文件都归 S1）。
//!
//! - 本文件：侧栏算好的样子（[`Sidebar`]），和排序用的纯函数。只在事件里算（
//!   `Sessions::rebuild_sidebar`），render 只读它。
//! - [`group`]：按状态、时间分组。
//! - [`machine`]：机器的系统图标和连接指示。
//! - [`view`]：列表视图，行内改名、拖拽、从访达拖进目录的界面状态。
//! - [`rows`]：各种行怎么画。
//! - [`menus`]：机器、项目、工作集、会话的菜单，「筛选与分组」菜单，和它们的动作。
//! - [`offscreen`]：滚到可视区外、要你看一眼的会话，在列表边上浮的提示。
//!
//! 项目里的会话按项目（默认）、状态或时间分组；按项目时可以藏起没有会话的项目。默认所有
//! 机器的项目排在一份列表里：机器只是项目的一个属性，远程项目在行上标机器名，机器连不上时
//! 它的项目变灰、写为什么；机器的菜单和「在哪台机器上打开项目」在「项目」标题上。挂着远程
//! 机器时可以打开「按机器分开」：一台机器一片（[`Area`]），机器的标题行带连接状态和菜单，
//! 片里照常按分组依据排，行上不再标机器名。没有键盘
//! 上下移动、字母跳转、分段拖动高度、Cmd+1..9、归档区、额度、设置按钮（按 11-16 的决定删了
//! 或归别的插件）。

mod actions;
mod group;
mod machine;
mod menus;
mod motion;
mod offscreen;
mod rows;
mod view;

use rust_i18n::t;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use gpui::{App, SharedString};
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{WorkspaceConversation, WorkspaceDirectory};
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::attention::Unread;
use crate::model::{
    self, ConversationKey, Fold, Pin, Placement, ProjectKey, Sessions, SidebarGrouping, SidebarUi,
};
use crate::worksets::{MergeSummary, WorksetCheck, WorksetKey};

pub(crate) use group::Bucket;
pub(crate) use view::build_view;

use machine::MachineLink;
use offscreen::{Notice, Tally};

/// 侧栏算好的样子：从目录、会话状态、机器表和 [`Sessions::ui`] 现算的行。存在
/// [`Sessions::sidebar`]，render 只读它；`Sessions::rebuild_sidebar` 重算。选中哪行不在
/// 这里（选中变了不重算），画的时候对着 `Sessions::selection` 看。
#[derive(Default)]
pub(crate) struct Sidebar {
    /// 「对话」一段；一段跟智能体的对话都没有时是 `None`，不画这一段。
    conversations: Option<ConversationsSection>,
    /// 所有机器的项目排成一份：跨机器的手动顺序，置顶的在前。
    projects: Vec<ProjectGroup>,
    /// 「项目」标题下的几片，从上到下：不按机器分开时只有一片，按机器分开时一台机器一片。
    areas: Vec<Area>,
    /// 按机器分开（[`SidebarUi::by_machine`]，且挂着远程机器）。
    by_machine: bool,
    /// 机器表里的每台机器（「项目」标题上的机器菜单、「在哪台机器上打开」，还没收到目录的
    /// 机器那一行）。
    machines: Vec<MachineInfo>,
    /// 各机器上项目里的会话从上到下（拖会话、置顶时整份写回那台机器的 `conversation_order`）。
    conversation_ids: HashMap<RuntimeKey, Vec<String>>,
    order: Vec<ConversationKey>,
    /// 项目里的会话按什么分组。只有按项目时会话、项目能拖着换位置（别的分法顺序是算出来的）。
    grouping: SidebarGrouping,
    /// 按项目分组时藏起没有会话的项目（[`project_visible`]）。
    hide_empty: bool,
    /// 正按名字搜项目（[`Sessions::project_filter`] 不空）：各片里只有搜到的项目，一个都没有
    /// 时写「没有匹配的项目」而不是「没有打开的项目」。
    filtering: bool,
    /// 列表上要你看一眼的行，从上到下（[`offscreen`]）。
    notices: Vec<Notice>,
}

/// 「对话」一段：所有机器上跟智能体的对话，置顶的在前，其余按最近动静排。
struct ConversationsSection {
    collapsed: bool,
    /// 折起来时标在标题上的状态。
    status: AgentStatus,
    rows: Vec<SessionRow>,
}

/// 机器表里的一台机器。
struct MachineInfo {
    key: RuntimeKey,
    name: SharedString,
    os_icon: SharedString,
    link: MachineLink,
    /// 工作区目录收到了没有。没收到（还没连上过）时它的项目还不知道，列表里给它留一行说明。
    received: bool,
}

/// 「项目」标题下的一片。不按机器分开时只有一片，装着所有机器的项目；按机器分开时一台机器
/// 一片（机器表里的每台都有，目录还没收到的也有），上面是机器的标题行。
struct Area {
    /// 按机器分开时是这片的机器；不分开是 `None`。
    machine: Option<RuntimeKey>,
    /// 按机器分开时这片折起来了。
    collapsed: bool,
    /// 折起来时标在机器标题上的状态和会话数。
    status: AgentStatus,
    sessions: usize,
    /// 这片的项目，[`Sidebar::projects`] 的下标，从上到下。
    projects: Vec<usize>,
    /// 按状态、时间分组时这片的组；按项目时空着，画 [`Self::projects`]。
    buckets: Vec<BucketGroup>,
}

/// 按状态、时间分出来的一组是哪组：不按机器分开时所有机器一起分（`machine` 是 `None`），
/// 分开时各机器各分各的、各折各的。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BucketKey {
    pub(crate) machine: Option<RuntimeKey>,
    pub(crate) bucket: Bucket,
}

/// 按状态或时间分出来的一组：项目里的会话打散了重新分，每行带上它来自哪个项目。
struct BucketGroup {
    key: BucketKey,
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
    /// 远程机器的名字，标在行上；本机的不标，是 `None`。
    machine_name: Option<SharedString>,
    /// 机器连得怎样：用不了时整行变灰、写为什么，悬停机器名看完整说明。
    link: MachineLink,
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
    /// 有一格有没看过的提醒（跑完了、失败了你还没看），运行时记的。
    unread: bool,
    /// 机器眼下用不了时行尾写的字。
    unavailable: Option<SharedString>,
    /// 悬停的说明：完整标题（窄侧栏里常被截断）。
    tooltip: SharedString,
    /// 不在项目下面画时（「对话」一段、按状态和时间分的组）标在行上的远程机器名；在项目下面
    /// 时项目行已经标了，是 `None`。
    machine_name: Option<SharedString>,
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
    /// 哪家 CLI / ACP 的单色图标（资源路径），悬停看是哪家；跟智能体的对话画它底下引擎的
    /// 图标，悬停看「智能体名 · 引擎」。
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
        let mut projects = Vec::new();
        let mut machines = Vec::new();
        let mut conversation_ids = HashMap::new();
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
            let directory = host.workspace_directory(&machine.key, cx);
            if let Some(directory) = &directory {
                let input = MachineInput {
                    machine: &machine.key,
                    style: host.path_style(&machine.key, cx),
                    directory,
                    states: &host.session_states(&machine.key, cx),
                    agents: sessions.agent_names.get(&machine.key).unwrap_or(&no_agents),
                    unread: &sessions.unread,
                    link: &link,
                    remote_name: (!matches!(machine.key, RuntimeKey::Local))
                        .then(|| machine.name.clone().into()),
                    checks: &sessions.workset_checks,
                };
                let ids = fill_machine(&input, &sessions.ui, &mut projects, &mut agent_rows);
                conversation_ids.insert(machine.key.clone(), ids);
            }
            machines.push(MachineInfo {
                os_icon: machine::os_icon(&machine.key, &connections).into(),
                key: machine.key,
                name: machine.name.into(),
                link,
                received: directory.is_some(),
            });
        }
        let projects = manual_first(projects, &sessions.ui.project_order, |project, key| {
            project.machine == key.machine && project.root == key.root
        });
        let projects = pinned_first(projects, |project| project.pinned);
        let by_machine = sessions.ui.by_machine && has_remote(&machines);
        let filter = sessions.project_filter.as_str();
        let areas = if by_machine {
            machines
                .iter()
                .map(|machine| area(Some(&machine.key), &projects, &sessions.ui, filter, now))
                .collect()
        } else {
            vec![area(None, &projects, &sessions.ui, filter, now)]
        };
        let conversations = (!agent_rows.is_empty()).then(|| {
            agent_rows.sort_by(|a: &SessionRow, b| b.activity.cmp(&a.activity));
            let rows = pinned_first(agent_rows, |row| row.pinned);
            ConversationsSection {
                collapsed: sessions.ui.is_collapsed(&Fold::Conversations),
                status: AgentStatus::highest(rows.iter().map(|row| row.status)),
                rows,
            }
        });
        let mut sidebar = Self {
            conversations,
            projects,
            areas,
            by_machine,
            machines,
            conversation_ids,
            order: Vec::new(),
            grouping: sessions.ui.grouping,
            hide_empty: sessions.ui.hide_empty_projects,
            filtering: !filter.trim().is_empty(),
            notices: Vec::new(),
        };
        sidebar.order = sidebar.visible_order();
        sidebar.notices = sidebar.collect_notices();
        sidebar
    }

    /// 列表上看得见的会话，从上到下（折起来的不算）。上一段 / 下一段按它走。
    pub(crate) fn order(&self) -> &[ConversationKey] {
        &self.order
    }

    /// 按状态、时间分组时这段会话落在哪一组；按项目分组、或不在侧栏里时是 `None`。
    pub(crate) fn bucket_of(&self, key: &ConversationKey) -> Option<BucketKey> {
        self.areas
            .iter()
            .flat_map(|area| area.buckets.iter())
            .find(|group| group.rows.iter().any(|row| row.key == *key))
            .map(|group| group.key.clone())
    }

    /// 这片的项目，从上到下。
    fn area_projects<'a>(
        &'a self,
        area: &'a Area,
    ) -> impl Iterator<Item = (usize, &'a ProjectGroup)> {
        area.projects.iter().map(|ix| (*ix, &self.projects[*ix]))
    }

    /// 这片里所有的会话行（折起来的也算），只取当前分组用的那一处（按状态、时间分组时在组里，
    /// 按项目时在项目里）。
    fn area_rows<'a>(&'a self, area: &'a Area) -> Vec<&'a SessionRow> {
        if self.grouping != SidebarGrouping::Project {
            return area.buckets.iter().flat_map(|b| b.rows.iter()).collect();
        }
        self.area_projects(area)
            .flat_map(|(_, project)| project.items.iter())
            .flat_map(item_rows)
            .collect()
    }

    fn visible_order(&self) -> Vec<ConversationKey> {
        let mut order = Vec::new();
        if let Some(section) = self.conversations.as_ref().filter(|s| !s.collapsed) {
            order.extend(section.rows.iter().map(|row| row.key.clone()));
        }
        for area in self.areas.iter().filter(|area| !area.collapsed) {
            if self.grouping != SidebarGrouping::Project {
                for bucket in area.buckets.iter().filter(|b| !b.collapsed) {
                    order.extend(bucket.rows.iter().map(|row| row.key.clone()));
                }
                continue;
            }
            // 藏起来的空项目里没有会话，不用另外跳过。
            for (_, project) in self.area_projects(area).filter(|(_, p)| !p.collapsed) {
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

    /// 列表上各行（项目、组、工作树、会话）从上到下的身份，折起来的也算。列表视图拿前后两份
    /// 比（[`reordered`]），先后变了就让各行滑到新位置；身份也是各行滑动状态挂的 id。
    fn flip_order(&self) -> Vec<String> {
        let mut order = Vec::new();
        if let Some(section) = &self.conversations {
            order.extend(section.rows.iter().map(|row| session_flip(&row.key)));
        }
        for area in &self.areas {
            order.extend(area.machine.as_ref().map(machine_flip));
            if self.grouping != SidebarGrouping::Project {
                for bucket in &area.buckets {
                    order.push(bucket_flip(&bucket.key));
                    order.extend(bucket.rows.iter().map(|row| session_flip(&row.key)));
                }
                continue;
            }
            for (_, project) in self.area_projects(area) {
                order.push(project_flip(&project.machine, &project.root));
                for item in &project.items {
                    match item {
                        ProjectItem::Session(row) => order.push(session_flip(&row.key)),
                        ProjectItem::Workset(workset) => {
                            order.push(workset_flip(&workset.machine, &workset.id));
                            order.extend(workset.sessions.iter().map(|row| session_flip(&row.key)));
                        }
                    }
                }
            }
        }
        order
    }

    /// 列表上要你看一眼的行，从上到下：展开着的会话行各算各的，折起来的一组算在标题上。
    fn collect_notices(&self) -> Vec<Notice> {
        let mut notices = Vec::new();
        let mut push = |notice: Option<Notice>| notices.extend(notice);
        if let Some(section) = &self.conversations {
            if section.collapsed {
                push(folded(CONVERSATIONS_KEY.to_string(), &section.rows));
            } else {
                section.rows.iter().for_each(|row| push(row.notice()));
            }
        }
        for area in &self.areas {
            if let Some(machine) = area.machine.as_ref().filter(|_| area.collapsed) {
                push(folded(machine_flip(machine), self.area_rows(area)));
                continue;
            }
            if self.grouping != SidebarGrouping::Project {
                for bucket in &area.buckets {
                    if bucket.collapsed {
                        push(folded(bucket_flip(&bucket.key), &bucket.rows));
                    } else {
                        bucket.rows.iter().for_each(|row| push(row.notice()));
                    }
                }
                continue;
            }
            for (_, project) in self.area_projects(area) {
                if project.collapsed {
                    let rows = project.items.iter().flat_map(item_rows);
                    push(folded(project_flip(&project.machine, &project.root), rows));
                    continue;
                }
                for item in &project.items {
                    match item {
                        ProjectItem::Session(row) => push(row.notice()),
                        ProjectItem::Workset(workset) if workset.collapsed => push(folded(
                            workset_flip(&workset.machine, &workset.id),
                            &workset.sessions,
                        )),
                        ProjectItem::Workset(workset) => {
                            workset.sessions.iter().for_each(|row| push(row.notice()));
                        }
                    }
                }
            }
        }
        notices
    }

    /// 要你看一眼的行（[`offscreen`]），去掉正选中的那段会话：人正看着它。
    fn notices_besides(&self, selected: Option<&ConversationKey>) -> Vec<Notice> {
        let selected = selected.map(session_flip);
        self.notices
            .iter()
            .filter(|notice| Some(&notice.key) != selected.as_ref())
            .cloned()
            .collect()
    }

    /// 侧栏里所有的会话行（折起来的也算）。按状态分组时同一段会话在桶里，按项目分组时在
    /// 项目里，两处只取当前分组用的那一处（[`Self::area_rows`]）。
    fn session_rows(&self) -> impl Iterator<Item = &SessionRow> {
        let conversations = self
            .conversations
            .iter()
            .flat_map(|section| section.rows.iter());
        let areas = self.areas.iter().flat_map(|area| self.area_rows(area));
        conversations.chain(areas)
    }

    /// 侧栏里有没有这段会话（折起来的也算；搜索滤掉的项目里的不算）。
    pub(crate) fn has_session(&self, key: &ConversationKey) -> bool {
        self.session_rows().any(|row| row.key == *key)
    }

    /// 侧栏里正在跑的会话。
    fn running(&self) -> HashSet<ConversationKey> {
        self.session_rows()
            .filter(|row| row.status == AgentStatus::Running)
            .map(|row| row.key.clone())
            .collect()
    }

    fn has_remote(&self) -> bool {
        has_remote(&self.machines)
    }

    /// 列表里项目从上到下的身份（拖项目时在它上面挪）。
    fn project_keys(&self) -> Vec<ProjectKey> {
        self.projects.iter().map(ProjectGroup::key).collect()
    }

    /// 那台机器上项目里的会话从上到下。
    fn conversation_ids(&self, machine: &RuntimeKey) -> &[String] {
        self.conversation_ids
            .get(machine)
            .map_or(&[], Vec::as_slice)
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
                    .projects
                    .iter()
                    .find(|p| p.machine == *machine && p.root == *root)
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

/// 机器表里有没有远程机器。
fn has_remote(machines: &[MachineInfo]) -> bool {
    machines
        .iter()
        .any(|machine| !matches!(machine.key, RuntimeKey::Local))
}

/// 项目里的一项下面的会话行：一行会话，或工作集里的那些。
fn item_rows(item: &ProjectItem) -> &[SessionRow] {
    match item {
        ProjectItem::Session(row) => std::slice::from_ref(row),
        ProjectItem::Workset(workset) => &workset.sessions,
    }
}

/// 折起来的一组（标题的滑动身份是 `key`）合起来有几段要你看；点胶囊时打开组里第一段要你看
/// 的（打开时展开这一组）。一段都没有是 `None`。
fn folded<'a>(key: String, rows: impl IntoIterator<Item = &'a SessionRow>) -> Option<Notice> {
    let mut tally = Tally::default();
    let mut first = None;
    for row in rows {
        let own = row.tally();
        if !own.is_empty() && first.is_none() {
            first = Some(row.key.clone());
        }
        tally.add(own);
    }
    Some(Notice {
        key,
        tally,
        session: first?,
    })
}

impl SessionRow {
    /// 这一行要你看就是一条提示，点胶囊打开它自己。
    fn notice(&self) -> Option<Notice> {
        let tally = self.tally();
        (!tally.is_empty()).then(|| Notice {
            key: session_flip(&self.key),
            tally,
            session: self.key.clone(),
        })
    }

    /// 这一行算不算要你看：需要你处理的，或有没看过的提醒的。
    fn tally(&self) -> Tally {
        if self.status == AgentStatus::NeedsYou {
            Tally { need: 1, unread: 0 }
        } else if self.unread {
            Tally { need: 0, unread: 1 }
        } else {
            Tally::default()
        }
    }
}

impl ProjectGroup {
    fn key(&self) -> ProjectKey {
        ProjectKey {
            machine: self.machine.clone(),
            root: self.root.clone(),
        }
    }
}

/// 机器在 id 里的写法。
fn machine_id(machine: &RuntimeKey) -> &str {
    match machine {
        RuntimeKey::Local => "@local",
        RuntimeKey::Remote(id) => id,
    }
}

/// 「对话」一段标题的身份（它不跟着滑，只在折起来时当要盯的行用）。
const CONVERSATIONS_KEY: &str = "conversations";

fn session_flip(key: &ConversationKey) -> String {
    format!("session:{}:{}", machine_id(&key.machine), key.id)
}

fn project_flip(machine: &RuntimeKey, root: &str) -> String {
    format!("project:{}:{root}", machine_id(machine))
}

fn workset_flip(machine: &RuntimeKey, id: &str) -> String {
    format!("workset:{}:{id}", machine_id(machine))
}

fn bucket_flip(key: &BucketKey) -> String {
    match &key.machine {
        Some(machine) => format!("bucket:{}:{:?}", machine_id(machine), key.bucket),
        None => format!("bucket:{:?}", key.bucket),
    }
}

fn machine_flip(machine: &RuntimeKey) -> String {
    format!("machine:{}", machine_id(machine))
}

/// 前后两份身份里都有的那些，先后变了没有。只多了、少了几行（新建、关掉、折起）不算重排：
/// 那些行自己有落进、收起的动画，别的行跟着排版走。
fn reordered(old: &[String], new: &[String]) -> bool {
    let in_old: HashSet<&str> = old.iter().map(String::as_str).collect();
    let in_new: HashSet<&str> = new.iter().map(String::as_str).collect();
    old.iter()
        .filter(|key| in_new.contains(key.as_str()))
        .ne(new.iter().filter(|key| in_old.contains(key.as_str())))
}

/// 算一台机器要用的输入。
struct MachineInput<'a> {
    machine: &'a RuntimeKey,
    style: PathStyle,
    directory: &'a WorkspaceDirectory,
    states: &'a [DaemonSessionState],
    /// 智能体定义 id → 名字。
    agents: &'a HashMap<String, String>,
    unread: &'a Unread,
    link: &'a MachineLink,
    /// 远程机器的名字，标在项目行和不在项目下面的会话行上；本机是 `None`。
    remote_name: Option<SharedString>,
    /// 各棵工作树上一次问到的合并情况。
    checks: &'a HashMap<WorksetKey, WorksetCheck>,
}

/// 把一台机器的目录摆成项目组接到 `projects` 后面（按目录的顺序，跨机器的手动顺序和置顶
/// 由调用方排），跟智能体的对话放进 `agent_rows`（「对话」一段）。返回这台机器上项目里的会话
/// id，从上到下。
fn fill_machine(
    input: &MachineInput,
    ui: &SidebarUi,
    projects: &mut Vec<ProjectGroup>,
    agent_rows: &mut Vec<SessionRow>,
) -> Vec<String> {
    let empty = Vec::new();
    let manual_sessions = ui
        .machine(input.machine)
        .map_or(&empty, |ui| &ui.conversation_order);

    // 项目：打开的项目按目录的顺序，再接上只靠会话工作目录撑起来的组。
    let mut roots: Vec<String> = input.directory.projects.clone();
    let mut members: HashMap<String, Vec<(&WorkspaceConversation, Option<String>)>> =
        HashMap::new();
    for conversation in &input.directory.conversations {
        match model::placement(input.style, input.directory, conversation) {
            Placement::Hidden => {}
            Placement::Conversations => {
                let mut row = session_row(conversation, input, ui, GroupKey::Conversations);
                row.machine_name = input.remote_name.clone();
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
    let mut ids = Vec::new();
    for root in roots {
        let members = members.remove(&root).unwrap_or_default();
        let group = project_group(&root, members, manual_sessions, input, ui);
        ids.extend(group.items.iter().flat_map(|item| match item {
            ProjectItem::Session(row) => vec![row.key.id.clone()],
            ProjectItem::Workset(w) => w.sessions.iter().map(|row| row.key.id.clone()).collect(),
        }));
        projects.push(group);
    }
    ids
}

fn project_group(
    root: &str,
    members: Vec<(&WorkspaceConversation, Option<String>)>,
    manual_sessions: &[String],
    input: &MachineInput,
    ui: &SidebarUi,
) -> ProjectGroup {
    let machine = input.machine;
    let members = manual_first(members, manual_sessions, |(c, _), id| c.id == *id);
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
                Some((_, Err(error))) => vec![
                    t!(
                        "error.failed",
                        what = t!("action.check_merge"),
                        error = error
                    )
                    .into_owned(),
                ],
                None => Vec::new(),
            };
            let mut tooltip = t!(
                "workset.tooltip",
                task = workset.task,
                branch = workset.branch,
                repositories = repositories.join(&*t!("list.separator"))
            )
            .into_owned();
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
        machine_name: input.remote_name.clone(),
        link: input.link.clone(),
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
    let status = conversation.status(states);
    // 只有在跑时行尾才写「这一轮跑了多久」；停下来的格里留着的开始时间不算。
    let turn_started_at = if status == AgentStatus::Running {
        conversation
            .session_ids()
            .filter_map(|id| states.iter().find(|state| state.id == id))
            .map(|state| state.turn_started_at)
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let unread = conversation
        .session_ids()
        .any(|id| input.unread.contains(input.machine, id));
    let title: SharedString = model::title(conversation, states).into();
    let icon = row_icon(conversation, states, input.agents);
    SessionRow {
        pinned: ui.is_pinned(&Pin::Conversation(key.clone())),
        session_id: conversation.session_ids().next().map(str::to_string),
        key,
        tooltip: title.clone(),
        title,
        icon,
        status,
        activity,
        turn_started_at,
        unread,
        unavailable: input.link.unavailable.clone(),
        machine_name: None,
        group,
        origin: None,
    }
}

/// 「项目」标题下的一片：`machine` 是 `None` 时装所有机器的项目，否则只装那台机器的。
/// `filter` 是搜索框里的字，只装名字搜得到的项目（[`project_matches`]）；按状态、时间分组时
/// 组里也就只有这些项目的会话。
fn area(
    machine: Option<&RuntimeKey>,
    projects: &[ProjectGroup],
    ui: &SidebarUi,
    filter: &str,
    now: DateTime<Local>,
) -> Area {
    let members: Vec<usize> = projects
        .iter()
        .enumerate()
        .filter(|(_, project)| machine.is_none_or(|machine| project.machine == *machine))
        .filter(|(_, project)| project_matches(&project.name, filter))
        .map(|(ix, _)| ix)
        .collect();
    let buckets = bucket_groups(machine, members.iter().map(|ix| &projects[*ix]), ui, now);
    let rows: Vec<&SessionRow> = members
        .iter()
        .flat_map(|ix| projects[*ix].items.iter())
        .flat_map(item_rows)
        .collect();
    Area {
        collapsed: machine.is_some_and(|machine| ui.collapsed_machines.contains(machine)),
        machine: machine.cloned(),
        status: AgentStatus::highest(rows.iter().map(|row| row.status)),
        sessions: rows.len(),
        projects: members,
        buckets,
    }
}

/// 按状态、时间分组时的组：`projects` 里的会话（含工作树里的）按项目从上到下摊平，带上来源
/// 再分组；组里置顶的在前。按项目分组时是空的。`machine` 是这片的机器：不按机器分开
/// （`None`）时行上还要标远程机器名，分开时机器的标题行已经写了。
fn bucket_groups<'a>(
    machine: Option<&RuntimeKey>,
    projects: impl Iterator<Item = &'a ProjectGroup>,
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
                if machine.is_none() {
                    row.machine_name = project.machine_name.clone();
                }
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
            let key = BucketKey {
                machine: machine.cloned(),
                bucket,
            };
            BucketGroup {
                collapsed: ui.collapsed_buckets.contains(&key),
                key,
                status: AgentStatus::highest(members.iter().map(|row| row.status)),
                rows: members,
            }
        })
        .collect()
}

/// 项目名搜不搜得到：去掉两头空白后不分大小写地包含就算；空的什么都搜得到。
fn project_matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
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
    if let Some(acp) = &conversation.acp {
        let Some(kind) = ConversationAgentKind::from_id(&acp.agent) else {
            return RowIcon::Conversation;
        };
        // 智能体是谁看标题（建对话时写进去）和悬停；小标和别的对话一样只答「哪家」。
        let label = match conversation.agent_definition() {
            Some(id) => {
                let name = agents.get(id).map_or(id, |name| name.trim());
                let name = if name.is_empty() { id } else { name };
                format!("{name} · {}", kind.label())
            }
            None => kind.label().to_string(),
        };
        return RowIcon::Provider {
            path: kind.icon_asset().into(),
            label: label.into(),
        };
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

/// 项目组的名字：根目录的最后一段；没有工作目录的那组叫「未指定目录」。
fn project_name(style: PathStyle, root: &str) -> String {
    if root.is_empty() {
        return t!("project.no_folder").into_owned();
    }
    style
        .file_name(root)
        .filter(|name| !name.is_empty())
        .unwrap_or(root)
        .to_string()
}

// ---- 排序 ----

/// 手动排过的在前（按手动顺序），其余按原来的顺序接在后面。`is` 判断一项是不是手动顺序里
/// 的那一个。
fn manual_first<T, K>(items: Vec<T>, manual: &[K], is: impl Fn(&T, &K) -> bool) -> Vec<T> {
    let mut ranked: Vec<(usize, usize, T)> = items
        .into_iter()
        .enumerate()
        .map(|(ix, item)| {
            let rank = manual
                .iter()
                .position(|m| is(&item, m))
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
fn move_near<T, Q>(items: &mut Vec<T>, from: &Q, to: &Q, before: bool) -> bool
where
    T: PartialEq<Q>,
    Q: PartialEq + ?Sized,
{
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
fn splice_group<T: PartialEq + Clone>(all: &[T], group: &[T]) -> Vec<T> {
    let mut next = group.iter().filter(|id| all.contains(id));
    let mut out: Vec<T> = all
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

/// 列表里项目换了顺序之后要存的整份顺序：列表里的（`shown`，所有收到目录的机器上的项目）
/// 按新顺序；存着的顺序里列表上没有的（机器没连上、目录还没收到）不丢，留在存着时它前面
/// 那一项的后面，等机器连上时回到差不多的位置。
fn merge_project_order<T: PartialEq + Clone>(stored: &[T], shown: Vec<T>) -> Vec<T> {
    let mut out = shown;
    let mut after: Option<usize> = None;
    for item in stored {
        match out.iter().position(|shown| shown == item) {
            Some(at) => after = Some(at),
            None => {
                let at = after.map_or(0, |at| at + 1);
                out.insert(at, item.clone());
                after = Some(at);
            }
        }
    }
    out
}

/// 置顶状态改了之后，把 `item` 挪到组里置顶区和其余之间的交界：新置顶的排在置顶区最后，
/// 取消置顶的排在其余的最前。不挪的话它会落在按原顺序算出的位置，点完要满屏找。
/// `order` 是组里眼下从上到下的顺序，`pinned` 是改过之后的置顶判断。
fn move_to_pin_boundary<T: PartialEq + Clone>(
    order: &[T],
    item: &T,
    pinned: impl Fn(&T) -> bool,
) -> Vec<T> {
    let rest: Vec<&T> = order.iter().filter(|id| *id != item).collect();
    let (head, tail): (Vec<&T>, Vec<&T>) = rest.into_iter().partition(|id| pinned(id));
    head.into_iter()
        .cloned()
        .chain(std::iter::once(item.clone()))
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
            manual_first(items, &manual, |item, m| item == m),
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
    fn only_a_changed_order_of_common_rows_is_a_reorder() {
        let order = strings(&["a", "b", "c"]);
        assert!(!reordered(&order, &order));
        // 新建、关掉、折起只是多了少了行。
        assert!(!reordered(&order, &strings(&["new", "a", "b", "c"])));
        assert!(!reordered(&order, &strings(&["a", "c"])));
        assert!(reordered(&order, &strings(&["b", "a", "c"])));
        assert!(reordered(&order, &strings(&["c", "new", "a"])));
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
    fn project_order_keeps_projects_of_machines_not_shown() {
        // x 在没连上的机器上：列表里没有它，拖完照样留在原来跟着的那一项后面。
        assert_eq!(
            merge_project_order(&strings(&["x", "a", "b"]), strings(&["c", "a", "b"])),
            strings(&["x", "c", "a", "b"])
        );
        assert_eq!(
            merge_project_order(&strings(&["a", "x", "b"]), strings(&["b", "a", "c"])),
            strings(&["b", "a", "x", "c"])
        );
        // 从没拖过：就是列表里的新顺序。
        assert_eq!(
            merge_project_order(&[], strings(&["c", "a", "b"])),
            strings(&["c", "a", "b"])
        );
    }

    #[test]
    fn pinning_lands_on_the_boundary() {
        let order = strings(&["p1", "p2", "a", "b", "c"]);
        // 置顶 c：排在 p1、p2 之后。
        let pinned = |id: &String| matches!(id.as_str(), "p1" | "p2" | "c");
        assert_eq!(
            move_to_pin_boundary(&order, &"c".to_string(), pinned),
            strings(&["p1", "p2", "c", "a", "b"])
        );
        // 取消置顶 p1：排在其余的最前。
        let pinned = |id: &String| id == "p2";
        assert_eq!(
            move_to_pin_boundary(&order, &"p1".to_string(), pinned),
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
            unread: false,
            unavailable: None,
            tooltip: id.to_string().into(),
            machine_name: None,
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
            machine_name: None,
            link: MachineLink {
                light: machine::Light::Good,
                detail: String::new(),
                unavailable: None,
            },
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

    /// 两个项目：本机的 a（分支 main）里一段会话加一个工作树，远程机器 box 上的 b 里一段置顶
    /// 的、一段要人管的。
    fn two_projects() -> Vec<ProjectGroup> {
        use AgentStatus::*;
        let mut projects = vec![
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
        ];
        projects[1].machine = box_machine();
        projects[1].machine_name = Some("box".into());
        projects
    }

    fn box_machine() -> RuntimeKey {
        RuntimeKey::Remote("box".to_string())
    }

    fn bucket_key(bucket: Bucket) -> BucketKey {
        BucketKey {
            machine: None,
            bucket,
        }
    }

    /// 按 `ui` 摆好的侧栏：`machines` 为空时不按机器分开，一片装所有项目；否则一台一片。
    fn laid_out(projects: Vec<ProjectGroup>, machines: &[RuntimeKey], ui: &SidebarUi) -> Sidebar {
        let now = Local::now();
        let areas = if machines.is_empty() {
            vec![area(None, &projects, ui, "", now)]
        } else {
            machines
                .iter()
                .map(|machine| area(Some(machine), &projects, ui, "", now))
                .collect()
        };
        Sidebar {
            projects,
            areas,
            by_machine: !machines.is_empty(),
            grouping: ui.grouping,
            ..Default::default()
        }
    }

    fn keys(order: Vec<ConversationKey>) -> Vec<String> {
        order.into_iter().map(|key| key.id).collect()
    }

    #[test]
    fn status_grouping_takes_every_project_row_with_its_origin() {
        let mut ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        ui.collapsed_buckets.insert(bucket_key(Bucket::Idle));
        let groups = bucket_groups(None, two_projects().iter(), &ui, Local::now());

        let buckets: Vec<Bucket> = groups.iter().map(|g| g.key.bucket).collect();
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
        // 项目标题不在了，远程机器的会话行上自己标机器名；本机的不标。
        assert_eq!(groups[0].rows[0].machine_name, None);
        assert_eq!(
            groups[0].rows[1].machine_name.as_ref().map(|n| n.as_ref()),
            Some("box")
        );
        // 分组不改会话属于哪个项目（置顶照常按项目里的顺序挪）。
        assert!(matches!(
            &groups[0].rows[0].group,
            GroupKey::Project { workset: Some(w), .. } if w == "修复"
        ));
    }

    #[test]
    fn project_grouping_keeps_project_order() {
        let ui = SidebarUi::default();
        assert!(bucket_groups(None, two_projects().iter(), &ui, Local::now()).is_empty());
        assert_eq!(
            keys(laid_out(two_projects(), &[], &ui).visible_order()),
            ["r1", "r2", "r3", "r4"]
        );

        // 按状态时上一段 / 下一段按组走，折起来的组跳过。
        let mut ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        ui.collapsed_buckets.insert(bucket_key(Bucket::Idle));
        assert_eq!(
            keys(laid_out(two_projects(), &[], &ui).visible_order()),
            ["r2", "r4"]
        );
    }

    /// 按机器分开：一台一片，片里照常分组；行上不再标机器名；折起来的机器跳过，要你看的算在
    /// 机器标题上。
    #[test]
    fn by_machine_splits_areas_per_machine() {
        let machines = [box_machine(), RuntimeKey::Local];
        let ui = SidebarUi::default();
        let sidebar = laid_out(two_projects(), &machines, &ui);
        assert_eq!(sidebar.areas[0].projects, [1]);
        assert_eq!(sidebar.areas[1].projects, [0]);
        assert_eq!(sidebar.areas[0].status, AgentStatus::NeedsYou);
        assert_eq!(sidebar.areas[0].sessions, 2);
        // 片按机器表的顺序，不按项目的顺序。
        assert_eq!(keys(sidebar.visible_order()), ["r3", "r4", "r1", "r2"]);
        let flips = sidebar.flip_order();
        assert_eq!(flips[0], machine_flip(&box_machine()));
        assert!(flips.contains(&machine_flip(&RuntimeKey::Local)));

        let mut ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        ui.collapsed_machines.insert(box_machine());
        // 折起来的组按机器各折各的：折本机的「需要你」不折 box 的。
        ui.collapsed_buckets.insert(BucketKey {
            machine: Some(RuntimeKey::Local),
            bucket: Bucket::NeedsYou,
        });
        let sidebar = laid_out(two_projects(), &machines, &ui);
        assert!(sidebar.areas[0].collapsed);
        let on_box = &sidebar.areas[0].buckets[0];
        assert_eq!(on_box.key.machine, Some(box_machine()));
        assert!(!on_box.collapsed);
        assert_eq!(on_box.rows[0].machine_name, None);
        assert_eq!(keys(sidebar.visible_order()), ["r1"]);
        let notices: Vec<String> = sidebar
            .collect_notices()
            .into_iter()
            .map(|notice| notice.key)
            .collect();
        let local_need = bucket_flip(&BucketKey {
            machine: Some(RuntimeKey::Local),
            bucket: Bucket::NeedsYou,
        });
        assert_eq!(notices, [machine_flip(&box_machine()), local_need]);
    }

    /// 展开的会话行各算各的，折起来的项目算在标题上；正选中的那段不算。
    #[test]
    fn notices_point_at_rows_or_folded_groups() {
        let mut projects = two_projects();
        let ProjectItem::Session(r1) = &mut projects[0].items[0] else {
            panic!("a 的第一项是会话");
        };
        r1.unread = true;
        let mut sidebar = laid_out(projects, &[], &SidebarUi::default());
        let notices = |sidebar: &Sidebar| -> Vec<(String, Tally)> {
            sidebar
                .collect_notices()
                .into_iter()
                .map(|notice| (notice.key, notice.tally))
                .collect()
        };
        // 点胶囊打开哪段：会话行是它自己，折起来的一组是组里第一段要你看的。
        let opens = |sidebar: &Sidebar| -> Vec<String> {
            sidebar
                .collect_notices()
                .iter()
                .map(|notice| notice.session.id.clone())
                .collect()
        };
        let key = |id: &str| {
            session_flip(&ConversationKey {
                machine: RuntimeKey::Local,
                id: id.to_string(),
            })
        };
        let need = Tally { need: 1, unread: 0 };
        let unread = Tally { need: 0, unread: 1 };
        assert_eq!(
            notices(&sidebar),
            [(key("r1"), unread), (key("r2"), need), (key("r4"), need)]
        );

        sidebar.projects[0].collapsed = true;
        sidebar.notices = sidebar.collect_notices();
        let a = project_flip(&RuntimeKey::Local, "/a");
        assert_eq!(
            notices(&sidebar),
            [(a.clone(), Tally { need: 1, unread: 1 }), (key("r4"), need)]
        );
        assert_eq!(opens(&sidebar), ["r1", "r4"]);
        let selected = ConversationKey {
            machine: RuntimeKey::Local,
            id: "r4".to_string(),
        };
        let left: Vec<String> = sidebar
            .notices_besides(Some(&selected))
            .into_iter()
            .map(|notice| notice.key)
            .collect();
        assert_eq!(left, [a]);
    }

    #[test]
    fn project_search_ignores_case_and_surrounding_spaces() {
        assert!(project_matches("Smelt-Plugin-Demo", "  plugin "));
        assert!(project_matches("onoo", ""));
        assert!(project_matches("onoo", "   "));
        assert!(!project_matches("onoo", "smelt"));
    }

    /// 搜索只留名字搜得到的项目，上一段 / 下一段、按状态分的组都跟着只剩它们的会话。
    #[test]
    fn project_search_keeps_only_matching_projects() {
        let now = Local::now();
        let projects = two_projects();
        let ui = SidebarUi::default();
        assert_eq!(area(None, &projects, &ui, "B", now).projects, [1]);
        assert!(area(None, &projects, &ui, "zzz", now).projects.is_empty());

        let ui = SidebarUi {
            grouping: SidebarGrouping::Status,
            ..Default::default()
        };
        let found = area(None, &projects, &ui, "b", now);
        let rows: Vec<&str> = found
            .buckets
            .iter()
            .flat_map(|bucket| bucket.rows.iter().map(|row| row.key.id.as_str()))
            .collect();
        assert_eq!(rows, ["r4", "r3"]);
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
    fn agent_conversations_badge_their_engine_and_name_the_agent_on_hover() {
        use smelt_plugin_api::model::workspace_directory::WorkspaceAcp;
        let pi = ConversationAgentKind::Pi;
        let with = |definition: Option<&str>| WorkspaceConversation {
            acp: Some(WorkspaceAcp {
                agent: pi.id().into(),
                agent_definition_id: definition.map(str::to_string),
                ..WorkspaceAcp::default()
            }),
            ..WorkspaceConversation::default()
        };
        let agents = HashMap::from([
            ("dev".to_string(), "开发助理".to_string()),
            ("blank".to_string(), " ".to_string()),
        ]);
        let icon = |definition| row_icon(&with(definition), &[], &agents);
        let badge = |label: String| RowIcon::Provider {
            path: pi.icon_asset().into(),
            label: label.into(),
        };
        assert_eq!(
            icon(Some("dev")),
            badge(format!("开发助理 · {}", pi.label()))
        );
        assert_eq!(
            icon(Some("blank")),
            badge(format!("blank · {}", pi.label()))
        );
        assert_eq!(icon(Some("gone")), badge(format!("gone · {}", pi.label())));
        assert_eq!(icon(None), badge(pi.label().to_string()));
    }

    #[test]
    fn project_names_use_the_last_segment() {
        assert_eq!(project_name(PathStyle::Posix, "/a/smelt"), "smelt");
        assert_eq!(project_name(PathStyle::Posix, ""), "No Folder");
    }
}
