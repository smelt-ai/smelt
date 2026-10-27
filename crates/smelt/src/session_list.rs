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
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
pub(crate) use smelt_ui::Button;
use smelt_ui::IconAction as _;
use smelt_ui::ListRow;
use smelt_ui::motion::{ambient_animation, flip_slot};
use std::time::Duration;

// drag.rs / row.rs 作为本模块子模块沿用 `super::*` 读取 ACP 类型。
use crate::settings::ConversationAgentKind;
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
pub(crate) const PROJECT_SESSION_INDENT: f32 = 18.;
/// 引导线对着文件夹图标的中线。
pub(crate) const PROJECT_GUIDE_LEFT: f32 = 14.;
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

/// 机器名字，远程的带连接灯和要人处理时的几个字；悬停看完整的连接说明。
fn runtime_name_with_indicator(
    key: &RuntimeKey,
    name: impl IntoElement,
    indicator: Option<crate::runtimes::RuntimeIndicator>,
) -> AnyElement {
    // 机器名是分段标题，跟「项目」「对话」这些标题同一个样子：小号、中等字重、灰。
    let name = div()
        .min_w_0()
        .truncate()
        .text_xs()
        .font_medium()
        .text_color(rgb(ui_theme::text_muted()))
        .child(name);
    let Some(indicator) = indicator else {
        return name.into_any_element();
    };
    let light = match indicator.light {
        crate::runtimes::LinkLight::Good => ui_theme::green(),
        crate::runtimes::LinkLight::Degraded => ui_theme::yellow(),
        crate::runtimes::LinkLight::Down => ui_theme::red(),
    };
    let detail = SharedString::from(indicator.detail);
    h_flex()
        .id(ElementId::from(SharedString::from(format!(
            "runtime-indicator-{key:?}"
        ))))
        .min_w_0()
        .gap_1p5()
        .items_center()
        .child(name)
        .child(div().flex_none().size(px(6.)).rounded_full().bg(rgb(light)))
        .when_some(indicator.label, |d, label| {
            d.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(light))
                    .child(label),
            )
        })
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
        })
        // 延迟平时只在连接换路径时读；有人悬停来看时再读一次，提示里的数是新的。
        .on_hover({
            let key = key.clone();
            move |hovered, _window, cx| {
                if *hovered {
                    crate::runtimes::sample_link(&key, cx);
                }
            }
        })
        .into_any_element()
}

/// 侧栏里一台机器的分段标题：点整行折起或展开；远程机器名字后一盏连接灯（绿：在线；
/// 黄：慢、连接中、升级中；红：离线、版本不兼容），悬停名字看走哪条路、多慢、什么版本，
/// 要人处理的状态名字后再写几个字；折起时组里有对话在跑或在等你，挂一个状态点，免得
/// 「需要你」被折没了。`⋯` 和右键是同一份机器菜单（[`machine_menu`]），hover 时多出
/// 「远程桌面」（装了的才有）和「打开项目」的快捷按钮。`grouping` 是侧栏的「筛选与
/// 分组」按钮：分了机器段就没有「项目」标题了，它挂在第一段的标题上。
///
/// 跟「项目」「对话」标题同一套尺寸：28 高，文字和下面各行的图标落在同一条左缘。
/// 折叠箭头跟在名字后面，折起时常显，展开时悬停才出现。
fn runtime_section_header(
    runtime: crate::runtimes::Runtime,
    workspace: Entity<Workspace>,
    folded: bool,
    alert: Option<AgentStatus>,
    grouping: Option<Div>,
    // 键盘焦点环、滚到这一行用的锚，见 `keyboard`。
    extras: Vec<AnyElement>,
    cx: &App,
) -> impl IntoElement + use<> {
    let key = runtime.key().clone();
    let name = crate::runtimes::runtime_name(&key, cx);
    let indicator = crate::runtimes::runtime_indicator(&key, cx);
    let has_desktop = crate::runtimes::runtime_has_desktop(&key, cx);
    let group = SharedString::from(format!("runtime-header-{key:?}"));
    let toggle_workspace = workspace.clone();
    let toggle_key = key.clone();
    let menu_runtime = runtime.clone();
    let menu_workspace = workspace.clone();
    let context_runtime = runtime.clone();
    let context_workspace = workspace.clone();
    ListRow::new(ElementId::from(group.clone()))
        .group(group.clone())
        .relative()
        .children(extras)
        .mx_2()
        .mt_3()
        .pl(px(PROJECT_HEADER_PAD_X))
        .pr_1()
        .h(ui_theme::height::sm())
        .flex()
        .items_center()
        .gap_1()
        .rounded(ui_theme::row_radius())
        .on_click(move |_ev, _window, cx| {
            toggle_workspace.update(cx, |ws, cx| ws.toggle_runtime_fold(&toggle_key, cx));
        })
        .child(runtime_name_with_indicator(&key, name, indicator))
        .child(
            div()
                .flex_none()
                .opacity(if folded { 1.0 } else { 0.0 })
                .group_hover(group.clone(), |style| style.opacity(1.0))
                .child(
                    Icon::new(if folded {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size(px(12.))
                    .text_color(rgb(ui_theme::text_muted())),
                ),
        )
        .when_some(alert.filter(|_| folded), |d, status| {
            d.child(
                div()
                    .flex_none()
                    .size(px(6.))
                    .rounded_full()
                    .bg(ui_theme::session_dot_color(status)),
            )
        })
        .child(div().flex_1())
        .children(grouping.map(|grouping| {
            grouping
                .opacity(0.0)
                .group_hover(group.clone(), |style| style.opacity(1.0))
        }))
        .when(has_desktop, |d| {
            let runtime = runtime.clone();
            let workspace = workspace.clone();
            d.child(
                Button::new(ElementId::from(SharedString::from(format!(
                    "runtime-desktop-{key:?}"
                ))))
                .quiet()
                .icon_size()
                .icon(Icon::empty().path("smelt-icons/monitor.svg"))
                .icon_action("远程桌面")
                .opacity(0.0)
                .group_hover(group.clone(), |style| style.opacity(1.0))
                .on_click(move |_ev, window, cx| {
                    cx.stop_propagation();
                    let runtime = runtime.clone();
                    workspace.update(cx, |ws, cx| ws.open_desktop(runtime, window, cx));
                }),
            )
        })
        .child({
            let runtime = runtime.clone();
            let workspace = workspace.clone();
            Button::new(ElementId::from(SharedString::from(format!(
                "runtime-open-project-{key:?}"
            ))))
            .quiet()
            .icon_size()
            .icon(Icon::empty().path("smelt-icons/folder-plus.svg"))
            .icon_action("打开项目")
            .opacity(0.0)
            .group_hover(group, |style| style.opacity(1.0))
            .on_click(move |_ev, _window, cx| {
                cx.stop_propagation();
                let runtime = runtime.clone();
                workspace.update(cx, |ws, cx| ws.open_project_on(runtime, cx));
            })
        })
        // 点 `⋯` 只开菜单，不能顺带把分段折起来：在外面包一层截住冒泡。
        .child(
            div()
                .id(ElementId::from(SharedString::from(format!(
                    "runtime-menu-wrap-{key:?}"
                ))))
                .on_click(|_ev, _window, cx| cx.stop_propagation())
                .child(
                    Button::new(ElementId::from(SharedString::from(format!(
                        "runtime-menu-{key:?}"
                    ))))
                    .quiet()
                    .icon_size()
                    .icon(IconName::Ellipsis)
                    .icon_action("这台机器")
                    .dropdown_menu(move |menu, _window, cx| {
                        machine_menu(menu, menu_runtime.clone(), menu_workspace.clone(), cx)
                    }),
                ),
        )
        .context_menu(move |menu, _window, cx| {
            machine_menu(menu, context_runtime.clone(), context_workspace.clone(), cx)
        })
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
            .icon(Icon::new(gpui_kit_assets::IconName::Pin))
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

/// 底栏额度弹层的宽度：名字一栏、用量条、百分比放得下。
const QUOTA_POPOVER_WIDTH: f32 = 268.;

/// 底栏额度点开的列表：每家一行，名字、用量条、百分比。悬停看各窗口和重置时间，点哪家
/// 刷新哪家（只刷这一家）。
fn quota_popover(quotas: Vec<FooterQuota>, workspace: Entity<Workspace>) -> Div {
    div()
        .w_full()
        .p_1()
        // Popover 本身是无样式宿主，内容自己铺实色底和描边，跟新建菜单一样。
        .bg(ui_theme::glass_floating())
        .border_1()
        .border_color(ui_theme::card_stroke())
        .rounded(px(8.))
        .flex()
        .flex_col()
        .child(
            div()
                .h(ui_theme::height::sm())
                .px_2()
                .flex()
                .items_center()
                .text_xs()
                .font_medium()
                .text_color(rgb(ui_theme::text_muted()))
                .child("账户额度"),
        )
        .children(quotas.into_iter().map(|quota| {
            let color = quota.color;
            let target = quota.target;
            let tip = if quota.refreshing {
                format!("{} · 正在刷新…", quota.hint)
            } else {
                format!("{} · 点击刷新", quota.hint)
            };
            let workspace = workspace.clone();
            ListRow::new(SharedString::from(format!("quota-{}", target.id())))
                .h(px(30.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(6.))
                .text_xs()
                .child(
                    acp_provider_icon(target.agent)
                        .size(px(14.))
                        .text_color(rgb(color)),
                )
                .child(
                    div()
                        .w(px(64.))
                        .flex_shrink_0()
                        .truncate()
                        .text_color(rgb(ui_theme::text()))
                        .child(target.label()),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(4.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(rgb(ui_theme::border_dim()))
                        .child(
                            div()
                                .h_full()
                                .w(relative((quota.used / 100.0).clamp(0.0, 1.0) as f32))
                                .rounded_full()
                                .bg(rgb(color)),
                        ),
                )
                // 手点刷新、还没回来时，百分比那一格换成转圈：真在等，等完就停。
                .child(
                    div()
                        .w(px(36.))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .font_family(crate::terminal_view::font_family())
                        .text_color(rgb(color))
                        .map(|cell| {
                            if quota.refreshing {
                                cell.child(smelt_ui::motion::ambient_spinner(
                                    SharedString::from(format!("quota-refresh-{}", target.id())),
                                    rgb(color).into(),
                                    true,
                                ))
                            } else {
                                cell.child(quota.text)
                            }
                        }),
                )
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_click(move |_ev, _window, cx| {
                    workspace.update(cx, |ws, cx| {
                        ws.request_provider_quota_refresh(target);
                        cx.notify();
                    });
                })
        }))
}

fn project_disclosure_icon(collapsed_or_empty: bool) -> IconName {
    if collapsed_or_empty {
        IconName::Folder
    } else {
        IconName::FolderOpen
    }
}

mod drag;
mod keyboard;
mod machines;
mod new_session;
mod reveal;

pub(crate) use keyboard::SidebarItem;

pub(crate) use reveal::{SidebarReveal, attention_anchor, reveal_anchor};
use reveal::{attention_checker, attention_pill};

use drag::{
    ProjectDrag, SessionDrag, attach_project_drop_layers, attach_session_drop_layers,
    with_project_drag, with_session_drag,
};

pub(crate) mod row;

use machines::machine_menu;
use new_session::new_session_picker;
use row::{
    FooterQuota, acp_provider_icon, effective_pane_updated_at, focus_ring, group_disclosure,
    inline_rename_input, new_row_flash, project_header_is_selected, provider_icon,
    provider_quota_row, row_trailing_slot, session_progress_bar, session_row_is_selected,
    session_row_status_label, session_updated_at_text, status_text,
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
    /// 侧栏要画的分组：各机器依次铺开，每段内按当前分组方式分组，按项目分组时置顶的在前。
    /// render 和键盘导航共用这一份，两边的顺序才对得上。`each_project` 在分组前看一眼每台
    /// 机器的项目分组（render 要记下每个会话来自哪个项目）。
    pub(crate) fn sidebar_display_groups(
        &self,
        statuses: &[AgentStatus],
        last_updated_at: &[u64],
        cx: &App,
        mut each_project: impl FnMut(&RuntimeKey, &[crate::ProjectGroup]),
    ) -> Vec<crate::ProjectGroup> {
        let mut groups = Vec::new();
        for key in self.space_keys() {
            let Some(space) = self.space(&key) else {
                continue;
            };
            let project_groups = self.project_groups(&key, cx);
            each_project(&key, &project_groups);
            let members = self.space_members(&key, cx);
            let mut space_groups = crate::sidebar_groups(
                &key,
                self.sidebar_grouping,
                project_groups,
                statuses,
                last_updated_at,
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
        groups
    }

    /// 让会话在侧栏里看得见：它所在的机器段、项目折着就展开，下一帧滚到它那一行。
    /// 切会话（点行、⌘↑↓、点通知）和新建都走这里：当前会话不能藏在折起来的组里。
    pub(crate) fn reveal_session_in_sidebar(&mut self, ix: usize, cx: &App) {
        let Some(ui_id) = self.sessions.get(ix).map(|session| session.ui_id) else {
            return;
        };
        if let Some(group) = self
            .all_project_groups(cx)
            .into_iter()
            .find(|group| group.sessions.contains(&ix))
        {
            let folded = self.runtime_folded(&group.runtime);
            if let Some(space) = self.space_mut(&group.runtime) {
                if folded {
                    space.folded = Some(false);
                }
                space
                    .collapsed_projects
                    .retain(|root| !crate::sidebar_order::same_project_root(root, &group.root));
            }
        }
        self.sidebar_reveal.request(ui_id);
    }

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
        // 键盘焦点在侧栏里时，焦点环落在哪一行（`SidebarItem::key`）。
        focused_key: Option<u64>,
    ) -> Div {
        debug_assert_eq!(statuses.len(), self.sessions.len());
        // 上一帧量出的视口外「需要你」，这一帧画提示用。
        let offscreen = self.sidebar_reveal.offscreen();
        let reveal_target = self.sidebar_reveal.target();
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
        let groups =
            self.sidebar_display_groups(statuses, &last_updated_at, cx, |key, project_groups| {
                for group in project_groups {
                    // 仓库信息目前只有本机项目有。
                    let branch = self
                        .repo_info
                        .get(group.root.as_str())
                        .filter(|_| *key == RuntimeKey::Local)
                        .and_then(|(_, info)| info.as_ref())
                        .map(|info| info.branch.clone());
                    for &ix in &group.sessions {
                        session_projects[ix] = Some((group.label.clone(), branch.clone()));
                    }
                }
            });
        let session_route_active = self.active_tab().is_session();
        // 正在就地改名的那一行：标题那一格换成输入框。
        let rename_input = self.rename_input.clone();
        let renaming_session = match &self.rename_target {
            Some(RenameTarget::Session(ix)) => Some(*ix),
            _ => None,
        };
        let renaming_pane = match &self.rename_target {
            Some(RenameTarget::Pane(view)) => Some(view.entity_id()),
            _ => None,
        };

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
                    .quiet()
                    .compact()
                    .icon(Icon::new(gpui_kit_assets::IconName::SlidersHorizontal))
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

        // 挂了远程机器就按机器分段，机器名就是分段标题，不再叠一层「项目」。
        let runtime_sections = self.spaces.iter().any(|space| !space.runtime.is_local());
        // 「筛选与分组」只有一个：没分段时在「项目」标题上，分段时挂在第一段机器标题上。
        let mut grouping_slot = Some(grouping_btn);

        // 「项目」分组标题跟「对话」分组一起放进滚动区：两段列表共用一个滚动条。
        let e_open = this.clone();
        let project_section_header = (!runtime_sections).then(|| {
            let grouping_btn = grouping_slot.take();
            div()
                .id("project-section-header")
                .group(PROJ_SECTION_GROUP)
                .flex_shrink_0()
                .child(crate::workspace_frame::with_window_drag(
                    div()
                        .mx_2()
                        .mt_3()
                        .pl(px(PROJECT_HEADER_PAD_X))
                        .pr_1()
                        .h(ui_theme::height::sm())
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
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
                                .children(grouping_btn)
                                .child(
                                    div()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(
                                            Button::new("open-project")
                                                .quiet()
                                                .compact()
                                                .icon(
                                                    Icon::empty()
                                                        .path("smelt-icons/folder-plus.svg"),
                                                )
                                                .icon_action("添加项目")
                                                .on_click(move |_ev, _window, cx| {
                                                    e_open.update(cx, |ws, cx| ws.open_project(cx));
                                                }),
                                        ),
                                ),
                        ),
                ))
        });

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
            // 键盘操作，见 `keyboard`。焦点句柄挂在零大小的子元素上，点行不会把焦点从终端抢过来。
            .key_context("Sidebar")
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarUp, _window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_move(-1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarDown, _window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_move(1, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarCollapse, _window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_collapse(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarExpand, _window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_expand(cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarOpen, window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_open_cursor(window, cx));
                }
            })
            .on_action({
                let this = this.clone();
                move |_: &crate::SidebarExit, window, cx| {
                    this.update(cx, |ws, cx| ws.sidebar_exit(window, cx));
                }
            })
            // 打字跳转。只在焦点就在侧栏上时：行内改名的输入框也在这里面，打的字不能被截走。
            .on_key_down({
                let this = this.clone();
                let focus = self.sidebar_focus.clone();
                move |event: &KeyDownEvent, window, cx| {
                    let modifiers = &event.keystroke.modifiers;
                    if !focus.is_focused(window)
                        || modifiers.control
                        || modifiers.platform
                        || modifiers.alt
                        || modifiers.function
                    {
                        return;
                    }
                    let Some(text) = event
                        .keystroke
                        .key_char
                        .clone()
                        .filter(|text| !text.trim().is_empty())
                    else {
                        return;
                    };
                    cx.stop_propagation();
                    this.update(cx, |ws, cx| ws.sidebar_typeahead(&text, cx));
                }
            })
            .child(div().track_focus(&self.sidebar_focus).size_0())
            .child(self.render_product_conversation_nav(this.clone(), statuses, focused_key, cx))
            .children(project_section_header);

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
        // ---- 运行时标题行：挂了远程运行时才分段 ----
        // 按空间顺序出，还没有分组的运行时（刚添加、还没开项目）也要有标题，它的
        // 「打开项目」就在上面。
        // 折起的机器分段挂上组里最要紧的状态：在跑、在等你，折起来也看得见。
        let mut runtime_alerts: std::collections::HashMap<RuntimeKey, AgentStatus> =
            Default::default();
        for group in groups.iter() {
            for status in group
                .sessions
                .iter()
                .filter_map(|&i| statuses.get(i).copied())
            {
                runtime_alerts
                    .entry(group.runtime.clone())
                    .and_modify(|current| {
                        if status.rank() < current.rank() {
                            *current = status;
                        }
                    })
                    .or_insert(status);
            }
        }
        let header_for = |runtime: crate::runtimes::Runtime, grouping: Option<Div>, cx: &App| {
            let key = runtime.key().clone();
            let item_key = SidebarItem::Runtime(key.clone()).key();
            let mut extras = Vec::new();
            if focused_key == Some(item_key) {
                extras.push(focus_ring().into_any_element());
            }
            if reveal_target == Some(item_key) {
                extras.push(
                    reveal_anchor(
                        self.sidebar_reveal.clone(),
                        item_key,
                        self.sidebar_scroll.clone(),
                        this.clone(),
                    )
                    .into_any_element(),
                );
            }
            runtime_section_header(
                runtime,
                this.clone(),
                self.runtime_folded(&key),
                runtime_alerts
                    .get(&key)
                    .copied()
                    .filter(|status| *status != AgentStatus::Idle),
                grouping,
                extras,
                cx,
            )
        };
        let mut headers_left: std::collections::VecDeque<crate::runtimes::Runtime> =
            if !runtime_sections {
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
                    rows = rows.child(header_for(runtime, grouping_slot.take(), cx));
                    if reached {
                        break;
                    }
                }
            }
            // 机器分段折起来了，它下面的项目和会话都不画。
            if runtime_sections && self.runtime_folded(&group.runtime) {
                continue;
            }
            let is_project_group = grouping == SidebarGrouping::Project;
            let group_key = SidebarItem::Group(ProjectRef {
                runtime: group.runtime.clone(),
                root: group.root.clone(),
            })
            .key();
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
                // 项目行之间不留缝：跟导航、会话同一个行高，一列连续往下排。
                .when(grouping != SidebarGrouping::None, |d| d.mx_2());
            if grouping != SidebarGrouping::None {
                group_rows = group_rows.child(
                    ListRow::new(project_element_id("proj-group", &project))
                        .selected(project_header_selected)
                        // relative：右端的 + 浮层靠 absolute 定位到这行内。
                        .relative()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px(px(PROJECT_HEADER_PAD_X))
                        .h(ui_theme::height::md())
                        .rounded(ui_theme::row_radius())
                        .group(PROJ_HEADER_GROUP)
                        .when(focused_key == Some(group_key), |row| row.child(focus_ring()))
                        .when(reveal_target == Some(group_key), |row| {
                            row.child(reveal_anchor(
                                self.sidebar_reveal.clone(),
                                group_key,
                                self.sidebar_scroll.clone(),
                                this.clone(),
                            ))
                        })
                        .child(group_disclosure(
                            project_element_id("proj-disclosure", &project),
                            PROJ_HEADER_GROUP,
                            Icon::new(project_disclosure_icon(collapsed || empty_group))
                                .size(px(PROJECT_DISCLOSURE_ICON_SIZE)),
                            collapsed,
                            !empty_group,
                        ))
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
                                    // 项目名与会话同为 text_sm、常规字重：层级靠文件夹图标和
                                    // 会话的缩进表达，不靠加粗。活动项目提亮；空项目降一档到
                                    // muted，不降到 faint：它仍是能点、能新建会话的条目，faint
                                    // 读起来像禁用。
                                    div()
                                        .flex_shrink_0()
                                        .text_sm()
                                        // 项目行自己被选中（它的会话没露出来）时跟会话行一样：
                                        // 选中字色加中等字重。
                                        .when(project_header_selected, |name| name.font_medium())
                                        .text_color(rgb(if project_header_selected {
                                            ui_theme::text_selected()
                                        } else if is_active_group {
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
                                            .child(Icon::new(gpui_kit_assets::IconName::Pin).size(px(11.))),
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
                            // 槽仍是 28，计数和按钮共用一个中心。「+」本身是图标档 20。
                            // 数字只在折起来、里面又有会话时有用：展开后下面的会话一眼就
                            // 数得出来；空项目挂个「0」只是在说没有东西。
                            div()
                                .relative()
                                .flex_shrink_0()
                                .w(px(PROJECT_NEW_BUTTON_SIZE))
                                // 行高仍是 20。28 的槽在垂直方向溢出，不把项目行撑高。
                                .h(px(PROJECT_ACTION_SLOT_HEIGHT))
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .text_color(rgb(ui_theme::text_muted()))
                                        .group_hover(PROJ_HEADER_GROUP, |style| {
                                            style.opacity(0.0)
                                        })
                                        .when(collapsed && !empty_group, |d| {
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
                                            .icon(Icon::new(gpui_kit_assets::IconName::Pin))
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
            // 会话一行，缩进一档；项目模式再从文件夹图标下方落一根 hairline，
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
                // ACP 身份优先：只有 ACP 的 agent 在终端表里查不到，
                // 走 `provider` 会把它显示成一个普通终端。
                let acp_provider = self.sessions[ix].acp_kind(cx);
                let provider_label: &'static str = acp_provider
                    .map(|agent| agent.label())
                    .or_else(|| provider.map(|agent| agent.label()))
                    .unwrap_or("终端");
                let agent_mark = crate::agents::bound_agent_mark(
                    self.sessions[ix].agent_definition_id.as_deref(),
                    self.sessions[ix].automation_id.as_deref(),
                    crate::settings::agents_on(self.sessions[ix].runtime(cx).key(), cx),
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
                // 终端会话取当前活动 pane，ACP 会话取自身。分屏子行会在各自的菜单中复制自己的 ID。
                let copy_id_source = crate::CopySessionIdSource::of(&self.sessions[ix].kind);
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
                let row = ListRow::new(("sess-row", ui_id))
                    .group(SESSION_ROW_GROUP)
                    .selected(is_active)
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(ui_theme::height::md())
                    .rounded(ui_theme::row_radius())
                    .when(is_drag_source_session, |d| d.opacity(0.4))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(row_tooltip.clone()).build(window, cx)
                    })
                    .when(self.sidebar_flash == Some(ui_id), |row| {
                        row.child(new_row_flash(("sidebar-new-row", ui_id)))
                    })
                    // 选中靠整张会话卡亮起来，不再靠左侧白条。悬停底由 ListRow 铺。
                    // Grok 对话列表：小图标，不套色块。状态只给图标上色。
                    .child(session_icon)
                    // 一行：会话名占满，最近活动时间在行尾那一格里。改名时这一格换成输入框。
                    .child(
                        match rename_input.as_ref().filter(|_| renaming_session == Some(ix)) {
                            Some(input) => {
                                inline_rename_input(input, this.clone()).into_any_element()
                            }
                            // 选中照组件库侧栏：选中底、选中字色、中等字重。没选中的标题
                            // 压到弱字，悬停才提亮：选中底和悬停底在不少深色主题里是相近
                            // 的两档灰，光靠底色分不出哪行是选中、哪行只是鼠标停着。
                            None => div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .map(|title| {
                                    if is_active {
                                        title
                                            .font_medium()
                                            .text_color(rgb(ui_theme::text_selected()))
                                    } else {
                                        title.text_color(rgb(ui_theme::text_muted())).group_hover(
                                            SESSION_ROW_GROUP,
                                            |style| style.text_color(rgb(ui_theme::text())),
                                        )
                                    }
                                })
                                .truncate()
                                .child(title.clone())
                                .into_any_element(),
                        },
                    )
                    // 分屏数用行尾小角标，跟在时间前面。
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
                    .children(progress.and_then(|progress| session_progress_bar(ui_id, progress)))
                    .when(reveal_target == Some(ui_id), |row| {
                        row.child(reveal_anchor(
                            self.sidebar_reveal.clone(),
                            ui_id,
                            self.sidebar_scroll.clone(),
                            this.clone(),
                        ))
                    })
                    .when(status == AgentStatus::NeedsYou, |row| {
                        row.child(attention_anchor(self.sidebar_reveal.clone(), ui_id))
                    })
                    .when(focused_key == Some(ui_id), |row| row.child(focus_ring()))
                    // 行尾一格：要人管时写状态（不加色胶囊），否则写时间；悬停换成关闭。
                    .child(row_trailing_slot(
                        SESSION_ROW_GROUP,
                        Some(match status_label {
                            Some(label) => (label.into(), ui_theme::session_dot_color(status).into()),
                            None => (updated_text.into(), rgb(ui_theme::text_muted()).into()),
                        }),
                        Button::new(("close-session", ui_id))
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(move |_ev, window, cx| {
                                cx.stop_propagation();
                                e_close.update(cx, |ws, cx| {
                                    ws.request_close_session(ix, window, cx)
                                });
                            }),
                    ))
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
                    // 双击改名：第一下已经切过去了，第二下就地改名。
                    .on_click(move |ev, window, cx| {
                        e_act.update(cx, |ws, cx| {
                            if ev.click_count() >= 2 {
                                ws.start_rename(RenameTarget::Session(ix), window, cx);
                            } else {
                                ws.activate(ix, window, cx);
                            }
                        });
                    })
                    .context_menu(move |menu, _window, _cx| {
                        let e_rename = e_rename.clone();
                        let e_restart = e_restart.clone();
                        let copy_id_source = copy_id_source.clone();
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
                                            copy_id_source.session_id(cx),
                                        ));
                                    }),
                            );
                        // 只有 ACP 对话会话才需要「强制重启」：卡在工具调用里、
                        // 「停止」按钮打不断时的兜底，见 `force_restart_acp_session`。
                        if is_acp {
                            menu = menu.item(
                                PopupMenuItem::new("强制重启")
                                    .icon(gpui_component::Icon::new(gpui_kit_assets::IconName::RotateCw))
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
                        let copy_id_source = crate::CopySessionIdSource::Terminal(view.clone());
                        let pane_entity_id = view.entity_id();
                        let pane_key = SidebarItem::Pane(ui_id, pane_entity_id).key();
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
                            ListRow::new(("sess-pane-row", pane_entity_id))
                                .group(SESSION_ROW_GROUP)
                                .selected(is_current_view)
                                .relative()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .h(ui_theme::height::md())
                                .rounded(ui_theme::row_radius())
                                .when(is_drag_source_session, |d| d.opacity(0.4))
                                .when(p_status == AgentStatus::NeedsYou, |row| {
                                    row.child(attention_anchor(self.sidebar_reveal.clone(), ui_id))
                                })
                                .when(focused_key == Some(pane_key), |row| {
                                    row.child(focus_ring())
                                })
                                .when(reveal_target == Some(pane_key), |row| {
                                    row.child(reveal_anchor(
                                        self.sidebar_reveal.clone(),
                                        pane_key,
                                        self.sidebar_scroll.clone(),
                                        this.clone(),
                                    ))
                                })
                                .child(pane_icon)
                                .child(
                                    match rename_input
                                        .as_ref()
                                        .filter(|_| renaming_pane == Some(pane_entity_id))
                                    {
                                        Some(input) => inline_rename_input(input, this.clone())
                                            .into_any_element(),
                                        None => div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            // 当前 pane 跟会话行同口径：选中亮，其余弱字、
                                            // 悬停提亮。
                                            .map(|title| {
                                                if is_current_view {
                                                    title.font_medium().text_color(rgb(
                                                        ui_theme::text_selected(),
                                                    ))
                                                } else {
                                                    title
                                                        .text_color(rgb(ui_theme::text_muted()))
                                                        .group_hover(SESSION_ROW_GROUP, |style| {
                                                            style.text_color(rgb(ui_theme::text()))
                                                        })
                                                }
                                            })
                                            .truncate()
                                            .child(p_title)
                                            .into_any_element(),
                                    },
                                )
                                // 每个 pane 自己的关闭键：只关这一个 pane，剩下的照常活着。
                                // 跟会话行一样跟时间共用行尾那一格，悬停才换出来。
                                .child(row_trailing_slot(
                                    SESSION_ROW_GROUP,
                                    Some((
                                        pane_updated_text.into(),
                                        rgb(ui_theme::text_muted()).into(),
                                    )),
                                    Button::new(("close-pane", pane_entity_id))
                                        .dismiss()
                                        .icon(IconName::Close)
                                        .accessibility_label("关闭分栏")
                                        .on_click(move |_ev, window, cx| {
                                            cx.stop_propagation();
                                            let pane = close_pane.clone();
                                            e_pane_close.update(cx, |ws, cx| {
                                                ws.request_close_session_pane(ix, pane, window, cx)
                                            });
                                        }),
                                ))
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
                                .on_click(move |ev, window, cx| {
                                    let pane = pane.clone();
                                    e_pane_act.update(cx, |ws, cx| {
                                        if ev.click_count() >= 2 {
                                            ws.start_rename(RenameTarget::Pane(pane), window, cx);
                                        } else {
                                            ws.activate_session_pane(ix, pane, window, cx);
                                        }
                                    });
                                })
                                .context_menu(move |menu, _window, _cx| {
                                    let e_rename = e_pane_menu.clone();
                                    let e_close_all = e_pane_menu.clone();
                                    let rename_pane = menu_pane.clone();
                                    let copy_id_source = copy_id_source.clone();
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
                                                    copy_id_source.session_id(cx),
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
                        .when(self.sidebar_flash == Some(ui_id), |group| {
                            group.child(new_row_flash(("sidebar-new-row", ui_id)))
                        })
                        .child(pane_rows)
                        .when(reveal_target == Some(ui_id), |group| {
                            group.child(reveal_anchor(
                                self.sidebar_reveal.clone(),
                                ui_id,
                                self.sidebar_scroll.clone(),
                                this.clone(),
                            ))
                        })
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
            rows = rows.child(header_for(runtime, grouping_slot.take(), cx));
        }

        if hidden_empty > 0 {
            let e_show = this.clone();
            rows = rows.child(
                ListRow::new("hidden-empty-projects")
                    .mx_2()
                    .mt_2()
                    .px(px(PROJECT_HEADER_PAD_X))
                    .py(px(4.))
                    .rounded(ui_theme::row_radius())
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
        // 「添加项目」在「项目」标题行右侧，跟 Grok 一样跟分组标题同行。「设置」点一下直接
        // 开设置窗口；有要处理的事（新版本、后台服务旧了、机器要处理）时挂红点，打开就停在那一页。
        let e_settings = this.clone();
        let settings_needs_attention = self.settings_attention(cx).is_some();

        // ---- 底部额度：跟设置同一条底栏，放在右边 ----
        // provider 账户额度（Codex / Claude / Copilot / Grok / Cursor），只列「检测到额度」
        // 的。一家就是一个图标加百分比，几家都常显，不收起来：要看额度还得悬停就白放在
        // 底栏了。侧栏窄、一行放不下时折到第二行，底栏跟着长高。悬停哪块看哪块的各窗口
        // 和重置时间；点开是全部：每家一行，点哪家刷新哪家。
        // Pi 不在这里：它底下几家 provider 是独立账户，额度跟在对话模型菜单的
        // provider 名字后面（括号），那里才说得清数字属于谁。
        // 状态在这里一次性读完，弹层的构造闭包里不再碰全局。
        let quota_this = this.clone();
        let quotas: Vec<FooterQuota> = crate::provider_quota::ProviderStatus::sidebar_targets(cx)
            .into_iter()
            .filter_map(|target| {
                let status = crate::provider_quota::ProviderStatus::quota_status(target, cx)?;
                let used = status.quota.as_ref()?.used_percent();
                let row = provider_quota_row(target, &status)?;
                Some(FooterQuota {
                    target,
                    text: row.text,
                    color: row.color,
                    hint: row.hint,
                    used,
                    refreshing: crate::provider_quota::ProviderStatus::is_manually_refreshing(
                        target, cx,
                    ),
                })
            })
            .collect();
        let quota_footer: Option<AnyElement> = if quotas.is_empty() {
            // 完全没有额度数据：只留「查询中」过渡；否则不占位。
            crate::provider_quota::ProviderStatus::is_refreshing(cx).then(|| {
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .font_family(crate::terminal_view::font_family())
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("额度查询中…")
                    .into_any_element()
            })
        } else {
            // 能折行的触发器：按钮高度固定折不了行，换成侧栏的列表行（悬停、弹层开着时的
            // 选中底都跟别的行一样）。
            let trigger = ListRow::new("sidebar-quota")
                .min_w_0()
                .px_1p5()
                .py_1()
                .rounded(ui_theme::row_radius())
                .flex()
                .flex_wrap()
                .justify_end()
                .items_center()
                .gap_x_2()
                .gap_y_1()
                .text_xs()
                .font_family(crate::terminal_view::font_family())
                .children(quotas.iter().map(|quota| {
                    let tip = if quota.refreshing {
                        format!("{} 正在刷新…", quota.hint)
                    } else {
                        quota.hint.clone()
                    };
                    h_flex()
                        .id(SharedString::from(format!(
                            "quota-chip-{}",
                            quota.target.id()
                        )))
                        .flex_shrink_0()
                        .gap_1()
                        .items_center()
                        .text_color(rgb(quota.color))
                        .child(acp_provider_icon(quota.target.agent).size(px(12.)))
                        .map(|chip| {
                            if quota.refreshing {
                                chip.child(smelt_ui::motion::ambient_spinner(
                                    SharedString::from(format!(
                                        "quota-chip-refresh-{}",
                                        quota.target.id()
                                    )),
                                    rgb(quota.color).into(),
                                    true,
                                ))
                            } else {
                                chip.child(quota.text.clone())
                            }
                        })
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                }));
            Some(
                gpui_component::popover::Popover::new("sidebar-quota-popover")
                    .anchor(Anchor::BottomRight)
                    .w(px(QUOTA_POPOVER_WIDTH))
                    .p_0()
                    // 弹层的触发容器要能收窄，额度才会在里面折行，不把底栏撑出侧栏。
                    .trigger_style(StyleRefinement::default().min_w_0())
                    .trigger(trigger)
                    .content(move |_, _window, _cx| {
                        quota_popover(quotas.clone(), quota_this.clone())
                    })
                    .into_any_element(),
            )
        };

        // 底栏一条：左边设置，右边额度。设置是普通入口，不做成通栏的大按钮。
        let footer = div()
            .flex_shrink_0()
            .min_h(px(44.))
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .border_t_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .id("sidebar-settings")
                    .relative()
                    .flex_shrink_0()
                    .child(
                        Button::new("sidebar-settings-btn")
                            .quiet()
                            .icon(IconName::Settings)
                            .label("设置")
                            .on_click(move |_, _, cx| {
                                e_settings.update(cx, |ws, cx| ws.open_settings(cx));
                            }),
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
            )
            .child(div().flex_1())
            .children(quota_footer);

        div()
            .w_full()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            // 外层工作区负责玻璃表面、描边与裁切；列表自身保持透明，才能让
            // vibrancy / 半透明底贯穿整个面板而不是只透出四角。
            .bg(gpui::transparent_black())
            // 顶部固定的只有几个入口；「对话」和项目都在下面的滚动区里。列表滚动过以后
            // 固定区下沿才出一条细线，平时不画（线一直占着 1px，出现时不跳）。
            .child(
                self.render_automation_navigation(this.clone(), statuses, cx)
                    .border_b_1()
                    .border_color(if self.sidebar_scroll.offset().y < px(0.) {
                        Hsla::from(ui_theme::hairline())
                    } else {
                        gpui::transparent_black()
                    }),
            )
            .child(
                div()
                    .id("session-rows-pane")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(rows)
                    .vertical_scrollbar(&self.sidebar_scroll)
                    // 放在所有行后面：行都量完了才算视口外的「需要你」。
                    .child(attention_checker(
                        self.sidebar_reveal.clone(),
                        self.sidebar_scroll.clone(),
                        this.clone(),
                    ))
                    .children(offscreen.above.map(|target| {
                        attention_pill(true, target, self.sidebar_reveal.clone(), this.clone())
                    }))
                    .children(offscreen.below.map(|target| {
                        attention_pill(false, target, self.sidebar_reveal.clone(), this.clone())
                    })),
            )
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
