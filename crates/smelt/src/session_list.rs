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

use chrono::Local;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
use smelt_ui::motion::ambient_animation;
use std::time::Duration;

use crate::git_panel::main_repo_root_from_common_dir;
// drag.rs / row.rs 作为本模块子模块沿用 `super::*` 读取 ACP 类型。
use crate::settings::ConversationAgentKind;
use crate::{
    AgentStatus, RenameTarget, SessionKind, SidebarGrouping, Workspace, pane_provider_kind,
    pane_status, pane_title, ui_theme,
};

/// 会话行 hover group 名：行 `.group()` + 右端操作条 `.group_hover()` 配对，
/// 鼠标移到行才显形关闭按钮；关闭操作不占会话正文宽度。
const SESS_ROW_GROUP: &str = "sess-row-hover";

/// 分屏 pane 行自己的 hover group 名：每行右端的「关掉这个 pane」按钮靠它显形。
/// 必须跟 SESS_ROW_GROUP 分开——共用一个名字的话 hover 组内任意位置，所有 pane 行的
/// × 会一起亮，用户分不清点下去关的是哪一个（正是「关一个 pane 结果两个都没了」的来源）。
const PANE_ROW_GROUP: &str = "sess-pane-row-hover";

/// 项目标题行的 hover group 名：整行 `.group()` + 左端文件夹 `.group_hover()`。
/// 整行点击展开/折叠；文件夹只表达项目身份和开合状态，hover 整行时同步提亮。
const PROJ_HEADER_GROUP: &str = "proj-header-hover";

/// 项目标题与会话列表之间的固定层级尺寸。集中声明是为了避免项目头、会话缩进和
/// 新建按钮各自改尺寸后失去对齐关系。
const PROJECT_HEADER_FONT_SIZE: f32 = 13.;
const PROJECT_NEW_BUTTON_SIZE: f32 = 28.;
const PROJECT_ACTION_SLOT_HEIGHT: f32 = 20.;
const PROJECT_DISCLOSURE_ICON_SIZE: f32 = 15.;
const PROJECT_SESSION_INDENT: f32 = 22.;
const PROJECT_GUIDE_LEFT: f32 = 18.;
const SESSION_AGENT_ICON_BOX_SIZE: f32 = 18.;
const SESSION_AGENT_ICON_SIZE: f32 = 17.;
pub(crate) const SESSION_GLOW_PERIOD: Duration = Duration::from_secs(2);

fn project_disclosure_icon(collapsed_or_empty: bool) -> IconName {
    if collapsed_or_empty {
        IconName::Folder
    } else {
        IconName::FolderOpen
    }
}

fn shared_project_action_payload(
    payloads: &[smelt_plugin_api::SessionActionInvocationPayload],
) -> Option<smelt_plugin_api::SessionActionInvocationPayload> {
    payloads
        .first()
        .cloned()
        .filter(|first| payloads.iter().all(|candidate| candidate == first))
}

fn entity_decoration_color(tone: smelt_plugin_api::EntityDecorationTone, cx: &App) -> Hsla {
    match tone {
        smelt_plugin_api::EntityDecorationTone::Neutral => cx.theme().muted_foreground,
        smelt_plugin_api::EntityDecorationTone::Accent => rgb(ui_theme::accent()).into(),
        smelt_plugin_api::EntityDecorationTone::Positive => rgb(ui_theme::green()).into(),
        smelt_plugin_api::EntityDecorationTone::Warning => cx.theme().warning,
        smelt_plugin_api::EntityDecorationTone::Negative => cx.theme().danger,
    }
}

pub(crate) fn entity_decoration_chip(
    id: impl Into<ElementId>,
    decoration: &crate::plugin_ui::PluginEntityDecorationPresentation,
    show_icon: bool,
    cx: &App,
) -> Stateful<Div> {
    let color = entity_decoration_color(decoration.tone, cx);
    let icon = show_icon
        .then(|| {
            decoration
                .icon_asset
                .as_ref()
                .map(|path| Icon::empty().path(path.clone()))
                .or_else(|| {
                    decoration
                        .external_url
                        .as_ref()
                        .map(|_| Icon::new(IconName::ExternalLink))
                })
        })
        .flatten();
    let tooltip = decoration.tooltip.clone();
    let external_url = decoration.external_url.clone().filter(|url| {
        url::Url::parse(url)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
    });
    div()
        .id(id)
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .text_color(color)
        .children(icon.map(|icon| icon.size(px(12.))))
        .children(decoration.badge.as_ref().map(|badge| {
            div()
                .max_w(px(72.))
                .truncate()
                .px_1()
                .py(px(1.))
                .rounded(px(3.))
                .border_1()
                .border_color(color.opacity(0.45))
                .bg(color.opacity(0.08))
                .text_size(px(9.))
                .child(badge.clone())
        }))
        .when_some(tooltip, |chip, tooltip| {
            chip.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
        })
        .when_some(external_url, |chip, url| {
            chip.cursor_pointer()
                .hover(|d| d.opacity(0.8))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                })
        })
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
    acp_provider_icon, effective_pane_updated_at, plugin_agent_icon, plugin_session_action_icon,
    project_header_is_selected, provider_icon, provider_quota_row, session_row_is_selected,
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
    /// 当前活动上下文的 **root 路径**：优先用用户点选的 `active_project`，该项目
    /// 已被关掉则回退到活动会话所在组，再回退第一组。
    ///
    /// 打开智能体对话时会把它的工作目录写进 `active_project`，于是「点项目行」
    /// 与「开对话」共用同一个上下文变量，后操作的那个生效。
    pub(crate) fn active_project_root(&self, cx: &App) -> Option<String> {
        let groups = self.project_groups(cx);
        let selected = self.active_project.as_deref();
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
    }

    /// `root` 是否是某段仍然开着的智能体对话的工作目录。
    fn agent_conversation_workspace_is_open(&self, root: &str, cx: &App) -> bool {
        smelt_core::session_control::is_agent_conversation_cwd(root)
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
        let project_groups = self.project_groups(cx);
        let active_root = self.active_project_root(cx);

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
        for group in &project_groups {
            let branch = self
                .repo_info
                .get(group.root.as_str())
                .and_then(|(_, info)| info.as_ref())
                .map(|info| info.branch.clone());
            for &ix in &group.sessions {
                session_projects[ix] = Some((group.label.clone(), branch.clone()));
            }
        }
        let mut groups = crate::sidebar_groups(
            self.sidebar_grouping,
            project_groups,
            statuses,
            &last_updated_at,
            self.sessions.len(),
        );
        if self.sidebar_grouping == SidebarGrouping::Project {
            groups = crate::sidebar_order::pinned_projects_first(
                groups,
                &self.pinned_projects,
                |group| group.root.as_str(),
            );
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
            Some(crate::SidebarDrag::Project(root)) => Some(root.clone()),
            _ => None,
        };
        let e_filter = this.clone();
        let header = crate::workspace_frame::with_window_drag(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .pt_3()
                .pb_2()
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .children(
                            [(false, "全部"), (true, "有会话")]
                                .into_iter()
                                .enumerate()
                                .map(|(ix, (hide, label))| {
                                    let selected = hide_empty == hide;
                                    let entity = e_filter.clone();
                                    div()
                                        .id(("sidebar-empty-filter", ix))
                                        .px_2()
                                        .py(px(3.))
                                        .rounded_full()
                                        .text_xs()
                                        .cursor_pointer()
                                        .text_color(rgb(if selected {
                                            ui_theme::text()
                                        } else {
                                            ui_theme::text_faint()
                                        }))
                                        .when(selected, |d| d.bg(rgb(ui_theme::bg_hover())))
                                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                                        .child(label)
                                        .on_click(move |_ev, _window, cx| {
                                            entity.update(cx, |ws, cx| {
                                                if ws.sidebar_hide_empty_projects != hide {
                                                    ws.sidebar_hide_empty_projects = hide;
                                                    ws.save_state(cx);
                                                    cx.notify();
                                                }
                                            });
                                        })
                                }),
                        ),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new("session-grouping")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Settings)
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let menu = menu.item(PopupMenuItem::label("分组依据"));
                                    let add_item = |
                                    menu: PopupMenu,
                                    mode: SidebarGrouping,
                                    label: &'static str,
                                    entity: Entity<Workspace>,
                                | {
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
                                    let menu = add_item(
                                        menu,
                                        SidebarGrouping::Project,
                                        "项目",
                                        e_group.clone(),
                                    );
                                    let menu = add_item(
                                        menu,
                                        SidebarGrouping::Status,
                                        "状态",
                                        e_group.clone(),
                                    );
                                    add_item(
                                        menu,
                                        SidebarGrouping::LastUpdated,
                                        "时间",
                                        e_group.clone(),
                                    )
                                }),
                        ),
                ),
        );

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
            });

        let mut hidden_empty = 0usize;
        let mut visible_ix = 0usize;
        for (pix, group) in groups.iter().enumerate() {
            let is_project_group = grouping == SidebarGrouping::Project;
            // ---- 项目分组标题行 ----
            // 分组身份一律用 root 路径（末段同名的两个目录是两个项目，见 ProjectGroup）；
            // name 只是显示用的。
            let cwd = &group.root;
            let name = &group.label;
            let ixs = &group.sessions;
            let collapsed = self.collapsed_projects.contains(cwd);
            let is_active_group = active_root
                .as_deref()
                .is_some_and(|root| crate::sidebar_order::same_project_root(cwd, root));
            let empty_group = ixs.is_empty();
            let is_pinned = is_project_group && self.is_project_pinned(cwd);
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

            let repo_info_here = self
                .repo_info
                .get(cwd.as_str())
                .and_then(|(_, i)| i.clone());
            let is_worktree_group = repo_info_here.as_ref().is_some_and(|i| i.is_worktree());
            let worktree_main_root = repo_info_here
                .as_ref()
                .and_then(|i| main_repo_root_from_common_dir(&i.common_dir))
                .unwrap_or_else(|| cwd.clone());
            let worktree_branch = repo_info_here
                .as_ref()
                .map(|i| i.branch.clone())
                .unwrap_or_default();
            // 分支名：worktree / 普通仓库都显示，跟项目名并排（淡色）。
            let branch_label = repo_info_here.as_ref().map(|i| i.branch.clone());
            let plugin_agent = if is_project_group {
                ixs.iter()
                    .find_map(|&session_ix| self.sessions[session_ix].plugin_agent_presentation(cx))
            } else {
                None
            };
            let project_action_payload = if is_project_group {
                let payloads = ixs
                    .iter()
                    .filter_map(|&session_ix| match &self.sessions[session_ix].kind {
                        SessionKind::Conversation(view) => view.read(cx).session_action_payload(),
                        SessionKind::Term { .. } => None,
                    })
                    .collect::<Vec<_>>();
                shared_project_action_payload(&payloads)
            } else {
                None
            };
            let project_session_actions = project_action_payload
                .as_ref()
                .map(|payload| {
                    crate::plugin_ui::session_actions(
                        &payload.agent_session,
                        smelt_plugin_api::SessionActionLocation::ProjectMenu,
                    )
                })
                .unwrap_or_default();
            let project_decorations = project_action_payload
                .as_ref()
                .map(|payload| {
                    crate::plugin_ui::entity_decorations(&payload.agent_session.instance, cx)
                })
                .unwrap_or_default();

            let e_toggle = this.clone();
            let toggle_root = cwd.clone();
            let e_menu = this.clone();
            let menu_cwd = if is_project_group {
                cwd.clone()
            } else {
                active_root.clone().unwrap_or_default()
            };
            let group_name: SharedString = name.clone().into();
            let drag_root: SharedString = cwd.clone().into();
            let is_drag_source_project = dragging_project.as_ref().is_some_and(|root| {
                crate::sidebar_order::normalize_project_root(root)
                    == crate::sidebar_order::normalize_project_root(cwd)
            });
            let proj_hint_before = can_drag
                && !is_drag_source_project
                && self.proj_drop_hint.as_ref() == Some(&(cwd.clone(), true));
            let proj_hint_after = can_drag
                && !is_drag_source_project
                && self.proj_drop_hint.as_ref() == Some(&(cwd.clone(), false));
            let mut group_rows = div()
                .id(("proj-group-rows", pix))
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
                        .id(("proj-group", pix))
                        // relative：右端的 + 浮层靠 absolute 定位到这行内。
                        .relative()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_3()
                        .py(px(4.))
                        .rounded(ui_theme::row_radius())
                        .cursor_pointer()
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
                                .id(("proj-disclosure", pix))
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
                                    // 项目名与会话同为 13px，再用 semibold 区分结构层级；
                                    // 空项目继续降到 faint，避免和活跃项目争夺注意力。
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(PROJECT_HEADER_FONT_SIZE))
                                        .font_semibold()
                                        .text_color(rgb(if is_active_group {
                                            ui_theme::text_bright()
                                        } else if crate::sidebar_order::project_header_is_faint(
                                            is_active_group,
                                            empty_group,
                                        ) {
                                            ui_theme::text_faint()
                                        } else {
                                            ui_theme::text_mid()
                                        }))
                                        .child(group_name.clone()),
                                )
                                .when(is_pinned, |row| {
                                    row.child(
                                        div()
                                            .id(("proj-pin", pix))
                                            .flex_shrink_0()
                                            .text_color(rgb(ui_theme::text_faint()))
                                            .child(Icon::new(IconName::Star).size(px(11.))),
                                    )
                                })
                                .when(agg != AgentStatus::Idle, |row| {
                                    row.child(
                                        div()
                                            .id(("proj-status-dot", pix))
                                            .flex_shrink_0()
                                            .size(px(6.))
                                            .rounded_full()
                                            .bg(ui_theme::session_dot_color(agg)),
                                    )
                                })
                                .children(plugin_agent.as_ref().map(|agent| {
                                    let agent_name = agent.name.clone();
                                    div()
                                        .id(("proj-plugin-agent-logo", pix))
                                        .flex_shrink_0()
                                        .size(px(15.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(gpui::rgb(ui_theme::text_muted()))
                                        .child(plugin_agent_icon(agent).size(px(14.)))
                                        .tooltip(move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                agent_name.clone(),
                                            )
                                            .build(window, cx)
                                        })
                                }))
                                .children(project_decorations.iter().take(2).enumerate().map(
                                    |(decoration_ix, decoration)| {
                                        entity_decoration_chip(
                                            ("proj-entity-decoration", pix * 4 + decoration_ix),
                                            decoration,
                                            plugin_agent.is_none(),
                                            cx,
                                        )
                                    },
                                ))
                                .children(branch_label.as_ref().map(|b| {
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .font_family(crate::terminal_view::font_family())
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .child(b.clone())
                                })),
                        )
                        .child(
                            // 数字和「+」不是两套右对齐元素，而是同一个方形槽里的
                            // 两个满铺图层；hover 只切透明度，因此字形中心不会横跳。
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
                                        .text_size(px(11.))
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .when(empty_group, |d| d.opacity(0.55))
                                        .group_hover(PROJ_HEADER_GROUP, |style| {
                                            style.opacity(0.0)
                                        })
                                        .child(ixs.len().to_string()),
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
                                                pix,
                                                e_menu.clone(),
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
                                    root: drag_root.clone(),
                                    label: group_name.clone(),
                                    collapsed,
                                    empty: empty_group,
                                    branch: branch_label
                                        .as_ref()
                                        .map(|branch| SharedString::from(branch.clone())),
                                    agg,
                                    plugin_agent: plugin_agent.clone(),
                                    session_count: ixs.len(),
                                    grab: Point::default(),
                                },
                                this.clone(),
                            )
                        })
                        .on_mouse_up(MouseButton::Left, move |_ev, window, cx| {
                            let root = toggle_root.clone();
                            e_toggle.update(cx, |ws, cx| {
                                // 整行也挂了 on_drag：超过阈值起拖后不要再当成点选。
                                if ws.sidebar_drag.is_some() {
                                    return;
                                }
                                ws.select_project(root.clone(), window, cx);
                                if crate::sidebar_order::project_header_click_toggles_collapse(
                                    !empty_group,
                                ) {
                                    ws.toggle_project_collapsed(&root, cx);
                                }
                            });
                        })
                        .context_menu({
                            // 复制路径人人有份；「删除 Worktree」只给 worktree 分组
                            //（`when` 的两个分支类型必须一致，所以是菜单项按条件加，
                            // 不是整个 context_menu 按条件挂）。
                            let e_del = e_menu.clone();
                            let e_pin = e_menu.clone();
                            let e_sidebar_pin = e_menu.clone();
                            let e_close_proj = e_menu.clone();
                            let e_plugin_action = e_menu.clone();
                            let project_action_payload = project_action_payload.clone();
                            let project_session_actions = project_session_actions.clone();
                            let path = cwd.clone();
                            let close_root = cwd.clone();
                            let sess_n = ixs.len();
                            let del_main_root = worktree_main_root.clone();
                            let del_branch = worktree_branch.clone();
                            let copy_branch = branch_label.clone();
                            move |menu, window, cx| {
                                if !is_project_group {
                                    return menu;
                                }
                                let copy_path = path.clone();
                                let del_path = path.clone();
                                let pin_path = path.clone();
                                let sidebar_pin_path = path.clone();
                                let wt_root = path.clone();
                                let e_wt = e_menu.clone();
                                let e_del = e_del.clone();
                                let e_pin = e_pin.clone();
                                let e_sidebar_pin = e_sidebar_pin.clone();
                                let e_close_proj = e_close_proj.clone();
                                let e_plugin_action = e_plugin_action.clone();
                                let project_action_payload = project_action_payload.clone();
                                let project_session_actions = project_session_actions.clone();
                                let close_root = close_root.clone();
                                let del_main_root = del_main_root.clone();
                                let del_branch = del_branch.clone();
                                let copy_branch = copy_branch.clone();
                                // 「新增 Worktree」用的主仓库根/基准分支：独立 clone，
                                // 免得和后面「删除 Worktree」的 when 闭包抢同一个变量。
                                let nwt_main_root = del_main_root.clone();
                                let nwt_base_branch = del_branch.clone();
                                let e_nwt = e_menu.clone();
                                // 已 pin → 显示「从文件树移除」，否则「加到文件树」（当前活动项目
                                // 天然在文件树里，pin 它=切走后仍保留，所以照样给这个开关）。
                                let pinned = e_pin.read(cx).is_file_tree_root_pinned(&pin_path);
                                let pin_label = if pinned {
                                    "从文件树移除"
                                } else {
                                    "加到文件树"
                                };
                                let sidebar_pinned =
                                    e_sidebar_pin.read(cx).is_project_pinned(&sidebar_pin_path);
                                let sidebar_pin_label = if sidebar_pinned {
                                    "取消置顶"
                                } else {
                                    "置顶"
                                };
                                // 按「复制 / Worktree / 项目 / 离开」分段；Worktree 是较少用
                                // 的高级操作，收进二级菜单，避免把常用项目操作挤成一长列。
                                let mut menu = menu;
                                if let Some(payload) = project_action_payload {
                                    let has_actions = !project_session_actions.is_empty();
                                    for action in project_session_actions {
                                        let e_action = e_plugin_action.clone();
                                        let payload = payload.clone();
                                        let action_for_click = action.clone();
                                        let mut item = PopupMenuItem::new(action.title.clone());
                                        if let Some(icon) = plugin_session_action_icon(action.icon) {
                                            item = item.icon(icon);
                                        }
                                        menu = menu.item(item.on_click(move |_ev, window, cx| {
                                            e_action.update(cx, |ws, cx| {
                                                ws.run_plugin_session_action(
                                                    action_for_click.clone(),
                                                    payload.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }));
                                    }
                                    if has_actions {
                                        menu = menu.separator();
                                    }
                                }

                                let open_root = copy_path.clone();
                                let e_open = e_menu.clone();
                                menu = menu
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
                                    .submenu_with_icon(
                                        Some(IconName::HardDrive.into()),
                                        "Worktree",
                                        window,
                                        cx,
                                        move |menu, _window, _cx| {
                                            let worktree_root = wt_root.clone();
                                            let worktree_workspace = e_wt.clone();
                                            let new_worktree_root = nwt_main_root.clone();
                                            let new_worktree_branch = nwt_base_branch.clone();
                                            let new_worktree_workspace = e_nwt.clone();
                                            menu.item(
                                                PopupMenuItem::new("查看关联的 Worktree...")
                                                    .icon(IconName::HardDrive)
                                                    .on_click(move |_ev, _window, cx| {
                                                        worktree_workspace.update(cx, |ws, cx| {
                                                            ws.open_worktree_list(
                                                                worktree_root.clone(),
                                                                cx,
                                                            )
                                                        });
                                                    }),
                                            )
                                            .item(
                                                PopupMenuItem::new("新建 Worktree...")
                                                    .icon(IconName::Plus)
                                                    .on_click(move |_ev, window, cx| {
                                                        new_worktree_workspace.update(
                                                            cx,
                                                            |ws, cx| {
                                                                ws.open_new_worktree(
                                                                    new_worktree_root.clone(),
                                                                    new_worktree_branch.clone(),
                                                                    window,
                                                                    cx,
                                                                )
                                                            },
                                                        );
                                                    }),
                                            )
                                        },
                                    )
                                    .separator()
                                    .item(
                                        PopupMenuItem::new(sidebar_pin_label)
                                            .icon(IconName::Star)
                                            .on_click(move |_ev, _window, cx| {
                                                let sidebar_pin_path = sidebar_pin_path.clone();
                                                e_sidebar_pin.update(cx, |ws, cx| {
                                                    ws.toggle_project_pinned(
                                                        sidebar_pin_path,
                                                        cx,
                                                    )
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
                                        .icon(IconName::CircleX)
                                        .on_click(
                                            move |_ev, _window, cx| {
                                                let root = close_root.clone();
                                                e_close_proj.update(cx, |ws, cx| {
                                                    ws.start_close_project(root, cx)
                                                });
                                            },
                                        ),
                                    );

                                menu.when(is_worktree_group, move |menu| {
                                    menu.separator().item(
                                        PopupMenuItem::new("删除 Worktree")
                                            .icon(IconName::Delete)
                                            .on_click(move |_ev, _window, cx| {
                                                let del_path = del_path.clone();
                                                let del_main_root = del_main_root.clone();
                                                let del_branch = del_branch.clone();
                                                e_del.update(cx, |ws, cx| {
                                                    ws.start_delete_worktree(
                                                        del_path,
                                                        del_main_root,
                                                        del_branch,
                                                        cx,
                                                    )
                                                });
                                            }),
                                    )
                                })
                            }
                        })
                        .when(can_drag && dragging, |header| {
                            attach_project_drop_layers(
                                header,
                                pix,
                                drag_root.clone(),
                                proj_hint_before,
                                proj_hint_after,
                                this.clone(),
                            )
                        }),
                );
            }

            if collapsed {
                if grouping != SidebarGrouping::None {
                    rows = rows.child(group_rows);
                }
                continue;
            }

            if ixs.is_empty() {
                // 空项目不占一行说明。Grok 的空分组就是没有条目，新建走标题上的 +。
                rows = rows.child(group_rows);
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
                let plugin_agent = self.sessions[ix].plugin_agent_presentation(cx);
                let identity_label = plugin_agent
                    .as_ref()
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| provider_label.to_string());
                let session_action_payload = match &self.sessions[ix].kind {
                    SessionKind::Conversation(view) => view.read(cx).session_action_payload(),
                    SessionKind::Term { .. } => None,
                };
                let session_actions = session_action_payload
                    .as_ref()
                    .map(|payload| {
                        crate::plugin_ui::session_actions(
                            &payload.agent_session,
                            smelt_plugin_api::SessionActionLocation::SessionMenu,
                        )
                    })
                    .unwrap_or_default();
                let session_decorations = session_action_payload
                    .as_ref()
                    .map(|payload| {
                        crate::plugin_ui::entity_decorations(&payload.agent_session.instance, cx)
                    })
                    .unwrap_or_default();
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
                let session_icon = if animate_running && status == AgentStatus::Running {
                    let plugin_agent = plugin_agent.clone();
                    ambient_animation(
                        ("sess-provider-glow", ix),
                        SESSION_GLOW_PERIOD,
                        true,
                        move |phase| {
                            let tooltip_label = identity_label.clone();
                            div()
                                .id(("sess-provider-icon", ix))
                                .flex_shrink_0()
                                .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(Self::session_icon_color(status, Some(phase)))
                                .child(
                                    plugin_agent
                                        .as_ref()
                                        .map(plugin_agent_icon)
                                        .unwrap_or_else(|| {
                                            acp_provider
                                                .map(acp_provider_icon)
                                                .unwrap_or_else(|| provider_icon(provider))
                                        })
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
                        .id(("sess-provider-icon", ix))
                        .flex_shrink_0()
                        .size(px(SESSION_AGENT_ICON_BOX_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(Self::session_icon_color(status, None))
                        .child(
                            plugin_agent
                                .as_ref()
                                .map(plugin_agent_icon)
                                .unwrap_or_else(|| {
                                    acp_provider
                                        .map(acp_provider_icon)
                                        .unwrap_or_else(|| provider_icon(provider))
                                })
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
                    .id(("sess-row", ix))
                    .group(SESS_ROW_GROUP)
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(6.))
                    .rounded(ui_theme::row_radius())
                    .cursor_pointer()
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
                                    .text_size(px(13.))
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
                                    .text_size(px(10.))
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .truncate()
                                    .child(updated_text),
                            ),
                    )
                    .children(session_decorations.iter().take(2).enumerate().map(
                        |(decoration_ix, decoration)| {
                            entity_decoration_chip(
                                ("sess-entity-decoration", ix * 4 + decoration_ix),
                                decoration,
                                plugin_agent.is_none(),
                                cx,
                            )
                        },
                    ))
                    // 分屏数继续用行尾小角标；第二行固定留给跨会话可比较的时间。
                    .children(subtitle.as_ref().map(|s| {
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .font_family(crate::terminal_view::font_family())
                            .text_color(rgb(ui_theme::text_faint()))
                            .child(s.clone())
                    }))
                    // 按状态 / 不分组时，项目标题已经不存在，把来源项目与分支补到行内。
                    .children(project_context.map(|(project, branch)| {
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1p5()
                            .py(px(1.))
                            .rounded_full()
                            .bg(ui_theme::overlay(0x14))
                            .text_size(px(10.))
                            .text_color(rgb(ui_theme::text_muted()))
                            .child(project)
                            .children(branch.map(|name| {
                                div()
                                    .font_family(crate::terminal_view::font_family())
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .child(name)
                            }))
                    }))
                    // 只要人管才写字，不加色胶囊。
                    .children(status_label.map(|label| {
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(ui_theme::session_dot_color(status))
                            .child(label)
                    }))
                    .child(
                        // 标题占据中间可用宽度，关闭按钮作为唯一右侧操作贴行尾显示。
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .opacity(0.0)
                            .group_hover(SESS_ROW_GROUP, |s| s.opacity(1.0))
                            .child(
                                // 关闭键不用 ghost Button：它的 hover 底是
                                // secondary(bg_card).lighten(0.1)，压在选中行的
                                // bg_selected 上比底色还暗——鼠标压上去等于没反应，
                                // 且 ghost 的图标色 hover 前后不变。这里自己画，hover **只把
                                // 图标转红、不加底**：平时可见的只有 14px 图标，一加 20px 的
                                // hover 底就像整个键突然撑大一圈（布局没变，但眼睛就是这么读的）。
                                // 灰→红本身已经是够强的反馈，也把「这是关掉」说清楚了。
                                div()
                                    .id(("close-session", ix))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_muted()))
                                    .hover(|d| d.text_color(rgb(ui_theme::red())))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(Icon::new(IconName::CircleX).size(px(14.)))
                                    .on_click(move |_ev, _w, cx| {
                                        cx.stop_propagation();
                                        e_close.update(cx, |ws, cx| ws.close_session(ix, cx));
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
                                plugin_agent: plugin_agent.clone(),
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
                        let e_plugin_action = e_rename.clone();
                        let copy_session_id = session_id.clone();
                        let session_action_payload = session_action_payload.clone();
                        let session_actions = session_actions.clone();
                        let sess_ix = ix;
                        let mut menu = menu
                            .item(
                                PopupMenuItem::new("重命名").on_click(move |_ev, window, cx| {
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
                            menu = menu.item(PopupMenuItem::new("强制重启").on_click(
                                move |_ev, _window, cx| {
                                    e_restart.update(cx, |ws, cx| {
                                        ws.force_restart_acp_session(sess_ix, cx)
                                    });
                                },
                            ));
                        }
                        if let Some(payload) = session_action_payload {
                            if !session_actions.is_empty() {
                                menu = menu.separator();
                            }
                            for action in session_actions {
                                let e_action = e_plugin_action.clone();
                                let payload = payload.clone();
                                let action_for_click = action.clone();
                                let mut item = PopupMenuItem::new(action.title.clone());
                                if let Some(icon) = plugin_session_action_icon(action.icon) {
                                    item = item.icon(icon);
                                }
                                menu = menu.item(item.on_click(move |_ev, window, cx| {
                                    e_action.update(cx, |ws, cx| {
                                        ws.run_plugin_session_action(
                                            action_for_click.clone(),
                                            payload.clone(),
                                            window,
                                            cx,
                                        )
                                    });
                                }));
                            }
                        }
                        menu
                    })
                    .when(can_drag && dragging, |row| {
                        attach_session_drop_layers(
                            row,
                            ix,
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
                    group_body = group_body.child(row);
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
                        let pane_icon_id = ix * 100 + lix;
                        let pane_icon = if animate_running && p_status == AgentStatus::Running {
                            ambient_animation(
                                ("pane-provider-glow", pane_icon_id),
                                SESSION_GLOW_PERIOD,
                                true,
                                move |phase| {
                                    let tooltip_label = pane_tooltip.clone();
                                    div()
                                        .id(("pane-provider-icon", pane_icon_id))
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
                                .id(("pane-provider-icon", pane_icon_id))
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
                                .id(("sess-pane-row", ix * 100 + lix))
                                .group(PANE_ROW_GROUP)
                                .relative()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py(px(6.))
                                .rounded(ui_theme::row_radius())
                                .cursor_pointer()
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
                                                .text_size(px(13.))
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
                                                .text_size(px(10.))
                                                .text_color(rgb(ui_theme::text_faint()))
                                                .truncate()
                                                .child(pane_updated_text),
                                        ),
                                )
                                // 每个 pane 自己的关闭键：只关这一个 pane，剩下的照常活着。
                                // 常态透明，hover 本行才显形（本行 group，不是整组）。
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .opacity(0.0)
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .group_hover(PANE_ROW_GROUP, |s| s.opacity(1.0))
                                        .child(
                                            Button::new(("close-pane", ix * 100 + lix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::CircleX)
                                                .on_click(move |_ev, window, cx| {
                                                    cx.stop_propagation();
                                                    let pane = close_pane.clone();
                                                    e_pane_close.update(cx, |ws, cx| {
                                                        ws.close_session_pane(ix, pane, window, cx)
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
                                            plugin_agent: plugin_agent.clone(),
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
                                    menu.item(PopupMenuItem::new("重命名").on_click(
                                        move |_ev, window, cx| {
                                            let target = RenameTarget::Pane(rename_pane.clone());
                                            e_rename.update(cx, |ws, cx| {
                                                ws.start_rename(target, window, cx)
                                            });
                                        },
                                    ))
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
                                        PopupMenuItem::new("关闭整个会话（含全部分屏）").on_click(
                                            move |_ev, _window, cx| {
                                                e_close_all
                                                    .update(cx, |ws, cx| ws.close_session(ix, cx));
                                            },
                                        ),
                                    )
                                })
                                .when(can_drag && dragging, |row| {
                                    attach_session_drop_layers(
                                        row,
                                        ix * 100 + lix,
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
                        .id(("sess-pane-group", ix))
                        .group(SESS_ROW_GROUP)
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

                    group_body = group_body.child(pane_group);
                }
            }
            rows = rows.child(group_rows.child(group_body));
        }

        if hidden_empty > 0 {
            let e_show = this.clone();
            rows = rows.child(
                div()
                    .id("hidden-empty-projects")
                    .mx_2()
                    .mt_2()
                    .px_3()
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
                    .text_size(px(11.))
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(format!("已隐藏 {hidden_empty} 个无会话项目")),
            );
        }

        // ---- 底部操作条：设置 + 打开项目 ----
        let e_open = this.clone();
        let e_settings = this.clone();
        let settings_needs_attention =
            self.update_available() || self.daemon_outdated == Some(true);
        let account_menu_entries = crate::settings::sidebar_account_menu_entries(cx);

        // ---- 底部额度条：provider 账户额度（Codex / Claude / Copilot / Grok / Cursor）----
        // 从舞台头栏迁到这里（底部操作条上方）。只显示「检测到额度」的 provider，
        // 全部挤在一行：每项 = provider 图标 + 最差窗口的已用百分比（颜色也由它
        // 决定）；悬浮单项看完整窗口明细与重置时间；点击整条刷新。
        let quota_this = this.clone();
        let quota_state = &self.provider_quota_state;
        // (provider 身份, 行内百分比文本, 行内颜色, tooltip 完整文案)
        let quota_rows = crate::provider_quota::quota_provider_kinds()
            .filter_map(|provider| {
                let row = provider_quota_row(provider, quota_state.status(provider)?)?;
                Some((provider, row.text, row.color, row.hint))
            })
            .collect::<Vec<_>>();

        let quota_footer = if quota_rows.is_empty() {
            // 完全没有额度数据：只保留「查询中」过渡行；否则整条不占位。
            if !quota_state.refreshing.is_empty() {
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
                    .children(quota_rows.into_iter().map(|(provider, text, color, hint)| {
                        // 只对用户手动点击刷新的那家转绿；启动首查和定时刷新保持
                        // 静默，继续显示已有额度自己的风险颜色。
                        // 点击只刷新当前这一家（点谁查谁）。
                        let is_manual_refreshing =
                            quota_state.manual_refreshing.contains(&provider);
                        let tip = if is_manual_refreshing {
                            format!("{hint} 正在刷新…")
                        } else {
                            format!("{hint} 点击刷新。")
                        };
                        let on_click_this = quota_this.clone();
                        div()
                            .id(provider.id())
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
                            .child(acp_provider_icon(provider).size(px(12.)))
                            .child(text)
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                            })
                            .on_click(move |_ev, _window, cx| {
                                on_click_this.update(cx, |workspace, cx| {
                                    workspace.request_provider_quota_refresh(provider);
                                    cx.notify();
                                });
                            })
                    })),
            )
        };

        let footer = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_2()
            .border_t_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .id("sidebar-settings")
                    .relative()
                    .flex_1()
                    .h(px(32.))
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
                            .h(px(32.))
                            .w_full()
                            .px_0()
                            .cursor_pointer()
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
                                    let e_account_settings = e_settings.clone();
                                    let accounts = account_menu_entries.clone();
                                    let multiple_accounts = accounts.len() > 1;
                                    let has_accounts = !accounts.is_empty();
                                    let mut menu = menu
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

                                    for account in accounts {
                                        for action in
                                            account.actions.iter().filter(|action| !action.disabled).cloned()
                                        {
                                            let label = if multiple_accounts {
                                                format!(
                                                    "{}：{}",
                                                    account.settings_title, action.label
                                                )
                                            } else {
                                                action.label.clone()
                                            };
                                            let plugin_id = account.plugin_id.clone();
                                            let section_id = account.settings_section_id.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(label)
                                                    .icon(IconName::User)
                                                    .on_click(move |_ev, _window, cx| {
                                                        crate::plugin_ui::run_settings_action(
                                                            plugin_id.clone(),
                                                            section_id.clone(),
                                                            action.clone(),
                                                            None,
                                                            cx,
                                                        );
                                                    }),
                                            );
                                        }

                                        let title = if account.settings_title.trim().is_empty() {
                                            "插件设置".to_string()
                                        } else {
                                            format!("{} 设置", account.settings_title)
                                        };
                                        let plugin_id = account.plugin_id;
                                        let settings_entity = e_account_settings.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(title)
                                                .icon(IconName::Settings)
                                                .on_click(move |_ev, _window, cx| {
                                                    settings_entity.update(cx, |ws, cx| {
                                                        ws.open_settings_section(
                                                            crate::settings::SettingsSection::Plugin {
                                                                id: plugin_id.clone(),
                                                            },
                                                            cx,
                                                        );
                                                    });
                                                }),
                                        );
                                    }
                                    if has_accounts {
                                        menu = menu.separator();
                                    }

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
            )
            .child(
                div()
                    .id("open-project")
                    .h(px(32.))
                    .flex_1()
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
                        Icon::new(IconName::FolderOpen)
                            .size(px(14.))
                            .text_color(rgb(ui_theme::text_muted())),
                    )
                    .child("打开项目")
                    .on_click(move |_ev, _window, cx| {
                        e_open.update(cx, |ws, cx| ws.open_project(cx));
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
            // 工作台（含智能体对话）、插件、项目是三个并列分组。
            .child(self.render_automation_navigation(
                this.clone(),
                statuses,
                animate_running,
                cx,
            ))
            .children(self.render_workspace_surface_rows(this, cx))
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("项目"),
            )
            .child(header)
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

    fn render_workspace_surface_rows(&self, entity: Entity<Workspace>, _cx: &App) -> Option<Div> {
        let surfaces = crate::plugin_ui::workspace_surfaces();
        if surfaces.is_empty() {
            return None;
        }
        let active = self.plugin_surface_key().map(str::to_string);
        Some(
            div()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .px_2()
                .pt_1()
                .pb_1()
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .font_medium()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("插件"),
                )
                .children(surfaces.into_iter().map(|tab| {
                    let key = tab.key();
                    let selected = active.as_deref() == Some(key.as_str());
                    let title = self.workspace_surface_display_title(&key);
                    let entity_click = entity.clone();
                    let entity_rename = entity.clone();
                    let open_key = key.clone();
                    let rename_key = key.clone();
                    div()
                        .id(SharedString::from(format!("workspace-surface-{key}")))
                        .h(px(32.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(ui_theme::row_radius())
                        .cursor_pointer()
                        .map(|d| {
                            if selected {
                                d.bg(rgb(ui_theme::bg_selected()))
                            } else {
                                d.hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                            }
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(18.))
                                .rounded(px(4.))
                                .bg(rgb(0x167c80))
                                .text_color(gpui::white())
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    title
                                        .chars()
                                        .next()
                                        .unwrap_or('P')
                                        .to_uppercase()
                                        .to_string(),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(rgb(if selected {
                                    ui_theme::text_bright()
                                } else {
                                    ui_theme::text_mid()
                                }))
                                .truncate()
                                .child(title),
                        )
                        .on_click(move |ev, window, cx| {
                            if ev.click_count() >= 2 {
                                entity_rename.update(cx, |ws, cx| {
                                    ws.start_rename(
                                        RenameTarget::WorkspaceSurface(rename_key.clone()),
                                        window,
                                        cx,
                                    );
                                });
                                return;
                            }
                            entity_click.update(cx, |ws, cx| {
                                ws.open_workspace_surface(open_key.clone(), window, cx);
                            });
                        })
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROJECT_ACTION_SLOT_HEIGHT, PROJECT_DISCLOSURE_ICON_SIZE, PROJECT_GUIDE_LEFT,
        PROJECT_HEADER_FONT_SIZE, PROJECT_NEW_BUTTON_SIZE, PROJECT_SESSION_INDENT,
        SESSION_AGENT_ICON_BOX_SIZE, SESSION_AGENT_ICON_SIZE, project_disclosure_icon,
        project_header_is_selected, session_row_is_selected, session_row_status_label,
        shared_project_action_payload, status_text,
    };
    use crate::AgentStatus;
    use gpui_component::IconName;

    const _: () = {
        assert!(PROJECT_NEW_BUTTON_SIZE >= 28.);
        assert!(PROJECT_HEADER_FONT_SIZE == 13.);
        assert!(PROJECT_NEW_BUTTON_SIZE > PROJECT_ACTION_SLOT_HEIGHT);
        assert!(PROJECT_SESSION_INDENT == 22.);
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

    #[test]
    fn project_action_requires_every_session_to_target_the_same_context() {
        let payload = |issue_id: &str| {
            serde_json::from_value(serde_json::json!({
                "agent_session": {
                    "agent": {
                        "plugin_id": "com.example.issues",
                        "contribution_id": "issue-agent"
                    },
                    "controller": {
                        "plugin_id": "com.example.issues",
                        "contribution_id": "issue-session"
                    },
                    "instance": {
                        "plugin_id": "com.example.issues",
                        "resource_type": "issue",
                        "resource_id": "shared-instance"
                    }
                },
                "context": {"issue_id": issue_id}
            }))
            .unwrap()
        };

        assert!(shared_project_action_payload(&[payload("issue-1"), payload("issue-2")]).is_none());
    }
}
