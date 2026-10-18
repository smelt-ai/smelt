//! 会话列表：窗口最左的单列，**按项目上下组织**——项目是一级标题行，
//! 它的会话缩进排在下面，一屏看全所有项目的所有会话（不必先切项目）。
//!
//! 设计稿原本是「64px 项目 rail + 270px 会话列」的左右两列，实测割裂：项目在
//! rail 上只剩一个字母，且必须先点中某个项目才看得到它的会话。改回单列分组。
//!
//! 日常扫的是「某个项目里的对话」。会话缩进一档挂在项目下面，并用极淡竖线提示
//! 从属关系；当前会话可见时独占强选中底色，活动项目只用文字亮度提示上下文。
//! 空项目只把字降淡。顶部可筛掉无会话
//! 项目（固定的和当前项目除外）。右键固定的提到最前。
//! 没有项目头像。会话只用 agent 小图标。`+` 仍在项目行 hover 出现。
//! 按项目分组时可整组/整行拖拽排序：项目改 `projects`，组内会话改 `sessions`，
//! 都通过 `save_state` 落盘。
//!
//! 跟 file_tree 模块（`crates/smelt/src/file_tree/`）同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use crate::{ProjectRef, RuntimeKey};
use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
use smelt_ui::IconAction as _;
use smelt_ui::motion::{ambient_animation, flip_slot};
use std::time::Duration;

// drag.rs / row.rs 作为本模块子模块沿用 `super::*` 读取 ACP 类型。
use crate::settings::{AgentHostState, ConversationAgentKind};
use crate::{
    AgentStatus, RenameTarget, SessionKind, SidebarGrouping, Workspace, pane_provider_kind,
    pane_status, pane_title, ui_theme,
};

/// 项目标题行的 hover group 名：整行 `.group()` + 左端文件夹 `.group_hover()`。
/// 整行点击展开/折叠；文件夹只表达项目身份和开合状态，hover 整行时同步提亮。
const PROJ_HEADER_GROUP: &str = "proj-header-hover";

/// 「项目」分组标题行：筛选和打开项目只在 hover 这行时出现，跟 Grok 一样。
const PROJ_SECTION_GROUP: &str = "proj-section-hover";

/// 会话行：关闭按钮只在 hover 这行时出现。
const SESSION_ROW_GROUP: &str = "session-row-hover";

/// 侧栏正文走 `text_sm`，辅助信息走 `text_xs`，跟着界面字号缩放。
/// 层级靠字重和颜色，不靠放大字。
///
/// 项目标题与会话列表之间的固定层级尺寸。集中声明是为了避免项目头、会话缩进和
/// 新建按钮各自改尺寸后失去对齐关系。
const PROJECT_NEW_BUTTON_SIZE: f32 = 28.;
const PROJECT_ACTION_SLOT_HEIGHT: f32 = 20.;
const PROJECT_DISCLOSURE_ICON_SIZE: f32 = 15.;
/// 项目行左内边距。加上分组外边距，文件夹图标跟侧栏入口图标、分组标题落在同一条左缘。
const PROJECT_HEADER_PAD_X: f32 = 8.;
/// 会话图标落在项目名正下方。
const PROJECT_SESSION_INDENT: f32 = 18.;
/// 引导线对着文件夹图标的中线。
const PROJECT_GUIDE_LEFT: f32 = 14.;
const SESSION_AGENT_ICON_BOX_SIZE: f32 = 18.;
const SESSION_AGENT_ICON_SIZE: f32 = 17.;
pub(crate) const SESSION_GLOW_PERIOD: Duration = Duration::from_secs(2);

/// 项目组的重排过渡跟着根路径走。行 id 也用根路径，重排后不会把 hover 留在旧槽位。
fn project_flip_id(project: &ProjectRef) -> ElementId {
    project_element_id("sidebar-flip-project", project)
}

/// 两个运行时上可以有同一个路径，元素身份要带上运行时。
fn project_element_id(name: &'static str, project: &ProjectRef) -> ElementId {
    let key = match &project.runtime {
        RuntimeKey::Local => project.root.clone(),
        RuntimeKey::Remote(id) => format!("{id}:{}", project.root),
    };
    (ElementId::from(name), key).into()
}

/// 侧栏里一段运行时的标题：名字、在线状态，hover 出「在这台机器上打开项目」。
fn runtime_section_header(
    runtime: crate::runtimes::Runtime,
    workspace: Entity<Workspace>,
    cx: &App,
) -> Stateful<Div> {
    let key = runtime.key().clone();
    let name = crate::runtimes::runtime_name(&key, cx);
    let unavailable = crate::runtimes::runtime_unavailable_label(&key, cx);
    let group = SharedString::from(format!("runtime-header-{key:?}"));
    div()
        .id(ElementId::from(group.clone()))
        .group(group.clone())
        .mx_2()
        .mt_3()
        .px_3()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(rgb(ui_theme::text_faint()))
        .child(
            div()
                .size(px(6.))
                .rounded_full()
                .bg(rgb(if unavailable.is_none() {
                    ui_theme::session_dot_color(AgentStatus::Idle).into()
                } else {
                    ui_theme::text_faint()
                })),
        )
        .child(name)
        .when_some(unavailable, |d, label| d.child(format!("· {label}")))
        .child(div().flex_1())
        .child({
            let runtime = runtime.clone();
            let workspace = workspace.clone();
            Button::new(ElementId::from(SharedString::from(format!(
                "runtime-browser-{key:?}"
            ))))
            .ghost()
            .xsmall()
            .icon(IconName::Globe)
            .icon_action("浏览器")
            .opacity(0.0)
            .group_hover(group.clone(), |style| style.opacity(1.0))
            .on_click(move |_ev, window, cx| {
                let runtime = runtime.clone();
                workspace.update(cx, |ws, cx| ws.open_browser(runtime, window, cx));
            })
        })
        .child(
            Button::new(ElementId::from(SharedString::from(format!(
                "runtime-open-project-{key:?}"
            ))))
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .icon_action("打开项目")
            .opacity(0.0)
            .group_hover(group, |style| style.opacity(1.0))
            .on_click(move |_ev, _window, cx| {
                let runtime = runtime.clone();
                workspace.update(cx, |ws, cx| ws.open_project_on(runtime, cx));
            }),
        )
}

/// 远程项目的右键菜单：复制路径、置顶、关闭。
fn remote_project_menu(
    menu: PopupMenu,
    project: ProjectRef,
    session_count: usize,
    workspace: Entity<Workspace>,
    cx: &App,
) -> PopupMenu {
    let pinned = workspace.read(cx).is_project_pinned(&project);
    let copy_path = project.root.clone();
    let pin_project = project.clone();
    let pin_workspace = workspace.clone();
    menu.item(
        PopupMenuItem::new("复制项目路径")
            .icon(IconName::Copy)
            .on_click(move |_ev, _window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(IconName::Star)
            .on_click(move |_ev, _window, cx| {
                let project = pin_project.clone();
                pin_workspace.update(cx, |ws, cx| ws.toggle_project_pinned(project, cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(if session_count > 0 {
            format!("关闭项目（含 {session_count} 个会话）")
        } else {
            "关闭项目".to_string()
        })
        .icon(IconName::Close)
        .on_click(move |_ev, _window, cx| {
            let project = project.clone();
            workspace.update(cx, |ws, cx| ws.start_close_project(project, cx));
        }),
    )
}

fn project_disclosure_icon(collapsed_or_empty: bool) -> IconName {
    if collapsed_or_empty {
        IconName::Folder
    } else {
        IconName::FolderOpen
    }
}

mod drag;
mod new_session;

use drag::{
    ProjectDrag, SessionDrag, attach_project_drop_layers, attach_session_drop_layers,
    with_project_drag, with_session_drag,
};

pub(crate) mod row;

use new_session::new_session_picker;
use row::{
    acp_provider_icon, effective_pane_updated_at, project_header_is_selected, provider_icon,
    provider_quota_row, session_progress_bar, session_row_is_selected, session_row_status_label,
    session_updated_at_text, status_text,
};

/// 工具面板（文件树 / 变更 / 历史 / 技能 / 插件）绑定的上下文根。
///
/// 抽成纯函数是因为整个 `Workspace` 在测试里起不来，而这段优先级恰恰是会被
/// 误改的地方：智能体对话的托管工作目录不进项目分组，如果它压不过「上一个
/// 打开的项目」，工具面板就会停在某个不相干的仓库上。
///
/// - `selected`：用户最后一次选定的上下文（点项目行或打开对话都写它）
/// - `selected_group_root`：`selected` 命中的项目组 root
/// - `selected_is_open_conversation`：`selected` 是某段仍开着的对话的工作目录
/// - `active_session_group_root` / `first_group_root`：选定项已失效时的回退
pub(crate) fn tool_panel_context_root(
    selected: Option<&str>,
    selected_group_root: Option<&str>,
    selected_is_open_conversation: bool,
    active_session_group_root: Option<&str>,
    first_group_root: Option<&str>,
) -> Option<String> {
    if let Some(root) = selected_group_root {
        return Some(root.to_string());
    }
    if selected_is_open_conversation && let Some(root) = selected {
        return Some(root.to_string());
    }
    active_session_group_root
        .or(first_group_root)
        .map(str::to_string)
}

impl Workspace {
    /// 当前活动的项目：优先用用户点选的 `active_project`，该项目已被关掉则回退到活动
    /// 会话所在组，再回退第一组，都在选中项目（没有就是活动会话）所在的运行时里找。
    ///
    /// 打开智能体对话时会把它的工作目录写进 `active_project`，于是「点项目行」
    /// 与「开对话」共用同一个上下文变量，后操作的那个生效。
    pub(crate) fn active_project_ref(&self, cx: &App) -> Option<ProjectRef> {
        let runtime = self
            .active_project
            .as_ref()
            .map(|project| project.runtime.clone())
            .or_else(|| self.cur().map(|session| session.runtime(cx).key().clone()))
            .unwrap_or(RuntimeKey::Local);
        let groups = self.project_groups(&runtime, cx);
        let selected = self
            .active_project
            .as_ref()
            .filter(|project| project.runtime == runtime)
            .map(|project| project.root.as_str());
        let selected_group_root = selected.and_then(|root| {
            groups
                .iter()
                .find(|g| crate::sidebar_order::same_project_root(&g.root, root))
                .map(|g| g.root.as_str())
        });
        // 对话关掉或目录已被清理时这里不再命中，自然回退到项目分组，
        // 不会把工具面板钉死在一个不存在的路径上。
        let selected_is_open_conversation =
            selected.is_some_and(|root| self.agent_conversation_workspace_is_open(root, cx));
        tool_panel_context_root(
            selected,
            selected_group_root,
            selected_is_open_conversation,
            groups
                .iter()
                .find(|g| g.sessions.contains(&self.active_session))
                .map(|g| g.root.as_str()),
            groups.first().map(|g| g.root.as_str()),
        )
        .map(|root| ProjectRef { runtime, root })
    }

    /// 工具面板（文件树、变更、历史……）绑定的本机项目根。面板目前只读本机文件，活动
    /// 项目在远程运行时上就没有。
    pub(crate) fn active_project_root(&self, cx: &App) -> Option<String> {
        self.active_project_ref(cx)
            .filter(|project| project.runtime == RuntimeKey::Local)
            .map(|project| project.root)
    }

    /// `root` 是否是某段仍然开着的智能体对话的工作目录。
    fn agent_conversation_workspace_is_open(&self, root: &str, cx: &App) -> bool {
        smelt_model::session_control::is_agent_conversation_cwd(root)
            && self.sessions.iter().any(|session| {
                session.is_agent_conversation(cx)
                    && session
                        .cwd(cx)
                        .is_some_and(|cwd| crate::sidebar_order::same_project_root(&cwd, root))
            })
    }

    /// 会话行的 agent 图标颜色：空闲/要你走默认三态色；运行中进入时短暂提亮。
    ///
    /// 过渡只在 Running 时生效（见 `render_session_list` 图标处注释）：短暂提高基准蓝
    /// 的亮度，而不是整体淡入淡出——图标同时承担「哪家 agent」的身份，淡掉会伤
    /// 识别。过渡结束或非运行时回落静态状态色。
    pub(crate) fn session_icon_color(status: AgentStatus, breath_phase: Option<f32>) -> gpui::Rgba {
        match (status, breath_phase) {
            (AgentStatus::Running, Some(phase)) => ui_theme::running_glow_color(phase),
            _ => ui_theme::session_dot_color(status),
        }
    }

    /// 280px 会话列表（全部项目，按项目分组）。
    pub(crate) fn render_session_list(
        &self,
        this: Entity<Workspace>,
        cx: &App,
        statuses: &[AgentStatus],
        animate_running: bool,
    ) -> Div {
        debug_assert_eq!(statuses.len(), self.sessions.len());
        let active = self.active_session;
        // 注：项目实体化后允许关到一个会话都不剩（侧栏还有项目行撑着，舞台落到引导页），
        // 所以关闭键不再有「最后一个不许关」那道门槛，全都常显。
        let active_project = self.active_project_ref(cx);

        let titles: Vec<(usize, String)> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(ix, s)| (ix, s.title(cx)))
            .collect();
        let last_updated_at: Vec<u64> = self
            .sessions
            .iter()
            .map(|s| s.effective_updated_at(cx))
            .collect();
        let now = Local::now();
        // 非项目分组会拿掉项目标题，因此把每个会话所属项目的标签摊到行内展示，
        // 否则同名会话按状态混在一起后完全看不出来自哪个项目。
        let mut session_projects = vec![None; self.sessions.len()];
        // 各运行时依次铺开，每段内按当前分组方式分组。
        let mut groups = Vec::new();
        for key in self.space_keys() {
            let Some(space) = self.space(&key) else {
                continue;
            };
            let project_groups = self.project_groups(&key, cx);
            for group in &project_groups {
                // 仓库信息目前只有本机项目有。
                let branch = self
                    .repo_info
                    .get(group.root.as_str())
                    .filter(|_| key == RuntimeKey::Local)
                    .and_then(|(_, info)| info.as_ref())
                    .map(|info| info.branch.clone());
                for &ix in &group.sessions {
                    session_projects[ix] = Some((group.label.clone(), branch.clone()));
                }
            }
            let members = self.space_members(&key, cx);
            let mut space_groups = crate::sidebar_groups(
                &key,
                self.sidebar_grouping,
                project_groups,
                statuses,
                &last_updated_at,
                &members,
            );
            if self.sidebar_grouping == SidebarGrouping::Project {
                space_groups = crate::sidebar_order::pinned_projects_first(
                    space_groups,
                    &space.pinned_projects,
                    |group| group.root.as_str(),
                );
            }
            groups.extend(space_groups);
        }
        let session_route_active = self.active_tab().is_session();

        // ---- 头部：工作区标题 + 分组设置 ----
        // 新建入口全撤：建会话一律走「项目行 hover 出的 +」（落到那个项目）；
        // 历史是右侧 Tool Panel 的项目级面板。
        let e_group = this.clone();
        let grouping = self.sidebar_grouping;
        let hide_empty = self.sidebar_hide_empty_projects;
        // 只有「按项目」时顺序才是手动序；按状态/时间分组是派生的，拖了也对不上。
        let can_drag = grouping == SidebarGrouping::Project;
        let dragging = can_drag && self.sidebar_drag.is_some();
        let dragging_session = match self.sidebar_drag {
            Some(crate::SidebarDrag::Session(id)) => Some(id),
            _ => None,
        };
        let dragging_project = match &self.sidebar_drag {
            Some(crate::SidebarDrag::Project(project)) => Some(project.clone()),
            _ => None,
        };
        // 筛选 / 分组收进「项目」行右侧的滑块菜单，参考 Copilot Repositories 头。
        let grouping_btn = div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("session-grouping")
                    .ghost()
                    .size(px(24.))
                    .icon(IconName::Settings2)
                    .icon_action("筛选与分组")
                    .dropdown_menu(move |menu, _window, _cx| {
                        let menu = menu.item(PopupMenuItem::label("分组依据"));
                        let add_group =
                            |menu: PopupMenu,
                             mode: SidebarGrouping,
                             label: &'static str,
                             entity: Entity<Workspace>| {
                                let mut entry = PopupMenuItem::new(label);
                                if grouping == mode {
                                    entry = entry.icon(IconName::Check);
                                }
                                menu.item(entry.on_click(move |_ev, _window, cx| {
                                    entity.update(cx, |ws, cx| {
                                        ws.sidebar_grouping = mode;
                                        ws.save_state(cx);
                                        cx.notify();
                                    });
                                }))
                            };
                        let menu =
                            add_group(menu, SidebarGrouping::Project, "项目", e_group.clone());
                        let menu =
                            add_group(menu, SidebarGrouping::Status, "状态", e_group.clone());
                        let menu =
                            add_group(menu, SidebarGrouping::LastUpdated, "时间", e_group.clone());
                        let menu = menu.separator().item(PopupMenuItem::label("显示"));
                        let add_show =
                            |menu: PopupMenu,
                             hide: bool,
                             label: &'static str,
                             entity: Entity<Workspace>| {
                                let mut entry = PopupMenuItem::new(label);
                                if hide_empty == hide {
                                    entry = entry.icon(IconName::Check);
                                }
                                menu.item(entry.on_click(move |_ev, _window, cx| {
                                    entity.update(cx, |ws, cx| {
                                        if ws.sidebar_hide_empty_projects != hide {
                                            ws.sidebar_hide_empty_projects = hide;
                                            ws.save_state(cx);
                                            cx.notify();
                                        }
                                    });
                                }))
                            };
                        let menu = add_show(menu, false, "全部", e_group.clone());
                        add_show(menu, true, "有会话", e_group.clone())
                    }),
            );

        // 「项目」分组标题跟「最近对话」一起放进滚动区：两段列表共用一个滚动条，
        // 对话再多也只是往下推，不会把项目列表压没。
        let e_open = this.clone();
        let project_section_header = div()
            .id("project-section-header")
            .group(PROJ_SECTION_GROUP)
            .flex_shrink_0()
            .child(crate::workspace_frame::with_window_drag(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .px_1()
                            .flex_shrink_0()
                            .text_xs()
                            .font_medium()
                            .text_color(rgb(ui_theme::text_muted()))
                            .child("项目"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .opacity(0.0)
                            .group_hover(PROJ_SECTION_GROUP, |s| s.opacity(1.0))
                            .child(grouping_btn)
                            .child(
                                div()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        Button::new("open-project")
                                            .ghost()
                                            .size(px(24.))
                                            .icon(Icon::empty().path("smelt-icons/folder-plus.svg"))
                                            .icon_action("添加项目")
                                            .on_click(move |_ev, _window, cx| {
                                                e_open.update(cx, |ws, cx| ws.open_project(cx));
                                            }),
                                    ),
                            ),
                    ),
            ));

        let e_scroll_sess = this.clone();
        let e_scroll_proj = this.clone();
        // 滚动条必须叠在滚动容器外面：若作为 flex_col 的子节点，即使 absolute
        // 也会把内容高度顶出视口，列表明明放得下也会画出一条灰杠。
        let mut rows = div()
            .id("session-rows")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .track_scroll(&self.sidebar_scroll)
            .lock_scroll_axis()
            .on_drag_move(move |ev: &DragMoveEvent<SessionDrag>, _window, cx| {
                e_scroll_sess.update(cx, |ws, cx| {
                    ws.update_sidebar_drag_scroll(ev.event.position, cx);
                });
            })
            .on_drag_move(move |ev: &DragMoveEvent<ProjectDrag>, _window, cx| {
                e_scroll_proj.update(cx, |ws, cx| {
                    ws.update_sidebar_drag_scroll(ev.event.position, cx);
                });
            })
            .child(self.render_product_conversation_nav(this.clone(), statuses, cx))
            .child(project_section_header);

        // 拖拽排序生效后，项目组和会话行都从旧位置滑到新位置。
        let reorder_generation = self.sidebar_reorder_generation;
        // 被拖的那一项从松手时拖拽浮层的高度落到新位置，其余各项从旧位置滑过去。
        let drop_origin = |item: crate::SidebarDrag| {
            self.sidebar_drop_origin
                .as_ref()
                .filter(|(dropped, _)| *dropped == item)
                .map(|(_, top)| *top)
        };
        let mut hidden_empty = 0usize;
        let mut visible_ix = 0usize;
        // ---- 运行时标题行：挂了远程运行时才分段 ----
        // 按空间顺序出，还没有分组的运行时（刚添加、还没开项目）也要有标题，它的
        // 「打开项目」就在上面。
        let mut headers_left: std::collections::VecDeque<crate::runtimes::Runtime> =
            if self.remote_spaces.is_empty() {
                Default::default()
            } else {
                self.space_keys()
                    .iter()
                    .filter_map(|key| self.space(key).map(|space| space.runtime.clone()))
                    .collect()
            };
        for group in groups.iter() {
            let Some(space) = self.space(&group.runtime) else {
                continue;
            };
            if headers_left
                .iter()
                .any(|runtime| runtime.key() == &group.runtime)
            {
                while let Some(runtime) = headers_left.pop_front() {
                    let reached = runtime.key() == &group.runtime;
                    rows = rows.child(runtime_section_header(runtime, this.clone(), cx));
                    if reached {
                        break;
                    }
                }
            }
            let is_project_group = grouping == SidebarGrouping::Project;
            // ---- 项目分组标题行 ----
            // 分组身份一律用运行时加 root 路径（末段同名的两个目录是两个项目，见
            // ProjectGroup）；name 只是显示用的。
            let cwd = &group.root;
            let project = ProjectRef {
                runtime: group.runtime.clone(),
                root: group.root.clone(),
            };
            let is_local_group = group.runtime == RuntimeKey::Local;
            let name = &group.label;
            let ixs = &group.sessions;
            let collapsed = space.collapsed_projects.contains(cwd);
            let is_active_group = active_project.as_ref().is_some_and(|active| {
                active.runtime == group.runtime
                    && crate::sidebar_order::same_project_root(cwd, &active.root)
            });
            let empty_group = ixs.is_empty();
            let is_pinned = is_project_group && self.is_project_pinned(&project);
            if is_project_group
                && !crate::sidebar_order::sidebar_empty_project_visible(
                    hide_empty,
                    ixs.len(),
                    is_pinned,
                    is_active_group,
                )
            {
                hidden_empty += 1;
                continue;
            }
            let active_session_is_visible =
                session_route_active && !collapsed && ixs.contains(&active);
            let project_header_selected = project_header_is_selected(
                session_route_active,
                is_active_group,
                active_session_is_visible,
            );
            // 组内最高优先级状态（声明序即优先级）当聚合状态点；折叠时尤其有用。
            let agg = ixs
                .iter()
                .filter_map(|&i| statuses.get(i).copied())
                .min_by_key(|s| s.rank())
                .unwrap_or(AgentStatus::Idle);

            // 分支名：git 仓库都显示，跟项目名并排（淡色）。
            let branch_label = self
                .repo_info
                .get(cwd.as_str())
                .filter(|_| is_local_group)
                .and_then(|(_, info)| info.as_ref())
                .map(|info| info.branch.clone());

            let e_toggle = this.clone();
            let toggle_project = project.clone();
            let e_menu = this.clone();
            let menu_runtime = space.runtime.clone();
            let menu_cwd = if is_project_group {
                cwd.clone()
            } else {
                active_project
                    .as_ref()
                    .filter(|active| active.runtime == group.runtime)
                    .map(|active| active.root.clone())
                    .unwrap_or_default()
            };
            let group_name: SharedString = name.clone().into();
            let is_drag_source_project = dragging_project.as_ref().is_some_and(|dragged| {
                dragged.runtime == project.runtime
                    && crate::sidebar_order::normalize_project_root(&dragged.root)
                        == crate::sidebar_order::normalize_project_root(cwd)
            });
            let proj_hint_before = can_drag
                && !is_drag_source_project
                && self.proj_drop_hint.as_ref() == Some(&(project.clone(), true));
            let proj_hint_after = can_drag
                && !is_drag_source_project
                && self.proj_drop_hint.as_ref() == Some(&(project.clone(), false));
            let mut group_rows = div()
                .id(project_element_id("proj-group-rows", &project))
                .flex()
                .flex_col()
                .when(is_drag_source_project, |d| d.opacity(0.4))
                .when(grouping != SidebarGrouping::None, |d| {
                    d.mx_2().when(visible_ix > 0, |d| d.mt_2())
                });
            visible_ix += 1;
            if grouping != SidebarGrouping::None {
                group_rows = group_rows.child(
                    div()
                        .id(project_element_id("proj-group", &project))
                        // relative：右端的 + 浮层靠 absolute 定位到这行内。
                        .relative()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px(px(PROJECT_HEADER_PAD_X))
                        .py(px(4.))
                        .rounded(ui_theme::row_radius())
                        .group(PROJ_HEADER_GROUP)
                        .map(|d| {
                            if project_header_selected {
                                d.bg(rgb(ui_theme::bg_selected()))
                            } else {
                                d.hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                            }
                        })
                        .child(
                            div()
                                .id(project_element_id("proj-disclosure", &project))
                                .w(px(14.))
                                .flex_shrink_0()
                                .flex()
                                .justify_center()
                                .text_color(rgb(ui_theme::text_muted()))
                                .group_hover(PROJ_HEADER_GROUP, |s| {
                                    s.text_color(rgb(ui_theme::text_bright()))
                                })
                                .child(
                                    Icon::new(project_disclosure_icon(collapsed || empty_group))
                                        .size(px(PROJECT_DISCLOSURE_ICON_SIZE)),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                // 项目名、插件智能体标识和分支名都放在同一行，统一按
                                // 几何中心对齐；图标不能跟着小字号文字的 baseline 漂移。
                                .items_center()
                                .gap_1p5()
                                .overflow_hidden()
                                .child(
                                    // 项目名与会话同为 text_sm，再用 semibold 区分结构层级；
                                    // 空项目降一档到 muted，不和活跃项目抢注意力。不降到 faint：
                                    // 它仍是能点、能新建会话的条目，faint 读起来像禁用。
                                    div()
                                        .flex_shrink_0()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(rgb(if is_active_group {
                                            ui_theme::text_bright()
                                        } else if crate::sidebar_order::project_header_is_dimmed(
                                            is_active_group,
                                            empty_group,
                                        ) {
                                            ui_theme::text_muted()
                                        } else {
                                            ui_theme::text_mid()
                                        }))
                                        .child(group_name.clone()),
                                )
                                .when(is_pinned, |row| {
                                    row.child(
                                        div()
                                            .id(project_element_id("proj-pin", &project))
                                            .flex_shrink_0()
                                            .text_color(rgb(ui_theme::text_faint()))
                                            .child(Icon::new(IconName::Star).size(px(11.))),
                                    )
                                })
                                .when(agg != AgentStatus::Idle, |row| {
                                    row.child(
                                        div()
                                            .id(project_element_id("proj-status-dot", &project))
                                            .flex_shrink_0()
                                            .size(px(6.))
                                            .rounded_full()
                                            .bg(ui_theme::session_dot_color(agg)),
                                    )
                                })
                                .children(branch_label.as_ref().map(|b| {
                                    // 窄侧栏里分支名常被截掉后半段，悬浮看全名。
                                    let full_branch: SharedString = b.clone().into();
                                    div()
                                        .id(project_element_id("proj-branch", &project))
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .font_family(crate::terminal_view::font_family())
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .tooltip(move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                full_branch.clone(),
                                            )
                                            .build(window, cx)
                                        })
                                        .child(b.clone())
                                })),
                        )
                        .child(
                            // 数字和「+」不是两套右对齐元素，而是同一个方形槽里的
                            // 两个满铺图层；hover 只切透明度，因此字形中心不会横跳。
                            // 数字只在折叠或空项目时有用：展开后下面的会话一眼就数得出来。
                            div()
                                .relative()
                                .flex_shrink_0()
                                .w(px(PROJECT_NEW_BUTTON_SIZE))
                                // 保留原来的项目行高；28px 新建按钮在 hover 时向上下
                                // 各扩 4px，完整占满标题行，而不是把所有项目行撑高。
                                .h(px(PROJECT_ACTION_SLOT_HEIGHT))
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .when(empty_group, |d| d.opacity(0.55))
                                        .group_hover(PROJ_HEADER_GROUP, |style| {
                                            style.opacity(0.0)
                                        })
                                        .when(collapsed || empty_group, |d| {
                                            d.child(ixs.len().to_string())
                                        }),
                                )
                                .when(is_project_group, |slot| {
                                    slot.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .top(px(
                                                (PROJECT_ACTION_SLOT_HEIGHT
                                                    - PROJECT_NEW_BUTTON_SIZE)
                                                    / 2.,
                                            ))
                                            .w(px(PROJECT_NEW_BUTTON_SIZE))
                                            .h(px(PROJECT_NEW_BUTTON_SIZE))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .child(new_session_picker(
                                                e_menu.clone(),
                                                menu_runtime.clone(),
                                                menu_cwd.clone(),
                                                cx,
                                            )),
                                    )
                                }),
                        )
                        .when(can_drag, |header| {
                            with_project_drag(
                                header,
                                ProjectDrag {
                                    project: project.clone(),
                                    label: group_name.clone(),
                                    collapsed,
                                    empty: empty_group,
                                    branch: branch_label
                                        .as_ref()
                                        .map(|branch| SharedString::from(branch.clone())),
                                    agg,
                                    session_count: ixs.len(),
                                    grab: Point::default(),
                                },
                                this.clone(),
                            )
                        })
                        .on_mouse_up(MouseButton::Left, move |_ev, window, cx| {
                            let project = toggle_project.clone();
                            e_toggle.update(cx, |ws, cx| {
                                // 整行也挂了 on_drag：超过阈值起拖后不要再当成点选。
                                if ws.sidebar_drag.is_some() {
                                    return;
                                }
                                ws.select_project(project.clone(), window, cx);
                                if crate::sidebar_order::project_header_click_toggles_collapse(
                                    !empty_group,
                                ) {
                                    ws.toggle_project_collapsed(&project, cx);
                                }
                            });
                        })
                        .context_menu({
                            let e_pin = e_menu.clone();
                            let e_sidebar_pin = e_menu.clone();
                            let e_close_proj = e_menu.clone();
                            let path = cwd.clone();
                            let close_project = project.clone();
                            let menu_project = project.clone();
                            let sess_n = ixs.len();
                            let copy_branch = branch_label.clone();
                            move |menu, window, cx| {
                                if !is_project_group {
                                    return menu;
                                }
                                // 远程项目的文件不在这台机器上：打开方式、文件树都还做不了，
                                // 只给通用的几项。
                                if !is_local_group {
                                    return remote_project_menu(
                                        menu,
                                        menu_project.clone(),
                                        sess_n,
                                        e_close_proj.clone(),
                                        cx,
                                    );
                                }
                                let copy_path = path.clone();
                                let pin_path = path.clone();
                                let sidebar_pin_project = close_project.clone();
                                let e_pin = e_pin.clone();
                                let e_sidebar_pin = e_sidebar_pin.clone();
                                let e_close_proj = e_close_proj.clone();
                                let close_project = close_project.clone();
                                let copy_branch = copy_branch.clone();
                                // 已 pin → 显示「从文件树移除」，否则「加到文件树」（当前活动项目
                                // 天然在文件树里，pin 它=切走后仍保留，所以照样给这个开关）。
                                let pinned = e_pin.read(cx).is_file_tree_root_pinned(&pin_path);
                                let pin_label = if pinned {
                                    "从文件树移除"
                                } else {
                                    "加到文件树"
                                };
                                let sidebar_pinned =
                                    e_sidebar_pin.read(cx).is_project_pinned(&sidebar_pin_project);
                                let sidebar_pin_label = if sidebar_pinned {
                                    "取消置顶"
                                } else {
                                    "置顶"
                                };
                                // 按「复制 / 项目 / 离开」分段。
                                let open_root = copy_path.clone();
                                let e_open = e_menu.clone();
                                menu
                                    .submenu_with_icon(
                                        Some(IconName::ExternalLink.into()),
                                        "用其他应用打开",
                                        window,
                                        cx,
                                        move |menu, window, menu_cx| {
                                            let popup = menu_cx.entity();
                                            let root = open_root.clone();
                                            e_open.update(menu_cx, |ws, cx| {
                                                ws.build_ide_popup_menu(
                                                    menu, root, popup, window, cx,
                                                )
                                            })
                                        },
                                    )
                                    .item(
                                        PopupMenuItem::new("复制项目路径")
                                            .icon(IconName::Copy)
                                            .on_click(move |_ev, _window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_path.clone(),
                                                ));
                                            }),
                                    )
                                    // 分支名只有 git 仓库才有（项目行上能看见分支名才给复制）。
                                    .when(copy_branch.is_some(), move |menu| {
                                        let branch = copy_branch.unwrap_or_default();
                                        menu.item(
                                            PopupMenuItem::new("复制分支名")
                                                .icon(IconName::Copy)
                                                .on_click(move |_ev, _window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(branch.clone()),
                                                    );
                                                }),
                                        )
                                    })
                                    .separator()
                                    .item(
                                        PopupMenuItem::new(sidebar_pin_label)
                                            .icon(IconName::Star)
                                            .on_click(move |_ev, _window, cx| {
                                                let project = sidebar_pin_project.clone();
                                                e_sidebar_pin.update(cx, |ws, cx| {
                                                    ws.toggle_project_pinned(project, cx)
                                                });
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new(pin_label)
                                            .icon(IconName::Folder)
                                            .on_click(move |_ev, _window, cx| {
                                                let pin_path = pin_path.clone();
                                                e_pin.update(cx, |ws, cx| {
                                                    ws.toggle_file_tree_root(pin_path, cx)
                                                });
                                            }),
                                    )
                                    // 关项目 = 从工作台移走这个项目，连带关掉它下面的会话
                                    //（标数量，别让人点完才发现关掉了一堆活）。
                                    .separator()
                                    .item(
                                        PopupMenuItem::new(if sess_n > 0 {
                                            format!("关闭项目（含 {sess_n} 个会话）")
                                        } else {
                                            "关闭项目".to_string()
                                        })
                                        .icon(IconName::Close)
                                        .on_click(
                                            move |_ev, _window, cx| {
                                                let project = close_project.clone();
                                                e_close_proj.update(cx, |ws, cx| {
                                                    ws.start_close_project(project, cx)
                                                });
                                            },
                                        ),
                                    )
                            }
                        })
                        .when(can_drag && dragging, |header| {
                            attach_project_drop_layers(
                                header,
                                project.clone(),
                                proj_hint_before,
                                proj_hint_after,
                                this.clone(),
                            )
                        }),
                );
            }

            if collapsed {
                if grouping != SidebarGrouping::None {
                    rows = rows.child(
                        flip_slot(project_flip_id(&project), reorder_generation, group_rows)
                            .from_top(drop_origin(crate::SidebarDrag::Project(project.clone()))),
                    );
                }
                continue;
            }

            if ixs.is_empty() {
                // 空项目不占一行说明。Grok 的空分组就是没有条目，新建走标题上的 +。
                rows = rows.child(
                    flip_slot(project_flip_id(&project), reorder_generation, group_rows)
                        .from_top(drop_origin(crate::SidebarDrag::Project(project.clone()))),
                );
                continue;
            }

            // ---- 组内会话行 ----
            // 会话保持两行信息和 22px 缩进；项目模式再从 chevron 下方落一根 hairline，
            // 它位于会话卡片左侧，不会和选中底色叠在一起。
            let mut group_body = div()
                .relative()
                .flex()
                .flex_col()
                .gap(px(1.))
                .pb_1()
                .when(is_project_group, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(PROJECT_GUIDE_LEFT))
                            .top_0()
                            .bottom(px(4.))
                            .w(px(1.))
                            .bg(ui_theme::hairline()),
                    )
                })
                .when(grouping != SidebarGrouping::None, |d| {
                    d.pl(px(PROJECT_SESSION_INDENT))
                })
                .when(grouping == SidebarGrouping::None, |d| d.px_2());
            for &ix in ixs {
                let title = titles.get(ix).map(|(_, t)| t.clone()).unwrap_or_default();
                let status = statuses.get(ix).copied().unwrap_or(AgentStatus::Idle);
                let provider = self.sessions[ix].provider_kind(cx);
                // ACP 身份优先：只有 ACP 的 agent（dsh）在终端表里查不到，
                // 走 `provider` 会把它显示成一个普通终端。
                let acp_provider = self.sessions[ix].acp_kind(cx);
                let provider_label: &'static str = acp_provider
                    .map(|agent| agent.label())
                    .or_else(|| provider.map(|agent| agent.label()))
                    .unwrap_or("终端");
                let agent_mark = crate::agents::bound_agent_mark(
                    self.sessions[ix].agent_definition_id.as_deref(),
                    self.sessions[ix].automation_id.as_deref(),
                    &cx.global::<AgentHostState>().agents,
                );
                let identity_label = provider_label.to_string();
                let project_context_matches = !is_project_group || is_active_group;
                let is_active = session_row_is_selected(
                    session_route_active,
                    project_context_matches,
                    ix,
                    active,
                );
                // 分屏数是额外状态，不和第二行的最近活动时间争正文位置。
                let subtitle = match &self.sessions[ix].kind {
                    SessionKind::Conversation(_) => None,
                    SessionKind::Term { .. } => {
                        let n = self.sessions[ix].pane_count();
                        (n > 1).then(|| format!("⑂{n}"))
                    }
                };
                // 运行中 / 要人管的状态才配文字。空闲不写——每行都写「空闲」
                // 等于用一整行高度说一句废话。
                let status_label = session_row_status_label(status);
                // 终端程序报的进度（OSC 9;4），看当前那个分屏的。
                let progress = match &self.sessions[ix].kind {
                    SessionKind::Term { active, .. } => {
                        let view = active.read(cx);
                        smelt_ui::daemon_states_global::DaemonStates::progress(
                            view.runtime().key(),
                            view.session_id(),
                            cx,
                        )
                    }
                    SessionKind::Conversation(_) => None,
                };
                let session_updated_at = last_updated_at.get(ix).copied().unwrap_or_default();
                let updated_text = session_updated_at_text(session_updated_at, &now);
                let project_context = (grouping != SidebarGrouping::Project)
                    .then(|| session_projects.get(ix).cloned().flatten())
                    .flatten();
                let is_acp = matches!(self.sessions[ix].kind, SessionKind::Conversation(_));
                // 会话 ID 是 smeltd 侧的稳定运行时标识；终端会话取当前活动 pane，
                // ACP 会话取自身。分屏子行会在各自的菜单中复制自己的 ID。
                let session_id = match &self.sessions[ix].kind {
                    SessionKind::Term { active, .. } => active.read(cx).session_id().to_string(),
                    SessionKind::Conversation(view) => view.read(cx).session_id().to_string(),
                };
                let e_act = this.clone();
                let e_close = this.clone();
                let e_rename = this.clone();
                let e_restart = this.clone();
                let ui_id = self.sessions[ix].ui_id;
                let drag_title: SharedString = title.clone().into();
                let is_drag_source_session = dragging_session == Some(ui_id);
                let sess_hint_before = can_drag
                    && !is_drag_source_session
                    && self.sess_drop_hint == Some((ui_id, true));
                let sess_hint_after = can_drag
                    && !is_drag_source_session
                    && self.sess_drop_hint == Some((ui_id, false));

                // 运行态只在进入时短暂提亮一次；随后保留静态蓝，不让一个长任务
                // 永久持有动画 timer。明确状态仍可从行内标签或图标 tooltip 获得。
                let session_icon = if let Some(mark) = agent_mark {
                    div()
                        .id(("sess-provider-icon", ui_id))
                        .flex_shrink_0()
                        .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(Self::session_icon_color(status, None))
                        .child(
                            div()
                                .size(px(SESSION_AGENT_ICON_SIZE))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .font_medium()
                                .child(mark),
                        )
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(identity_label.clone())
                                .build(window, cx)
                        })
                        .into_any_element()
                } else if animate_running && status == AgentStatus::Running {
                    ambient_animation(
                        ("sess-provider-glow", ui_id),
                        SESSION_GLOW_PERIOD,
                        true,
                        move |phase| {
                            let tooltip_label = identity_label.clone();
                            div()
                                .id(("sess-provider-icon", ui_id))
                                .flex_shrink_0()
                                .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(Self::session_icon_color(status, Some(phase)))
                                .child(
                                    acp_provider
                                        .map(acp_provider_icon)
                                        .unwrap_or_else(|| provider_icon(provider))
                                        .size(px(SESSION_AGENT_ICON_SIZE)),
                                )
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tooltip_label.clone())
                                        .build(window, cx)
                                })
                        },
                    )
                    .into_any_element()
                } else {
                    div()
                        .id(("sess-provider-icon", ui_id))
                        .flex_shrink_0()
                        .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(Self::session_icon_color(status, None))
                        .child(
                            acp_provider
                                .map(acp_provider_icon)
                                .unwrap_or_else(|| provider_icon(provider))
                                .size(px(SESSION_AGENT_ICON_SIZE)),
                        )
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(identity_label.clone())
                                .build(window, cx)
                        })
                        .into_any_element()
                };

                let row_tooltip = title.clone();
                let row = div()
                    .id(("sess-row", ui_id))
                    .group(SESSION_ROW_GROUP)
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(6.))
                    .rounded(ui_theme::row_radius())
                    .when(is_drag_source_session, |d| d.opacity(0.4))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(row_tooltip.clone()).build(window, cx)
                    })
                    // 选中靠整张会话卡亮起来，不再靠左侧白条——那是频道列表的旧说法。
                    // hover 走 bg_row_hover：划过只浮起一点，亮起来留给当前会话。
                    .map(|d| {
                        if is_active {
                            d.bg(rgb(ui_theme::bg_selected()))
                        } else {
                            d.hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                        }
                    })
                    // Grok 对话列表：小图标，不套色块。状态只给图标上色。
                    .child(session_icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            // 会话名是侧栏主体（常规字重、正文色）；项目标题则用同字号
                            // semibold 表达上一级结构，不靠继续放大字号争夺注意力。
                            .child(
                                div()
                                    .min_w_0()
                                    .line_height(px(16.))
                                    .text_sm()
                                    .text_color(rgb(if is_active {
                                        ui_theme::text_bright()
                                    } else {
                                        ui_theme::text()
                                    }))
                                    .truncate()
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .line_height(px(14.))
                                    .text_xs()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .truncate()
                                    .child(updated_text),
                            ),
                    )
                    // 分屏数继续用行尾小角标；第二行固定留给跨会话可比较的时间。
                    .children(subtitle.as_ref().map(|s| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_family(crate::terminal_view::font_family())
                            .text_color(rgb(ui_theme::text_faint()))
                            .child(s.clone())
                    }))
                    // 按状态 / 不分组时，项目标题已经不存在，把来源项目与分支补到行内。
                    .children(project_context.map(|(project, branch)| {
                        div()
                            .flex_shrink_0()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(rgb(ui_theme::text_muted()))
                            .child(project)
                            .children(branch.map(|name| {
                                div()
                                    .font_family(crate::terminal_view::font_family())
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child(name)
                            }))
                    }))
                    .children(progress.map(session_progress_bar))
                    // 只要人管才写字，不加色胶囊。
                    .children(status_label.map(|label| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(ui_theme::session_dot_color(status))
                            .child(label)
                    }))
                    .child(
                        // 关闭是这一行唯一的主操作，放在行尾，hover 整行才显出来；用透明度
                        // 而不是不渲染，位置一直占着，显隐时标题截断不跳。不用 ghost Button：
                        // 它的 hover 底是 secondary(bg_card).lighten(0.1)，压在选中行的
                        // bg_selected 上比底色还暗，且图标色 hover 前后不变。这里自己画，
                        // hover 只把图标转红、不加底。
                        div()
                            .id(("close-session", ui_id))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded(px(4.))
                            .text_color(rgb(ui_theme::text_muted()))
                            .opacity(0.0)
                            .group_hover(SESSION_ROW_GROUP, |s| s.opacity(1.0))
                            .hover(|d| d.text_color(rgb(ui_theme::red())))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .child(Icon::new(IconName::Close).size(px(14.)))
                            .on_click(move |_ev, window, cx| {
                                cx.stop_propagation();
                                e_close.update(cx, |ws, cx| ws.request_close_session(ix, window, cx));
                            }),
                    )
                    .when(can_drag, |row| {
                        with_session_drag(
                            row,
                            SessionDrag {
                                id: ui_id,
                                title: drag_title.clone(),
                                acp: acp_provider,
                                terminal: provider,
                                status,
                                subtitle: subtitle.as_deref().map(SharedString::from),
                                status_label: status_label.map(SharedString::from),
                                grab: Point::default(),
                            },
                            this.clone(),
                        )
                    })
                    .on_click(move |_ev, window, cx| {
                        e_act.update(cx, |ws, cx| ws.activate(ix, window, cx));
                    })
                    .context_menu(move |menu, _window, _cx| {
                        let e_rename = e_rename.clone();
                        let e_restart = e_restart.clone();
                        let copy_session_id = session_id.clone();
                        let sess_ix = ix;
                        let mut menu = menu
                            .item(
                                PopupMenuItem::new("重命名")
                                    .icon(Icon::empty().path("smelt-icons/square-pen.svg"))
                                    .on_click(move |_ev, window, cx| {
                                        e_rename.update(cx, |ws, cx| {
                                            ws.start_rename(RenameTarget::Session(ix), window, cx)
                                        });
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("复制会话 ID")
                                    .icon(IconName::Copy)
                                    .on_click(move |_ev, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_session_id.clone(),
                                        ));
                                    }),
                            );
                        // 只有 ACP 对话会话才需要「强制重启」：卡在工具调用里、
                        // 「停止」按钮打不断时的兜底，见 `force_restart_acp_session`。
                        if is_acp {
                            menu = menu.item(
                                PopupMenuItem::new("强制重启")
                                    .icon(IconName::Redo)
                                    .on_click(move |_ev, _window, cx| {
                                        e_restart.update(cx, |ws, cx| {
                                            ws.force_restart_acp_session(sess_ix, cx)
                                        });
                                    }),
                            );
                        }
                        menu
                    })
                    .when(can_drag && dragging, |row| {
                        attach_session_drop_layers(
                            row,
                            ui_id,
                            sess_hint_before,
                            sess_hint_after,
                            this.clone(),
                        )
                    });
                // 单 pane 会话：整会话就是这一行（上面构建的 row）。
                // 分屏会话：不显示会话名父行；把同一 tab 的多个 pane 用内层括线
                // 圈在一起平铺，pane 行本身只管「切到该 pane」。
                if self.sessions[ix].pane_count() <= 1 {
                    group_body = group_body.child(
                        flip_slot(("sidebar-flip-session", ui_id), reorder_generation, row)
                            .from_top(drop_origin(crate::SidebarDrag::Session(ui_id))),
                    );
                } else {
                    let leaves = self.sessions[ix].term_leaves();
                    let active_pane_id = self.sessions[ix].anchor_id();
                    // 组内重名的 pane 标题补序号，避免「smelt 里又一个 smelt」。
                    let raw_titles: Vec<String> =
                        leaves.iter().map(|v| pane_title(v, cx)).collect();

                    // 内层括线：比项目引导线再内缩一档，左侧圆角竖线把同一 tab 的
                    // 几个 pane 圈成一组（视觉上 ≈ ╭…╰ 括号）。
                    // pane 行跟普通会话行同缩进、同字号，不再内缩变小；「成组」只由
                    // 左侧一条括线表达（见下方 pane_group 的 absolute 竖线）。
                    let mut pane_rows = div().flex().flex_col().gap(px(1.));
                    for (lix, view) in leaves.into_iter().enumerate() {
                        let base = raw_titles[lix].clone();
                        let dup = raw_titles.iter().filter(|t| **t == base).count() > 1;
                        let p_title = if dup {
                            format!("{} · {}", base, lix + 1)
                        } else {
                            base
                        };
                        let p_status = pane_status(&view, cx);
                        let pane_updated_at = crate::daemon_state_for(&view, cx)
                            .map(|state| state.updated_at)
                            .unwrap_or_default();
                        let pane_updated_text = session_updated_at_text(
                            effective_pane_updated_at(pane_updated_at, session_updated_at),
                            &now,
                        );
                        let pane_provider = pane_provider_kind(&view, cx);
                        let pane_tooltip = format!(
                            "{} · {}",
                            pane_provider.map(|agent| agent.label()).unwrap_or("终端"),
                            status_text(p_status)
                        );
                        let is_current_view = is_active && view.entity_id() == active_pane_id;
                        let e_pane_act = this.clone();
                        let e_pane_menu = this.clone();
                        let e_pane_close = this.clone();
                        let pane = view.clone();
                        let menu_pane = view.clone();
                        let close_pane = view.clone();
                        let copy_session_id = view.read(cx).session_id().to_string();
                        let pane_entity_id = view.entity_id();
                        let pane_icon = if animate_running && p_status == AgentStatus::Running {
                            ambient_animation(
                                ("pane-provider-glow", pane_entity_id),
                                SESSION_GLOW_PERIOD,
                                true,
                                move |phase| {
                                    let tooltip_label = pane_tooltip.clone();
                                    div()
                                        .id(("pane-provider-icon", pane_entity_id))
                                        .flex_shrink_0()
                                        .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(Self::session_icon_color(p_status, Some(phase)))
                                        .child(
                                            provider_icon(pane_provider)
                                                .size(px(SESSION_AGENT_ICON_SIZE)),
                                        )
                                        .tooltip(move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                tooltip_label.clone(),
                                            )
                                            .build(window, cx)
                                        })
                                },
                            )
                            .into_any_element()
                        } else {
                            div()
                                .id(("pane-provider-icon", pane_entity_id))
                                .flex_shrink_0()
                                .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(Self::session_icon_color(p_status, None))
                                .child(
                                    provider_icon(pane_provider).size(px(SESSION_AGENT_ICON_SIZE)),
                                )
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(pane_tooltip.clone())
                                        .build(window, cx)
                                })
                                .into_any_element()
                        };
                        pane_rows = pane_rows.child(
                            div()
                                .id(("sess-pane-row", pane_entity_id))
                                .relative()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py(px(6.))
                                .rounded(ui_theme::row_radius())
                                .when(is_drag_source_session, |d| d.opacity(0.4))
                                .map(|d| {
                                    if is_current_view {
                                        d.bg(rgb(ui_theme::bg_selected()))
                                    } else {
                                        d.hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                                    }
                                })
                                .child(pane_icon)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(px(1.))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .line_height(px(16.))
                                                .text_sm()
                                                // 当前 pane 提亮，其余用中灰——和会话行选中态同口径。
                                                .text_color(rgb(if is_current_view {
                                                    ui_theme::text_bright()
                                                } else {
                                                    ui_theme::text_mid()
                                                }))
                                                .truncate()
                                                .child(p_title),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .line_height(px(14.))
                                                .text_xs()
                                                .text_color(rgb(ui_theme::text_faint()))
                                                .truncate()
                                                .child(pane_updated_text),
                                        ),
                                )
                                // 每个 pane 自己的关闭键：只关这一个 pane，剩下的照常活着。
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(
                                            Button::new(("close-pane", pane_entity_id))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Close)
                                                .accessibility_label("关闭分栏")
                                                .on_click(move |_ev, window, cx| {
                                                    cx.stop_propagation();
                                                    let pane = close_pane.clone();
                                                    e_pane_close.update(cx, |ws, cx| {
                                                        ws.request_close_session_pane(
                                                            ix, pane, window, cx,
                                                        )
                                                    });
                                                }),
                                        ),
                                )
                                .when(can_drag, |row| {
                                    with_session_drag(
                                        row,
                                        SessionDrag {
                                            id: ui_id,
                                            title: drag_title.clone(),
                                            acp: acp_provider,
                                            terminal: provider,
                                            status,
                                            subtitle: subtitle.as_deref().map(SharedString::from),
                                            status_label: status_label.map(SharedString::from),
                                            grab: Point::default(),
                                        },
                                        this.clone(),
                                    )
                                })
                                .on_click(move |_ev, window, cx| {
                                    let pane = pane.clone();
                                    e_pane_act.update(cx, |ws, cx| {
                                        ws.activate_session_pane(ix, pane, window, cx)
                                    });
                                })
                                .context_menu(move |menu, _window, _cx| {
                                    let e_rename = e_pane_menu.clone();
                                    let e_close_all = e_pane_menu.clone();
                                    let rename_pane = menu_pane.clone();
                                    let copy_session_id = copy_session_id.clone();
                                    menu.item(
                                        PopupMenuItem::new("重命名")
                                            .icon(Icon::empty().path("smelt-icons/square-pen.svg"))
                                            .on_click(move |_ev, window, cx| {
                                                let target = RenameTarget::Pane(rename_pane.clone());
                                                e_rename.update(cx, |ws, cx| {
                                                    ws.start_rename(target, window, cx)
                                                });
                                            }),
                                    )
                                    .item(
                                        PopupMenuItem::new("复制会话 ID")
                                            .icon(IconName::Copy)
                                            .on_click(move |_ev, _window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_session_id.clone(),
                                                ));
                                            }),
                                    )
                                    // 行内的 × 只关这一个 pane；要连整组一起关走这里。
                                    .item(
                                        PopupMenuItem::new("关闭整个会话（含全部分屏）")
                                            .icon(IconName::Close)
                                            .on_click(move |_ev, window, cx| {
                                                e_close_all.update(cx, |ws, cx| {
                                                    ws.request_close_session(ix, window, cx)
                                                });
                                            }),
                                    )
                                })
                                .when(can_drag && dragging, |row| {
                                    attach_session_drop_layers(
                                        row,
                                        ui_id,
                                        sess_hint_before && lix == 0,
                                        sess_hint_after && lix + 1 == raw_titles.len(),
                                        this.clone(),
                                    )
                                }),
                        );
                    }

                    // 组容器只负责把同一会话的多个 pane 视觉归在一起。
                    let pane_group = div()
                        .id(("sess-pane-group", ui_id))
                        .relative()
                        .child(pane_rows)
                        // 成组的唯一标记：贴左缘一条圆角竖线，把这几个 pane 圈在一起，
                        // 不挤占内容宽度（pane 行仍与普通会话行左对齐、同字号）。括线放在
                        // pane_rows 后绘制，确保 hover / 选中背景不会把分组关系盖掉。
                        .child(
                            div()
                                .absolute()
                                .left(px(1.))
                                .top(px(2.))
                                .bottom(px(2.))
                                .w(px(9.))
                                // 圆弧括号 ╭…╰：只描左/上/下三条边 + 整体圆角，右边开口，
                                // 顶底两个圆角拐角把这组 pane「抱」起来（不挤占内容宽度）。
                                .border_l_1()
                                .border_t_1()
                                .border_b_1()
                                .border_color(rgb(ui_theme::border_loud()))
                                .rounded(px(7.)),
                        );

                    group_body = group_body.child(
                        flip_slot(
                            ("sidebar-flip-session", ui_id),
                            reorder_generation,
                            pane_group,
                        )
                        .from_top(drop_origin(crate::SidebarDrag::Session(ui_id))),
                    );
                }
            }
            rows = rows.child(
                flip_slot(
                    project_flip_id(&project),
                    reorder_generation,
                    group_rows.child(group_body),
                )
                .from_top(drop_origin(crate::SidebarDrag::Project(project.clone()))),
            );
        }

        for runtime in headers_left {
            rows = rows.child(runtime_section_header(runtime, this.clone(), cx));
        }

        if hidden_empty > 0 {
            let e_show = this.clone();
            rows = rows.child(
                div()
                    .id("hidden-empty-projects")
                    .mx_2()
                    .mt_2()
                    .px(px(PROJECT_HEADER_PAD_X))
                    .py(px(4.))
                    .rounded(ui_theme::row_radius())
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_ev, _window, cx| {
                        e_show.update(cx, |ws, cx| {
                            ws.sidebar_hide_empty_projects = false;
                            ws.save_state(cx);
                            cx.notify();
                        });
                    })
                    .text_xs()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(format!("已隐藏 {hidden_empty} 个无会话项目")),
            );
        }

        // ---- 底部操作条：设置 ----
        // 「添加项目」在「项目」标题行右侧，跟 Grok 一样跟分组标题同行。
        let e_settings = this.clone();
        let settings_needs_attention =
            self.update_available() || self.daemon_outdated == Some(true);

        // ---- 底部额度条：provider 账户额度（Codex / Claude / Copilot / Grok / Cursor）----
        // 从舞台头栏迁到这里（底部操作条上方）。只显示「检测到额度」的 provider，
        // 全部挤在一行：每项 = provider 图标 + 最差窗口的已用百分比（颜色也由它
        // 决定）；悬浮单项看完整窗口明细与重置时间；点击整条刷新。
        // Pi 不在这里：它底下几家 provider 是独立账户，额度跟在对话模型菜单的
        // provider 名字后面（括号），那里才说得清数字属于谁。
        let quota_this = this.clone();
        // (provider 身份, 行内百分比文本, 行内颜色, tooltip 完整文案, 是否用户手动刷新中)
        // 状态在这里一次性读完，子元素的构造闭包里不再碰全局。
        let quota_rows = crate::provider_quota::ProviderStatus::sidebar_targets(cx)
            .into_iter()
            .filter_map(|target| {
                let status = crate::provider_quota::ProviderStatus::quota_status(target, cx)?;
                let row = provider_quota_row(target, &status)?;
                let manual_refreshing =
                    crate::provider_quota::ProviderStatus::is_manually_refreshing(target, cx);
                Some((target, row.text, row.color, row.hint, manual_refreshing))
            })
            .collect::<Vec<_>>();

        let quota_footer = if quota_rows.is_empty() {
            // 完全没有额度数据：只保留「查询中」过渡行；否则整条不占位。
            if crate::provider_quota::ProviderStatus::is_refreshing(cx) {
                Some(
                    div()
                        .id("sidebar-quota")
                        .flex_shrink_0()
                        .px_2()
                        .py(px(4.))
                        .border_t_1()
                        .border_color(rgb(ui_theme::border_dim()))
                        .text_xs()
                        .font_family(crate::terminal_view::font_family())
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("额度查询中…"),
                )
            } else {
                None
            }
        } else {
            Some(
                div()
                    .id("sidebar-quota")
                    .flex_shrink_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(4.))
                    .border_t_1()
                    .border_color(rgb(ui_theme::border_dim()))
                    .children(quota_rows.into_iter().map(
                        |(target, text, color, hint, is_manual_refreshing)| {
                            // 只对用户手动点击刷新的那家转绿；启动首查和定时刷新保持
                            // 静默，继续显示已有额度自己的风险颜色。
                            // 点击只刷新当前这一家（点谁查谁）。
                            let tip = if is_manual_refreshing {
                                format!("{hint} 正在刷新…")
                            } else {
                                format!("{hint} 点击刷新。")
                            };
                            let on_click_this = quota_this.clone();
                            div()
                                .id(SharedString::from(target.id()))
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_1()
                                .rounded(px(4.))
                                .text_xs()
                                .font_family(crate::terminal_view::font_family())
                                .text_color(rgb(if is_manual_refreshing {
                                    ui_theme::green()
                                } else {
                                    color
                                }))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                                .child(acp_provider_icon(target.agent).size(px(12.)))
                                .child(text)
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tip.clone())
                                        .build(window, cx)
                                })
                                .on_click(move |_ev, _window, cx| {
                                    on_click_this.update(cx, |workspace, cx| {
                                        workspace.request_provider_quota_refresh(target);
                                        cx.notify();
                                    });
                                })
                        },
                    )),
            )
        };

        let footer = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .px_2()
            .py_2()
            .border_t_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .id("sidebar-settings")
                    .relative()
                    .flex_1()
                    .h(ui_theme::height::md())
                    .min_w_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded_full()
                    .cursor_pointer()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_mid()))
                    .hover(|d| {
                        d.bg(rgb(ui_theme::bg_row_hover()))
                            .text_color(rgb(ui_theme::text_bright()))
                    })
                    .child(
                        Button::new("sidebar-settings-btn")
                            .custom(ButtonCustomVariant::new(cx))
                            .h(ui_theme::height::md())
                            .w_full()
                            .px_0()
                            .cursor_pointer()
                            .accessibility_label("设置")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(rgb(ui_theme::text_mid()))
                                    .child(
                                        Icon::new(IconName::Settings)
                                            .size(px(14.))
                                            .text_color(rgb(ui_theme::text_muted())),
                                    )
                                    .child("设置"),
                            )
                            .dropdown_menu_with_anchor(
                                Anchor::BottomLeft,
                                move |menu, _window, _cx| {
                                    let e_settings = e_settings.clone();
                                    let e_check_update = e_settings.clone();
                                    let menu = menu
                                        .item(
                                            PopupMenuItem::new("帮助文档")
                                                .icon(IconName::BookOpen)
                                                .on_click(|_ev, _window, cx| {
                                                    cx.open_url(
                                                        "https://smelt.onoo.io/",
                                                    );
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("反馈问题")
                                                .icon(IconName::Info)
                                                .on_click(|_ev, _window, cx| {
                                                    cx.open_url(crate::settings::FEEDBACK_URL);
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("检查更新...")
                                                .icon(IconName::LoaderCircle)
                                                .on_click(move |_ev, _window, cx| {
                                                    e_check_update.update(cx, |ws, cx| {
                                                        if ws.update_status.can_check()
                                                            || ws.update_status.can_retry_recovery()
                                                        {
                                                            ws.check_or_recover_update(false, cx);
                                                        }
                                                        ws.open_settings_section(
                                                            crate::settings::SettingsSection::maintenance_update(),
                                                            cx,
                                                        );
                                                    });
                                                }),
                                        )
                                        .separator();

                                    menu.item(
                                        PopupMenuItem::new("设置...")
                                            .icon(IconName::Settings)
                                            .on_click(move |_ev, _window, cx| {
                                                e_settings.update(cx, |ws, cx| {
                                                    ws.check_daemon_outdated(cx);
                                                    ws.settings_section =
                                                        crate::settings::SettingsSection::appearance();
                                                    ws.open_settings_window(cx);
                                                });
                                            }),
                                    )
                                },
                            ),
                    )
                    .when(settings_needs_attention, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .right(px(4.))
                                .size(px(5.))
                                .rounded_full()
                                .bg(rgb(ui_theme::red())),
                        )
                    }),
            );

        div()
            .w_full()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            // 外层工作区负责玻璃表面、描边与裁切；列表自身保持透明，才能让
            // vibrancy / 半透明底贯穿整个面板而不是只透出四角。
            .bg(gpui::transparent_black())
            // 顶部固定的只有几个入口；「最近对话」和项目都在下面的滚动区里。
            .child(self.render_automation_navigation(this, statuses, cx))
            .child(
                div()
                    .id("session-rows-pane")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(rows)
                    .vertical_scrollbar(&self.sidebar_scroll),
            )
            .children(quota_footer)
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROJECT_ACTION_SLOT_HEIGHT, PROJECT_DISCLOSURE_ICON_SIZE, PROJECT_GUIDE_LEFT,
        PROJECT_NEW_BUTTON_SIZE, PROJECT_SESSION_INDENT, SESSION_AGENT_ICON_BOX_SIZE,
        SESSION_AGENT_ICON_SIZE, project_disclosure_icon, project_header_is_selected,
        session_row_is_selected, session_row_status_label, status_text,
    };
    use crate::AgentStatus;
    use gpui_component::IconName;

    const _: () = {
        assert!(PROJECT_NEW_BUTTON_SIZE >= 28.);
        assert!(PROJECT_NEW_BUTTON_SIZE > PROJECT_ACTION_SLOT_HEIGHT);
        assert!(PROJECT_SESSION_INDENT == 18.);
        assert!(PROJECT_GUIDE_LEFT < PROJECT_SESSION_INDENT);
        assert!(PROJECT_DISCLOSURE_ICON_SIZE >= 15.);
        assert!(SESSION_AGENT_ICON_SIZE >= 17.);
        assert!(SESSION_AGENT_ICON_BOX_SIZE >= SESSION_AGENT_ICON_SIZE);
    };

    #[test]
    fn project_and_agent_icons_are_visually_distinct() {
        assert!(matches!(project_disclosure_icon(true), IconName::Folder));
        assert!(matches!(
            project_disclosure_icon(false),
            IconName::FolderOpen
        ));
    }

    #[test]
    fn task_panel_does_not_select_the_last_active_session() {
        assert!(session_row_is_selected(true, true, 2, 2));
        assert!(!session_row_is_selected(false, true, 2, 2));
        assert!(!session_row_is_selected(true, true, 1, 2));
        assert!(!session_row_is_selected(true, false, 2, 2));
    }

    #[test]
    fn active_session_is_the_only_selected_row_inside_its_project() {
        assert!(!project_header_is_selected(true, true, true));
        assert!(session_row_is_selected(true, true, 2, 2));
        // 会话被折叠隐藏或项目为空时，项目行接管唯一的主选中态。
        assert!(project_header_is_selected(true, true, false));
    }

    #[test]
    fn running_sessions_color_the_icon_instead_of_writing_running() {
        assert_eq!(session_row_status_label(AgentStatus::Running), None);
        assert_eq!(status_text(AgentStatus::Running), "运行中");
    }

    #[test]
    fn idle_sessions_do_not_show_status_chrome() {
        assert_eq!(session_row_status_label(AgentStatus::Idle), None);
    }

    /// 智能体对话行和项目会话行共用同一张状态表：换了个位置不该自成一套。
    /// 行内只有「需要你」写字，运行中只上色。
    #[test]
    fn agent_conversation_rows_reuse_the_project_session_status_contract() {
        for status in [
            AgentStatus::NeedsYou,
            AgentStatus::Running,
            AgentStatus::Idle,
        ] {
            assert_eq!(
                crate::session_list::row::session_row_status_label(status),
                session_row_status_label(status),
                "对话行与会话行的状态文案必须同源"
            );
        }
        // 运行中与空闲的图标色必须能区分，否则「有没有在跑」看不出来。
        assert_ne!(
            crate::Workspace::session_icon_color(AgentStatus::Running, None),
            crate::Workspace::session_icon_color(AgentStatus::Idle, None)
        );
    }

    #[test]
    fn attention_sessions_keep_their_labels() {
        assert_eq!(
            session_row_status_label(AgentStatus::NeedsYou),
            Some("需要你")
        );
    }
}
