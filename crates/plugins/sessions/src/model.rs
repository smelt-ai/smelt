//! 会话插件的状态和从工作区目录算东西的纯函数。
//!
//! 权威在哪：
//! - 有哪些会话、每段会话的终端和对话、项目、工作集：那台机器的工作区目录
//!   （`Host::workspace_directory`），运行时是唯一写入方。这里不另存一份，用的时候现取。
//! - 每一格在干什么、叫什么：守护推来的会话状态（`Host::session_states`），同样现取。
//! - 置顶、手动顺序、折叠、分屏布局、上次选中：本插件的库（[`crate::store`]）。
//!   [`Sessions::ui`] 是侧栏那部分的内存缓存，启动时读一次，改的时候同一事件里写库。
//!   分组依据、隐藏空项目也在 [`Sessions::ui`] 里，但库里还没有它们的列，暂时只在内存里。
//! - 哪些格有没看过的提醒：运行时推来的 `attention.changed`，[`Sessions::unread`] 是它的
//!   只读缓存。
//! - 选中哪段会话或哪个项目：只在内存里（[`Sessions::selection`]），选中的会话另存一份进库，
//!   下次启动恢复。
//! - 列表里哪一行上的操作刚失败了：只在内存里（[`Sessions::failures`]），是这次运行里刚发生
//!   的事，不是数据。
//! - 建好的会话（视图实例、分屏）：归会话视图（`session_view::SessionView`），它在窗口里建、
//!   在窗口里扔，不放在这里。选中会话眼下那一格由它推来一份只读缓存
//!   （[`Sessions::active_pane`]）。
//!
//! 整个插件只有一份 [`Sessions`]（全局实体，[`Sessions::global`]），列表、会话视图、
//! 命令都读它；变了 `cx.notify()`，看它的视图 `observe` 着重画。这里只放数据，不放
//! 绑着窗口的东西。

use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::session_control::is_background_acp_session_id;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceRevision, project_root_of,
};

use crate::attention::Unread;
use crate::contract::SessionAt;
use crate::lifecycle;
use crate::list::{BucketKey, Sidebar};
use crate::peers::conversation::ConversationStart;
use crate::session_view::Pane;
use crate::store::SessionsStore;
use crate::worksets::{WorksetCheck, WorksetKey};

/// 一段会话：哪台机器的工作区目录里的哪条对话。对话 id 是 uuid，跨机器实际不会撞，但
/// 机器是会话身份的一部分（库里的行、选中、跳转都带着），不靠「不会撞」省掉它。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ConversationKey {
    pub(crate) machine: RuntimeKey,
    /// 工作区目录里那条对话的 `id`（不是守护会话 id）。
    pub(crate) id: String,
}

/// 一个项目组：哪台机器上的哪个根目录。侧栏里所有机器的项目排在一份顺序里，靠它认。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProjectKey {
    pub(crate) machine: RuntimeKey,
    pub(crate) root: String,
}

/// 会话插件的全部内存状态。
pub(crate) struct Sessions {
    pub(crate) store: SessionsStore,
    /// 侧栏的置顶、顺序、折叠（库里那份的缓存）。只经 [`Self::set_pinned`] 这些方法改。
    pub(crate) ui: SidebarUi,
    /// 选中的会话或项目，见 [`Selection`]。只由 lifecycle 改（`select`、`select_project`，
    /// 没了、关了、机器移除时清掉）。
    pub(crate) selection: Selection,
    /// 选中的会话眼下那一格：会话视图从分屏推来的只读缓存（权威在分屏），带着是哪段会话，
    /// 和 [`Self::selection`] 对不上就不算。活动上下文、`SEND_TEXT` 按它。只由会话视图改，
    /// 改了不通知（没有谁画它）。
    pub(crate) active_pane: Option<(ConversationKey, Pane)>,
    /// 新建、分叉刚写进目录、还没建过视图的对话：建对话视图时带上，让它当新对话拉起
    /// （见 `peers::conversation::ConversationArgs::start`）。建视图时取走。
    pub(crate) starts: HashMap<ConversationKey, ConversationStart>,
    /// 在这台 app 上刚新建、还没在列表里露过面的会话：列表等它出现在侧栏里时取走，播一次
    /// 落进列表的动画。只由 `lifecycle::create_conversation` 放、列表视图取，不通知。
    pub(crate) fresh: Option<ConversationKey>,
    /// 要在列表里找出来的会话（[`crate::contract::REVEAL_IN_LIST`]）：列表视图取走，滚到那一行、
    /// 闪一下。放的一方通知。
    pub(crate) located: Option<ConversationKey>,
    /// 侧栏算好的行（列表画它，上一段 / 下一段按它的顺序）。目录、会话状态、机器、
    /// [`Self::ui`] 变了由 [`Self::rebuild_sidebar`] 重算；render 只读它。
    pub(crate) sidebar: Rc<Sidebar>,
    /// 各台机器上智能体定义 id → 名字，会话行标是跟谁开的。权威在智能体插件
    /// （`smelt.agents`），这里是只读缓存：lifecycle 起来时拉一份整的，之后跟着它的
    /// 「配置换了」事件整台换掉。
    pub(crate) agent_names: HashMap<RuntimeKey, HashMap<String, String>>,
    /// 生命周期（S3）自己的状态：订阅、看过哪些机器的目录、对话翻没翻在最新处等。
    pub(crate) lifecycle: lifecycle::State,
    /// 各棵工作树上一次问到的合并情况（权威在运行时，这里只是缓存），侧栏按它画状态。只由
    /// [`crate::worksets`] 改。
    pub(crate) workset_checks: HashMap<WorksetKey, WorksetCheck>,
    /// 哪些格有没看过的提醒（运行时推来的只读缓存），侧栏按它提亮标题。只由 lifecycle 改。
    pub(crate) unread: Unread,
    /// 行首头像用哪个（库里那份的缓存）。只经 [`Self::set_avatar`] 改。
    pub(crate) avatar: AvatarChoice,
    /// 「项目」标题上搜索框里的字：侧栏只留名字里含它的项目（[`crate::list`] 的
    /// `project_matches`）。空是不搜。只在内存里，不存库：关了 app 就清空。只经
    /// [`Self::set_project_filter`] 改。
    pub(crate) project_filter: String,
    /// 在列表里某一行上发起、失败了的操作，画在那一行上（`list::rows` 的 `failure_mark`）。
    /// 同一行再发起操作时先清掉（[`crate::report_row_failure`]），点那个标记也清掉。只经
    /// [`Self::set_failure`] 改。
    pub(crate) failures: Rc<HashMap<FailTarget, Failure>>,
}

/// 列表里能挂失败的一行。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FailTarget {
    Conversation(ConversationKey),
    Project { machine: RuntimeKey, root: String },
    Workset(WorksetKey),
}

/// 一行上失败的操作：做的是什么（「关闭会话」这类动宾短语）、为什么没做成。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) what: SharedString,
    pub(crate) error: SharedString,
}

impl Failure {
    /// 悬停在标记上看到的话。
    pub(crate) fn message(&self) -> SharedString {
        t!("error.failed", what = self.what, error = self.error)
            .into_owned()
            .into()
    }
}

struct SessionsGlobal(Entity<Sessions>);

impl Global for SessionsGlobal {}

impl Sessions {
    /// 打开库、读回侧栏状态和上次的选中，建好全局实体。`register` 一开头调一次。
    ///
    /// 上次的选中直接放回 [`Self::selection`]：那段会话可能在 app 关着时被关掉了，那台机器
    /// 的目录推到时里面没有它，lifecycle 照常清掉（库里那行一起）。会话视图等目录里有了它
    /// 再建。
    pub(crate) fn install(cx: &mut App) -> Entity<Self> {
        let store = SessionsStore::open();
        // TODO(待确认表结构)：分组依据、隐藏空项目还没有库里的列，`store.sidebar()` 读不回来，
        // 每次启动是默认（按项目、全部显示）。表定了在这里一起读回，写在 `save_view_options`。
        let ui = store.sidebar();
        let selection = store
            .selection()
            .map_or(Selection::None, Selection::Session);
        let avatar = store.avatar_choice();
        let sessions = cx.new(|cx| {
            let mut sessions = Self {
                store,
                ui,
                selection,
                active_pane: None,
                starts: HashMap::new(),
                fresh: None,
                located: None,
                sidebar: Rc::default(),
                agent_names: HashMap::new(),
                lifecycle: lifecycle::State::default(),
                workset_checks: HashMap::new(),
                unread: Unread::default(),
                avatar,
                project_filter: String::new(),
                failures: Rc::default(),
            };
            sessions.sidebar = Rc::new(Sidebar::build(&sessions, cx));
            sessions
        });
        cx.set_global(SessionsGlobal(sessions.clone()));
        sessions
    }

    /// 插件的那份状态。`register` 之后才有；视图、命令都在那之后。
    pub(crate) fn global(cx: &App) -> Entity<Self> {
        cx.global::<SessionsGlobal>().0.clone()
    }

    /// 重算侧栏并通知重画。只在事件里调（目录、会话状态、机器变了，置顶这些改了）。
    pub(crate) fn rebuild_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar = Rc::new(Sidebar::build(self, cx));
        cx.notify();
    }

    /// 给一行挂上或摘掉失败。没变就不通知。
    pub(crate) fn set_failure(
        &mut self,
        target: FailTarget,
        failure: Option<Failure>,
        cx: &mut Context<Self>,
    ) {
        if self.failures.get(&target) == failure.as_ref() {
            return;
        }
        let failures = Rc::make_mut(&mut self.failures);
        match failure {
            Some(failure) => failures.insert(target, failure),
            None => failures.remove(&target),
        };
        cx.notify();
    }

    /// 换行首头像：写库、通知，列表跟着重建头像。
    pub(crate) fn set_avatar(&mut self, choice: AvatarChoice, cx: &mut Context<Self>) {
        if self.avatar == choice {
            return;
        }
        self.store.save_avatar_choice(&choice);
        self.avatar = choice;
        cx.notify();
    }

    /// 置顶或取消。新置顶的排在同类已置顶的最后。
    pub(crate) fn set_pinned(&mut self, pin: Pin, pinned: bool, cx: &mut Context<Self>) {
        if !self.ui.set_pinned(&pin, pinned) {
            return;
        }
        self.store.set_pinned(&pin, pinned);
        self.rebuild_sidebar(cx);
    }

    /// 折起或展开。
    pub(crate) fn set_collapsed(&mut self, fold: Fold, collapsed: bool, cx: &mut Context<Self>) {
        if !self.ui.set_collapsed(&fold, collapsed) {
            return;
        }
        self.store.set_collapsed(&fold, collapsed);
        self.rebuild_sidebar(cx);
    }

    /// 换掉项目的手动顺序（所有机器一份，整份给，拖完一次给一次）。
    pub(crate) fn set_project_order(&mut self, order: Vec<ProjectKey>, cx: &mut Context<Self>) {
        if self.ui.project_order == order {
            return;
        }
        self.store.set_project_order(&order);
        self.ui.project_order = order;
        self.rebuild_sidebar(cx);
    }

    /// 换掉那台机器上会话的手动顺序（整份）。
    pub(crate) fn set_conversation_order(
        &mut self,
        machine: &RuntimeKey,
        ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let order = &mut self
            .ui
            .machines
            .entry(machine.clone())
            .or_default()
            .conversation_order;
        if *order == ids {
            return;
        }
        self.store.set_conversation_order(machine, &ids);
        *order = ids;
        self.rebuild_sidebar(cx);
    }

    /// 换分组依据（「筛选与分组」菜单）。
    pub(crate) fn set_grouping(&mut self, grouping: SidebarGrouping, cx: &mut Context<Self>) {
        if self.ui.grouping == grouping {
            return;
        }
        self.ui.grouping = grouping;
        self.save_view_options();
        self.rebuild_sidebar(cx);
    }

    /// 藏起或显示没有会话的项目（「筛选与分组」菜单、「已隐藏 N 个」那一行）。
    pub(crate) fn set_hide_empty_projects(&mut self, hide: bool, cx: &mut Context<Self>) {
        if self.ui.hide_empty_projects == hide {
            return;
        }
        self.ui.hide_empty_projects = hide;
        self.save_view_options();
        self.rebuild_sidebar(cx);
    }

    /// 按名字搜项目（「项目」标题上的搜索框，边打边搜）。空是不搜。
    pub(crate) fn set_project_filter(&mut self, query: String, cx: &mut Context<Self>) {
        if self.project_filter == query {
            return;
        }
        self.project_filter = query;
        self.rebuild_sidebar(cx);
    }

    /// 让侧栏露出这段会话：展开它所在的「对话」段，按项目分组时展开它的项目和工作集，按
    /// 状态、时间分组时展开它那一组。从通知、命令这些侧栏外面的地方跳过来时它可能正折着；
    /// 侧栏里点得到的本来就露着，什么都不改。
    pub(crate) fn reveal(&mut self, key: &ConversationKey, cx: &mut Context<Self>) {
        let host = smelt_plugin_api::host(cx);
        let Some(directory) = host.workspace_directory(&key.machine, cx) else {
            return;
        };
        let Some(conversation) = directory.conversations.iter().find(|c| c.id == key.id) else {
            return;
        };
        let style = host.path_style(&key.machine, cx);
        let at = placement(style, &directory, conversation);
        let mut changed = false;
        // 它在搜索滤掉的项目里：清掉搜索，不然展开了也看不见。
        if !self.project_filter.is_empty() && !self.sidebar.has_session(key) {
            self.project_filter.clear();
            changed = true;
        }
        for fold in folds_over(&key.machine, at, self.ui.grouping) {
            if self.ui.set_collapsed(&fold, false) {
                self.store.set_collapsed(&fold, false);
                changed = true;
            }
        }
        if changed {
            self.rebuild_sidebar(cx);
        }
        if self.ui.by_machine {
            self.set_machine_collapsed(&key.machine, false, cx);
        }
        if let Some(bucket) = self.sidebar.bucket_of(key) {
            self.set_bucket_collapsed(bucket, false, cx);
        }
    }

    /// 折起或展开按状态、时间分出来的一组。只改内存（见 [`SidebarUi::collapsed_buckets`]）。
    pub(crate) fn set_bucket_collapsed(
        &mut self,
        bucket: BucketKey,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.ui.collapsed_buckets.insert(bucket)
        } else {
            self.ui.collapsed_buckets.remove(&bucket)
        };
        if changed {
            self.rebuild_sidebar(cx);
        }
    }

    /// 打开或关掉「按机器分开」（「筛选与分组」菜单）。只改内存（见 [`SidebarUi::by_machine`]）。
    pub(crate) fn set_by_machine(&mut self, by_machine: bool, cx: &mut Context<Self>) {
        if self.ui.by_machine != by_machine {
            self.ui.by_machine = by_machine;
            self.rebuild_sidebar(cx);
        }
    }

    /// 按机器分开时折起或展开一台机器那片。只改内存（见 [`SidebarUi::collapsed_machines`]）。
    pub(crate) fn set_machine_collapsed(
        &mut self,
        machine: &RuntimeKey,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if collapsed {
            self.ui.collapsed_machines.insert(machine.clone())
        } else {
            self.ui.collapsed_machines.remove(machine)
        };
        if changed {
            self.rebuild_sidebar(cx);
        }
    }

    /// 存分组依据、隐藏空项目。
    fn save_view_options(&self) {
        // TODO(待确认表结构)：插件库还没有这两列，眼下只改内存里的 `ui`。表定了在这里
        // 写库（`self.ui.grouping`、`self.ui.hide_empty_projects`），读回见 `install`。
    }

    /// 那台机器的目录推到了：清掉库里、缓存里指着目录里已经没有的东西的行。不碰选中和
    /// 建好的会话（那归 lifecycle），也不重画（调用方随后会重算侧栏）。
    pub(crate) fn prune(
        &mut self,
        machine: &RuntimeKey,
        style: PathStyle,
        directory: &WorkspaceDirectory,
    ) {
        let known = known_ids(style, directory);
        self.ui.prune(machine, &known);
        self.store.prune(machine, &known);
    }

    /// 机器移除了：它的置顶、顺序、折叠、分屏、选中全删。只对真的从机器表里消失的调，
    /// 不对离线的调。
    pub(crate) fn forget_machine(&mut self, machine: &RuntimeKey) {
        self.ui.machines.remove(machine);
        self.ui.project_order.retain(|key| key.machine != *machine);
        self.ui.collapsed_machines.remove(machine);
        self.ui
            .collapsed_buckets
            .retain(|key| key.machine.as_ref() != Some(machine));
        self.store.forget_machine(machine);
    }
}

// ---- 侧栏的界面状态 ----

/// 侧栏的置顶、顺序、折叠，所有机器的；再加上「筛选与分组」里选的那几样。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidebarUi {
    pub(crate) machines: HashMap<RuntimeKey, MachineUi>,
    /// 用户拖出来的项目顺序，所有机器一份（侧栏里各机器的项目排在一起）；不在里面的按机器表、
    /// 目录的顺序接在后面。
    pub(crate) project_order: Vec<ProjectKey>,
    /// 「对话」那一段折起来了。不分机器：所有机器的智能体对话在一段里。
    pub(crate) conversations_collapsed: bool,
    /// 项目里的会话按什么分组。只管项目里的会话，「对话」一段不受它影响。
    pub(crate) grouping: SidebarGrouping,
    /// 「显示：有会话」：按项目分组时藏起没有会话、也没有工作树的项目（置顶的、选中的照常显示）。
    pub(crate) hide_empty_projects: bool,
    /// 按状态、时间分组时折起来的组（不按机器分开时所有机器的会话一起分，分开时各机器各一
    /// 套）。只在内存里：这些组是现算的，不是库里认得的东西，重启回到全展开。
    pub(crate) collapsed_buckets: HashSet<BucketKey>,
    /// 「按机器分开」：挂着远程机器时一台机器一片（只有本机时不起作用）。暂时只在内存里，
    /// 重启回到不分开。
    pub(crate) by_machine: bool,
    /// 按机器分开时折起来的机器。跟着 `by_machine` 只在内存里。
    pub(crate) collapsed_machines: HashSet<RuntimeKey>,
}

/// 侧栏「筛选与分组」里的「分组依据」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SidebarGrouping {
    /// 按项目（再按工作树）分组，顺序可以手动拖。
    #[default]
    Project,
    /// 按状态：需要你、运行中、空闲。
    Status,
    /// 按最近动静落在哪一天：今天、昨天、7 天内、更早。
    LastUpdated,
}

/// 一台机器上的侧栏状态。键都指着那台机器的工作区目录。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachineUi {
    /// 置顶的会话（目录里的对话 id），按置顶的先后。
    pub(crate) pinned_conversations: Vec<String>,
    /// 置顶的项目（根目录），按置顶的先后。
    pub(crate) pinned_projects: Vec<String>,
    /// 置顶的工作集，在项目里排前面。
    pub(crate) pinned_worksets: HashSet<String>,
    /// 用户拖出来的会话顺序；不在里面的按默认顺序接在后面。
    pub(crate) conversation_order: Vec<String>,
    pub(crate) collapsed_projects: HashSet<String>,
    pub(crate) collapsed_worksets: HashSet<String>,
}

/// 能置顶的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pin {
    Conversation(ConversationKey),
    Project { machine: RuntimeKey, root: String },
    Workset { machine: RuntimeKey, id: String },
}

/// 盖在一段会话上面、折起来就看不见它的那几层：「对话」段，或者（按项目分组时）它的项目
/// 和工作集。按状态、时间分组时项目、工作集不画，不算；那时的组见 [`Sidebar::bucket_of`]。
fn folds_over(machine: &RuntimeKey, placement: Placement, grouping: SidebarGrouping) -> Vec<Fold> {
    match placement {
        Placement::Hidden => Vec::new(),
        Placement::Conversations => vec![Fold::Conversations],
        Placement::Project { .. } if grouping != SidebarGrouping::Project => Vec::new(),
        Placement::Project { root, workset } => {
            let mut folds = vec![Fold::Project {
                machine: machine.clone(),
                root,
            }];
            folds.extend(workset.map(|id| Fold::Workset {
                machine: machine.clone(),
                id,
            }));
            folds
        }
    }
}

/// 行首头像用哪个。插槽 [`crate::contract::SESSION_AVATAR`] 只放一个，好几个插件都填时由
/// 用户选，谁优先这边不猜。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AvatarChoice {
    /// 没选过：用第一个填进来的（按登记顺序）。
    #[default]
    Unset,
    /// 不要头像，行首画图标。
    Icon,
    /// 用这个视图（id）。它没了（插件卸了）按没选过算，选择留着，装回来又是它。
    View(String),
}

impl AvatarChoice {
    /// 在填了插槽的视图（按登记顺序）里定用哪个；`None` 是画图标。
    pub(crate) fn pick<'a>(&self, filled: &'a [SharedString]) -> Option<&'a SharedString> {
        match self {
            Self::Icon => None,
            Self::View(id) => filled
                .iter()
                .find(|view| view.as_ref() == id)
                .or_else(|| filled.first()),
            Self::Unset => filled.first(),
        }
    }
}

/// 能折起来的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// 「对话」那一段。
    Conversations,
    Project {
        machine: RuntimeKey,
        root: String,
    },
    Workset {
        machine: RuntimeKey,
        id: String,
    },
}

impl SidebarUi {
    /// 那台机器的状态；没动过是 `None`（按全默认画）。
    pub(crate) fn machine(&self, machine: &RuntimeKey) -> Option<&MachineUi> {
        self.machines.get(machine)
    }

    pub(crate) fn is_pinned(&self, pin: &Pin) -> bool {
        match pin {
            Pin::Conversation(key) => self
                .machine(&key.machine)
                .is_some_and(|ui| ui.pinned_conversations.contains(&key.id)),
            Pin::Project { machine, root } => self
                .machine(machine)
                .is_some_and(|ui| ui.pinned_projects.contains(root)),
            Pin::Workset { machine, id } => self
                .machine(machine)
                .is_some_and(|ui| ui.pinned_worksets.contains(id)),
        }
    }

    pub(crate) fn is_collapsed(&self, fold: &Fold) -> bool {
        match fold {
            Fold::Conversations => self.conversations_collapsed,
            Fold::Project { machine, root } => self
                .machine(machine)
                .is_some_and(|ui| ui.collapsed_projects.contains(root)),
            Fold::Workset { machine, id } => self
                .machine(machine)
                .is_some_and(|ui| ui.collapsed_worksets.contains(id)),
        }
    }

    /// 改内存里这份，规则同库（新置顶的排最后）。没变是 `false`。
    fn set_pinned(&mut self, pin: &Pin, pinned: bool) -> bool {
        if self.is_pinned(pin) == pinned {
            return false;
        }
        match pin {
            Pin::Conversation(key) => toggle_listed(
                &mut self.machine_entry(&key.machine).pinned_conversations,
                &key.id,
                pinned,
            ),
            Pin::Project { machine, root } => toggle_listed(
                &mut self.machine_entry(machine).pinned_projects,
                root,
                pinned,
            ),
            Pin::Workset { machine, id } => {
                toggle_set(&mut self.machine_entry(machine).pinned_worksets, id, pinned)
            }
        }
        true
    }

    fn set_collapsed(&mut self, fold: &Fold, collapsed: bool) -> bool {
        if self.is_collapsed(fold) == collapsed {
            return false;
        }
        match fold {
            Fold::Conversations => self.conversations_collapsed = collapsed,
            Fold::Project { machine, root } => toggle_set(
                &mut self.machine_entry(machine).collapsed_projects,
                root,
                collapsed,
            ),
            Fold::Workset { machine, id } => toggle_set(
                &mut self.machine_entry(machine).collapsed_worksets,
                id,
                collapsed,
            ),
        }
        true
    }

    fn prune(&mut self, machine: &RuntimeKey, known: &KnownIds) {
        self.project_order
            .retain(|key| key.machine != *machine || known.roots.contains(&key.root));
        let Some(ui) = self.machines.get_mut(machine) else {
            return;
        };
        ui.pinned_conversations
            .retain(|id| known.conversations.contains(id));
        ui.conversation_order
            .retain(|id| known.conversations.contains(id));
        ui.pinned_projects.retain(|root| known.roots.contains(root));
        ui.collapsed_projects
            .retain(|root| known.roots.contains(root));
        ui.pinned_worksets.retain(|id| known.worksets.contains(id));
        ui.collapsed_worksets
            .retain(|id| known.worksets.contains(id));
    }

    pub(crate) fn machine_entry(&mut self, machine: &RuntimeKey) -> &mut MachineUi {
        self.machines.entry(machine.clone()).or_default()
    }
}

fn toggle_listed(list: &mut Vec<String>, key: &str, on: bool) {
    if on {
        list.push(key.to_string());
    } else {
        list.retain(|item| item != key);
    }
}

fn toggle_set(set: &mut HashSet<String>, key: &str, on: bool) {
    if on {
        set.insert(key.to_string());
    } else {
        set.remove(key);
    }
}

/// 一台机器的目录里还在的东西，清理库里的行用。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KnownIds {
    pub(crate) conversations: HashSet<String>,
    /// 打开的项目，加上不在任何项目里的会话按工作目录自成的组。
    pub(crate) roots: HashSet<String>,
    pub(crate) worksets: HashSet<String>,
}

pub(crate) fn known_ids(style: PathStyle, directory: &WorkspaceDirectory) -> KnownIds {
    let mut roots: HashSet<String> = directory.projects.iter().cloned().collect();
    for conversation in &directory.conversations {
        if let Placement::Project { root, .. } = placement(style, directory, conversation) {
            roots.insert(root);
        }
    }
    KnownIds {
        conversations: directory
            .conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect(),
        roots,
        worksets: directory
            .worksets
            .iter()
            .map(|workset| workset.id.clone())
            .collect(),
    }
}

// ---- 选中、要露的会话在哪 ----

/// 选中的是什么。会话和项目只能选一个：选中会话时项目跟着它算（活动上下文），选项目是
/// 项目里还没有会话、或想先看项目时。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Selection {
    #[default]
    None,
    /// 一段会话，会话视图露的就是它。存库，下次启动恢复。
    Session(ConversationKey),
    /// 一个项目（按那台机器规整过的根目录）：点了项目行、刚打开的项目，文件、Git 这些按
    /// 它看。只在内存里，不存库。
    Project {
        machine: RuntimeKey,
        root: String,
        /// 刚打开它时那次调用回的目录版本：目录还没推到这一版时，目录里没有它不算项目
        /// 没了。点项目行选的（目录里本来就有）是 `None`。
        since: Option<WorkspaceRevision>,
    },
}

impl Selection {
    /// 选中的会话。
    pub(crate) fn session(&self) -> Option<&ConversationKey> {
        match self {
            Self::Session(key) => Some(key),
            Self::None | Self::Project { .. } => None,
        }
    }

    /// 选中的项目：机器、根目录。
    pub(crate) fn project(&self) -> Option<(&RuntimeKey, &str)> {
        match self {
            Self::Project { machine, root, .. } => Some((machine, root)),
            Self::None | Self::Session(_) => None,
        }
    }

    /// 选中的东西在这台机器上。
    pub(crate) fn on(&self, machine: &RuntimeKey) -> bool {
        match self {
            Self::None => false,
            Self::Session(key) => key.machine == *machine,
            Self::Project { machine: on, .. } => on == machine,
        }
    }
}

/// [`locate`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Located {
    /// 在，露这段会话。
    Found(ConversationKey),
    /// 目录还没推到给的版本，接着等下一次推送。
    Wait,
    /// 不在（或不在侧栏：归档了、自动化运行），也不用再等。
    Missing,
}

/// 要露的那段会话在 `machine` 的目录里找不找得到。`after` 是写目录那次回的版本：目录比它
/// 旧时找不到（或还是归档的）就接着等；没给、或目录已经推到了，找不到就是没有。还没收到
/// 目录的，给了 `after` 等，没给算没有。
pub(crate) fn locate(
    machine: &RuntimeKey,
    at: &SessionAt,
    after: Option<&WorkspaceRevision>,
    style: PathStyle,
    directory: Option<&WorkspaceDirectory>,
) -> Located {
    let Some(directory) = directory else {
        return if after.is_some() {
            Located::Wait
        } else {
            Located::Missing
        };
    };
    let found = match at {
        SessionAt::Session { session_id } => directory.conversation_of_session(session_id),
        SessionAt::Conversation { conversation_id } => directory
            .conversations
            .iter()
            .find(|conversation| conversation.id == *conversation_id),
    };
    if let Some(conversation) =
        found.filter(|conversation| placement(style, directory, conversation) != Placement::Hidden)
    {
        return Located::Found(ConversationKey {
            machine: machine.clone(),
            id: conversation.id.clone(),
        });
    }
    if after.is_some_and(|after| directory.version().is_older_than(after)) {
        Located::Wait
    } else {
        Located::Missing
    }
}

/// 侧栏里看得见的那条（归档的、自动化运行的不能选、不能露）。
pub(crate) fn shown_conversation<'a>(
    style: PathStyle,
    directory: &'a WorkspaceDirectory,
    id: &str,
) -> Option<&'a WorkspaceConversation> {
    directory.conversations.iter().find(|conversation| {
        conversation.id == id && placement(style, directory, conversation) != Placement::Hidden
    })
}

// ---- 从目录和会话状态算的 ----

/// 最近一次动静（unix 秒）：各格最近一次换阶段、开始一轮里最晚的。守护里还没有它的
/// 是 0。「对话」那一段按它排。
pub(crate) fn last_activity(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
) -> u64 {
    conversation
        .states_in(states)
        .map(|state| state.phase_since.max(state.turn_started_at))
        .max()
        .unwrap_or(0)
}

/// 给人看的名字：运行时算好的展示名（对话的、或第一格终端的），没有就用用户起的名字，
/// 再没有就是通用名。
pub(crate) fn title(conversation: &WorkspaceConversation, states: &[DaemonSessionState]) -> String {
    let lead = conversation
        .acp
        .as_ref()
        .map(|acp| acp.sid.as_str())
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|terminal| terminal.id.as_deref())
        });
    let named = lead
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.conversation_name.as_deref());
    let custom = conversation.custom_title.as_deref();
    match [named, custom]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
    {
        Some(name) => name.to_string(),
        None if conversation.acp.is_some() => t!("kind.conversation").into_owned(),
        None => t!("kind.terminal").into_owned(),
    }
}

/// 这段会话的工作目录：对话的，或第一格终端起的时候的。是起的时候定的，不随终端里
/// `cd` 变（那是会话状态里的 `cwd`），所以分组稳定。
pub(crate) fn cwd_of(conversation: &WorkspaceConversation) -> Option<&str> {
    let cwd = match &conversation.acp {
        Some(acp) => acp.cwd.as_deref(),
        None => conversation
            .terminals
            .first()
            .and_then(|terminal| terminal.cwd.as_deref()),
    };
    cwd.filter(|cwd| !cwd.is_empty())
}

/// 会话摆在侧栏哪儿。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// 在一个项目组里。`root` 是打开的项目，或者不在任何项目里时它自己的工作目录（自成
    /// 一组，没有工作目录的是 `""`）。`workset` 是它绑的工作集。
    Project {
        root: String,
        workset: Option<String>,
    },
    /// 「对话」那一段：跟智能体的对话，工作目录不在任何打开的项目里。
    Conversations,
    /// 不在侧栏：已归档（去归档视图看），或自动化的运行（去自动化看）。
    Hidden,
}

pub(crate) fn placement(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
) -> Placement {
    if conversation.archived_at.is_some() || is_automation_run(conversation) {
        return Placement::Hidden;
    }
    if let Some(workset) = conversation
        .workset_id
        .as_ref()
        .and_then(|id| directory.worksets.iter().find(|workset| &workset.id == id))
    {
        return Placement::Project {
            root: workset.project_root.clone(),
            workset: Some(workset.id.clone()),
        };
    }
    let cwd = cwd_of(conversation);
    if let Some(root) = cwd.and_then(|cwd| project_root_of(style, &directory.projects, cwd)) {
        return Placement::Project {
            root: root.to_string(),
            workset: None,
        };
    }
    if conversation.agent_definition().is_some() {
        return Placement::Conversations;
    }
    Placement::Project {
        root: cwd.unwrap_or_default().to_string(),
        workset: None,
    }
}

/// 自动化跑出来的会话：智能体对话绑着自动化，或者是后台起的 ACP 会话。
fn is_automation_run(conversation: &WorkspaceConversation) -> bool {
    conversation
        .acp
        .as_ref()
        .is_some_and(|acp| acp.automation_id.is_some() || is_background_acp_session_id(&acp.sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::daemon_state::DaemonPhase;
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset,
    };

    fn terminal_session(id: &str, terminal: &str, cwd: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: vec![WorkspaceTerminal {
                id: Some(terminal.into()),
                cwd: Some(cwd.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn acp_session(id: &str, sid: &str, cwd: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(WorkspaceAcp {
                sid: sid.into(),
                cwd: cwd.map(Into::into),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn state(id: &str, phase: DaemonPhase, since: u64) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            phase,
            phase_since: since,
            turn_events: true,
            ..Default::default()
        }
    }

    fn key(machine: RuntimeKey, id: &str) -> ConversationKey {
        ConversationKey {
            machine,
            id: id.into(),
        }
    }

    #[test]
    fn avatar_choice_picks_among_the_fillers() {
        let filled: Vec<SharedString> = vec!["a.x/avatar".into(), "b.y/avatar".into()];
        let picked = |choice: AvatarChoice| choice.pick(&filled).map(|id| id.to_string());
        assert_eq!(picked(AvatarChoice::Unset).as_deref(), Some("a.x/avatar"));
        assert_eq!(picked(AvatarChoice::Icon), None);
        assert_eq!(
            picked(AvatarChoice::View("b.y/avatar".into())).as_deref(),
            Some("b.y/avatar")
        );
        // 选的那个卸了按没选过算。
        assert_eq!(
            picked(AvatarChoice::View("gone/avatar".into())).as_deref(),
            Some("a.x/avatar")
        );
        // 没人填，怎么选都是图标。
        assert_eq!(AvatarChoice::Unset.pick(&[]), None);
        assert_eq!(AvatarChoice::View("b.y/avatar".into()).pick(&[]), None);
    }

    #[test]
    fn activity_comes_from_the_daemon_only() {
        let mut conversation = terminal_session("c", "t1", "/repo");
        conversation.terminals.push(WorkspaceTerminal {
            id: Some("t2".into()),
            ..Default::default()
        });
        let states = [
            state("t1", DaemonPhase::Thinking, 100),
            state("t2", DaemonPhase::AwaitingApproval, 50),
            state("other", DaemonPhase::Failed, 900),
        ];
        assert_eq!(last_activity(&conversation, &states), 100);
        assert_eq!(last_activity(&conversation, &[]), 0);
    }

    #[test]
    fn title_prefers_runtime_name_then_custom_then_generic() {
        let mut conversation = acp_session("c", "a1", None);
        assert_eq!(title(&conversation, &[]), "Conversation");
        conversation.custom_title = Some("我起的".into());
        assert_eq!(title(&conversation, &[]), "我起的");
        let mut named = state("a1", DaemonPhase::Idle, 0);
        named.conversation_name = Some("运行时的".into());
        assert_eq!(title(&conversation, &[named]), "运行时的");
        assert_eq!(title(&terminal_session("t", "t1", "/"), &[]), "Terminal");
    }

    #[test]
    fn placement_groups_by_workset_project_agent_then_cwd() {
        let style = PathStyle::Posix;
        let mut in_workset = terminal_session("w", "t0", "/elsewhere");
        in_workset.workset_id = Some("ws".into());
        let in_project = terminal_session("p", "t1", "/repo/sub");
        let mut agent_in_project = acp_session("ap", "a0", Some("/repo"));
        agent_in_project.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let mut agent_space = acp_session("a", "a1", Some("/home/u/.smelt/agents/writer"));
        agent_space.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let loose = terminal_session("l", "t2", "/tmp/x");
        let mut no_cwd = terminal_session("n", "t3", "");
        no_cwd.terminals[0].cwd = None;
        let mut archived = terminal_session("old", "t4", "/repo");
        archived.archived_at = Some(1);
        let mut automation = acp_session("auto", "a2", Some("/repo"));
        automation.acp.as_mut().unwrap().automation_id = Some("nightly".into());
        let background = acp_session("bg", "acp-automation-1", Some("/repo"));
        let directory = WorkspaceDirectory {
            projects: vec!["/repo".into()],
            worksets: vec![WorkspaceWorkset {
                id: "ws".into(),
                project_root: "/repo".into(),
                ..Default::default()
            }],
            conversations: vec![
                in_workset,
                in_project,
                agent_in_project,
                agent_space,
                loose,
                no_cwd,
                archived,
                automation,
                background,
            ],
            ..Default::default()
        };
        let placed: Vec<Placement> = directory
            .conversations
            .iter()
            .map(|conversation| placement(style, &directory, conversation))
            .collect();
        let project = |root: &str, workset: Option<&str>| Placement::Project {
            root: root.into(),
            workset: workset.map(Into::into),
        };
        assert_eq!(
            placed,
            [
                project("/repo", Some("ws")),
                project("/repo", None),
                project("/repo", None),
                Placement::Conversations,
                project("/tmp/x", None),
                project("", None),
                Placement::Hidden,
                Placement::Hidden,
                Placement::Hidden,
            ]
        );

        let known = known_ids(style, &directory);
        assert!(known.roots.contains("/repo") && known.roots.contains("/tmp/x"));
        assert!(known.worksets.contains("ws"));
        assert!(known.conversations.contains("auto"));
    }

    #[test]
    fn locating_waits_only_until_the_written_revision() {
        let remote = RuntimeKey::Remote("box".into());
        let style = PathStyle::Posix;
        let revision = |revision| WorkspaceRevision {
            epoch: "e".into(),
            revision,
        };
        let mut directory = WorkspaceDirectory {
            epoch: "e".into(),
            revision: 3,
            conversations: vec![terminal_session("c", "t1", "/repo")],
            ..Default::default()
        };
        let by_session = SessionAt::Session {
            session_id: "t1".into(),
        };
        let by_id = |id: &str| SessionAt::Conversation {
            conversation_id: id.into(),
        };
        let found = Located::Found(key(remote.clone(), "c"));

        assert_eq!(
            locate(&remote, &by_session, None, style, Some(&directory)),
            found
        );
        assert_eq!(
            locate(
                &remote,
                &by_id("c"),
                Some(&revision(3)),
                style,
                Some(&directory)
            ),
            found
        );
        // 刚写进去的：目录推到那一版之前接着等，推到了还没有就是没有。
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&revision(4)),
                style,
                Some(&directory)
            ),
            Located::Wait
        );
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&revision(3)),
                style,
                Some(&directory)
            ),
            Located::Missing
        );
        assert_eq!(
            locate(&remote, &by_id("new"), None, style, Some(&directory)),
            Located::Missing
        );
        // 运行时的库换过（epoch 不同）谈不上新旧，不等。
        let other = WorkspaceRevision {
            epoch: "f".into(),
            revision: 9,
        };
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&other),
                style,
                Some(&directory)
            ),
            Located::Missing
        );
        // 还没收到目录。
        assert_eq!(
            locate(&remote, &by_id("c"), Some(&revision(1)), style, None),
            Located::Wait
        );
        assert_eq!(
            locate(&remote, &by_id("c"), None, style, None),
            Located::Missing
        );

        // 归档的不在侧栏：刚恢复、目录还没推到时等，推到了还是归档的就是没有。
        directory.conversations[0].archived_at = Some(1);
        assert_eq!(
            locate(
                &remote,
                &by_session,
                Some(&revision(4)),
                style,
                Some(&directory)
            ),
            Located::Wait
        );
        assert_eq!(
            locate(&remote, &by_session, None, style, Some(&directory)),
            Located::Missing
        );
        assert_eq!(shown_conversation(style, &directory, "c"), None);
    }

    #[test]
    fn selection_is_a_session_or_a_project() {
        let remote = RuntimeKey::Remote("box".into());
        let session = Selection::Session(key(remote.clone(), "c"));
        assert_eq!(session.session(), Some(&key(remote.clone(), "c")));
        assert_eq!(session.project(), None);
        assert!(session.on(&remote) && !session.on(&RuntimeKey::Local));

        let project = Selection::Project {
            machine: RuntimeKey::Local,
            root: "/repo".into(),
            since: None,
        };
        assert_eq!(project.session(), None);
        assert_eq!(project.project(), Some((&RuntimeKey::Local, "/repo")));
        assert!(project.on(&RuntimeKey::Local));
        assert!(!Selection::None.on(&RuntimeKey::Local));
    }

    #[test]
    fn sidebar_ui_mirrors_the_store_rules() {
        let mut ui = SidebarUi::default();
        let a = Pin::Conversation(key(RuntimeKey::Local, "a"));
        let b = Pin::Conversation(key(RuntimeKey::Local, "b"));
        assert!(ui.set_pinned(&b, true));
        assert!(ui.set_pinned(&a, true));
        assert!(!ui.set_pinned(&a, true));
        assert_eq!(
            ui.machines[&RuntimeKey::Local].pinned_conversations,
            ["b", "a"]
        );
        assert!(ui.set_pinned(&b, false));
        assert!(!ui.is_pinned(&b));

        let fold = Fold::Workset {
            machine: RuntimeKey::Local,
            id: "w".into(),
        };
        assert!(ui.set_collapsed(&fold, true));
        assert!(ui.is_collapsed(&fold));
        assert!(ui.set_collapsed(&Fold::Conversations, true));
        assert!(ui.is_collapsed(&Fold::Conversations));

        ui.prune(
            &RuntimeKey::Local,
            &KnownIds {
                conversations: HashSet::from(["a".to_string()]),
                ..Default::default()
            },
        );
        let local = &ui.machines[&RuntimeKey::Local];
        assert_eq!(local.pinned_conversations, ["a"]);
        assert!(local.collapsed_worksets.is_empty());
    }

    #[test]
    fn folds_over_a_session_follow_its_placement_and_grouping() {
        let machine = RuntimeKey::Remote("box".into());
        let in_workset = || Placement::Project {
            root: "/repo".into(),
            workset: Some("w".into()),
        };
        assert_eq!(
            folds_over(&machine, in_workset(), SidebarGrouping::Project),
            [
                Fold::Project {
                    machine: machine.clone(),
                    root: "/repo".into(),
                },
                Fold::Workset {
                    machine: machine.clone(),
                    id: "w".into(),
                },
            ]
        );
        let loose = Placement::Project {
            root: "/repo".into(),
            workset: None,
        };
        assert_eq!(
            folds_over(&machine, loose, SidebarGrouping::Project),
            [Fold::Project {
                machine: machine.clone(),
                root: "/repo".into(),
            }]
        );
        // 按状态、时间分组时不画项目、工作集，不去改它们存着的折叠。
        assert!(folds_over(&machine, in_workset(), SidebarGrouping::Status).is_empty());
        for grouping in [SidebarGrouping::Project, SidebarGrouping::LastUpdated] {
            assert_eq!(
                folds_over(&machine, Placement::Conversations, grouping),
                [Fold::Conversations]
            );
            assert!(folds_over(&machine, Placement::Hidden, grouping).is_empty());
        }
    }
}
