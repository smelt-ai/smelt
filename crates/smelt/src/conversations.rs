//! 侧栏「对话」：智能体对话怎么列在侧栏、怎么画在舞台上，以及侧栏顶上的入口。
//!
//! 对话本身是宿主的会话（[`crate::SessionKind::Conversation`]），列在哪、看哪一段由宿主
//! 管。智能体、自动化是插件视图，宿主只按视图 id 和命令 id 找它们，没装就没有那个入口：
//! 开对话走命令 [`NEW_CONVERSATION`]，对话里「管理能力」「看记忆」按视图 id 打开。
//!
//! `impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{Icon, IconName, StyledExt as _};
use smelt_ui::truncated_line::truncated_line;
use smelt_ui::{Button, IconAction as _, ListRow};

use crate::{AgentStatus, Workspace, WorkspaceRoute, acp_view, settings};

/// 智能体插件登记的「新对话」命令。侧栏顶上的按钮和 ⌘N 都是它。
const NEW_CONVERSATION: &str = "smelt.agents/new-conversation";
/// 智能体插件的视图 id：对话里「管理能力」（按智能体）、「看记忆」去那里。
const AGENTS_VIEW: &str = "agents";
/// 能力库插件的视图 id：对话里「管理能力」（不按智能体）去那里。
const CAPABILITIES_VIEW: &str = "capabilities";

/// 「对话」分组行、对话行的 hover group 名。
const CONVERSATION_GROUP_HOVER: &str = "conversation-group-hover";
const CONVERSATION_ROW_HOVER: &str = "product-conv-row";

impl Workspace {
    /// 侧栏顶上固定的入口：「新对话」，和没摆进布局的插件视图（当舞台上的一页打开）。
    pub(crate) fn render_sidebar_navigation(&self, entity: Entity<Workspace>, cx: &App) -> Div {
        div()
            .flex_shrink_0()
            .px_2()
            .pt_2()
            .pb_1()
            .flex()
            .flex_col()
            .gap_1()
            .children(new_conversation_button(cx))
            // 摆进布局的视图就在布局里看，不再给入口。舞台自己当不了舞台上的页。
            .children(
                smelt_plugin_api::views(cx)
                    .into_iter()
                    .filter(|view| {
                        !self.shell_view.laid_out.contains(view.id.as_ref())
                            && view.id.as_ref() != crate::builtin_views::Builtin::Stage.id()
                    })
                    .map(|view| {
                        let attention = smelt_plugin_api::view_badge(&view.id, cx).is_some();
                        self.render_view_navigation_row(
                            view.id.to_string(),
                            view.title,
                            Icon::empty().path(view.icon),
                            attention,
                            entity.clone(),
                        )
                    }),
            )
    }

    /// 侧栏顶上一个插件视图的入口：点了总是进那一页。
    fn render_view_navigation_row(
        &self,
        view_id: String,
        label: impl Into<SharedString>,
        icon: impl Into<Icon>,
        attention: bool,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let selected = matches!(self.active_tab(), WorkspaceRoute::Page(id) if *id == view_id);
        ListRow::new(SharedString::from(format!("{view_id}-route")))
            .selected(selected)
            .h(crate::ui_theme::height::md())
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .child(Icon::new(icon).size(px(16.)).text_color(rgb(if selected {
                crate::ui_theme::text_mid()
            } else {
                crate::ui_theme::text_muted()
            })))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(if selected {
                        crate::ui_theme::text_selected()
                    } else {
                        crate::ui_theme::text_mid()
                    }))
                    .child(label.into()),
            )
            .when(attention, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(crate::ui_theme::red())),
                )
            })
            .on_click(move |_, window, cx| {
                entity.update(cx, |workspace, cx| {
                    workspace.open_page(&view_id, serde_json::Value::Null, window, cx)
                });
            })
            .into_any_element()
    }

    /// 侧栏「对话」分组。跟项目分组同一套：分组行点一下收起或展开，展开列出全部对话，
    /// 按最近活动排；收起时分组行写数量和最要紧的状态。一个对话都没有就整组不出现。
    /// 跟项目在同一个滚动区里，对话多了只是把项目往下推。
    pub(crate) fn render_product_conversation_parts(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        // 键盘焦点在侧栏里时，焦点环落在哪一行（`SidebarItem::key`）。
        focused_key: Option<u64>,
        chevron_degrees: Option<f32>,
        cx: &App,
    ) -> Option<(AnyElement, Option<AnyElement>)> {
        // 跟键盘导航同一份顺序。
        let product = self.sidebar_conversation_order(cx);
        if product.is_empty() {
            return None;
        }
        let ordered: Vec<(usize, u64)> = product
            .iter()
            .map(|&ix| (ix, self.sessions[ix].effective_updated_at(cx)))
            .collect();
        let selected_sid = self.nav.conversation_sid();
        let collapsed = self.sidebar_conversations_collapsed;
        let agg = product
            .iter()
            .filter_map(|&ix| statuses.get(ix).copied())
            .min_by_key(|status| status.rank())
            .unwrap_or(AgentStatus::Idle);
        let selected_inside = selected_sid.is_some_and(|sid| {
            product.iter().any(|&ix| {
                self.sessions[ix]
                    .active_acp()
                    .is_some_and(|view| view.read(cx).session_id() == sid)
            })
        });
        let toggle_entity = entity.clone();
        let group_key = crate::session_list::SidebarItem::ConversationGroup.key();
        let reveal_target = self.sidebar_reveal.target();
        let conv_scroll = self.conversation_scroll();
        // 分组行跟项目行同一个样子：图标、名字，收起时右边写数量和状态点。选中的对话被
        // 收在里面时，分组行代它亮起来。
        let header = ListRow::new("conversation-group")
            .group(CONVERSATION_GROUP_HOVER)
            .relative()
            .when(focused_key == Some(group_key), |row| {
                row.child(crate::session_list::row::focus_ring())
            })
            .when(reveal_target == Some(group_key), |row| {
                row.child(crate::session_list::reveal_anchor(
                    self.sidebar_reveal.clone(),
                    group_key,
                    conv_scroll.clone(),
                    entity.clone(),
                ))
            })
            .selected(collapsed && selected_inside)
            .h(crate::ui_theme::height::md())
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(crate::ui_theme::row_radius())
            .child({
                if let Some(deg) = chevron_degrees {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .w(px(crate::session_list::row::GROUP_ICON_BOX))
                                .h(px(16.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(px(12.))
                                        .text_color(rgb(crate::ui_theme::text_muted()))
                                        .transform(Transformation::rotate(radians(
                                            deg.to_radians(),
                                        ))),
                                ),
                        )
                        .child(
                            Icon::new(gpui_kit_assets::IconName::MessageSquare)
                                .size(px(15.))
                                .text_color(rgb(crate::ui_theme::text_muted())),
                        )
                        .into_any_element()
                } else {
                    crate::session_list::row::group_disclosure(
                        "conversation-group-disclosure",
                        CONVERSATION_GROUP_HOVER,
                        Icon::new(gpui_kit_assets::IconName::MessageSquare).size(px(15.)),
                        collapsed,
                        true,
                        None,
                    )
                    .into_any_element()
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(crate::ui_theme::text_mid()))
                    .child("对话"),
            )
            .when(collapsed && agg != AgentStatus::Idle, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .size(px(6.))
                        .rounded_full()
                        .bg(crate::ui_theme::session_dot_color(agg)),
                )
            })
            .when(collapsed, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_muted()))
                        .child(product.len().to_string()),
                )
            })
            .on_click(move |_, _, cx| {
                toggle_entity.update(cx, |workspace, cx| {
                    workspace.sidebar_conversations_collapsed =
                        !workspace.sidebar_conversations_collapsed;
                    cx.notify();
                });
            });
        let header_elem = div().mx_2().mt_3().child(header).into_any_element();

        let now = chrono::Local::now();
        // 正在就地改名的那一行：标题那一格换成输入框。
        let renaming = match (&self.rename_target, &self.rename_input) {
            (Some(crate::RenameTarget::Session(ix)), Some(input)) => Some((*ix, input.clone())),
            _ => None,
        };
        let mut rows = Vec::new();
        for (ix, updated_at) in ordered {
            let Some(session) = self.sessions.get(ix) else {
                continue;
            };
            let Some(view) = session.active_acp() else {
                continue;
            };
            let sid = view.read(cx).session_id().to_string();
            let selected = selected_sid == Some(sid.as_str());
            let runtime = view.read(cx).runtime().key().clone();
            let title = conversation_row_title(
                session.custom_title.as_deref(),
                view.read(cx).auto_title().as_deref(),
            );
            let status = statuses.get(ix).copied().unwrap_or(AgentStatus::Idle);
            // 行尾一格：要人管时写状态；对话所在的机器用不了时写它怎么了；否则写时间。
            let meta: (SharedString, Hsla) =
                match crate::session_list::row::session_row_status_label(status) {
                    Some(label) => (
                        label.into(),
                        crate::ui_theme::session_dot_color(status).into(),
                    ),
                    None => (
                        conversation_row_meta(
                            &runtime,
                            crate::session_list::row::session_updated_at_text(updated_at, &now),
                            cx,
                        )
                        .into(),
                        rgb(crate::ui_theme::text_muted()).into(),
                    ),
                };
            let tooltip = conversation_row_tooltip(&title, &runtime, cx);
            let bound = bound_agent_definition_id(
                session.agent_definition_id.as_deref(),
                session.automation_id.as_deref(),
            );
            // 对话绑定的智能体在对话所在的那台机器上。
            let agent_name = bound.and_then(|id| {
                settings::agent_hosts(cx)?
                    .agent(&runtime, id)
                    .map(|agent| agent_display_name(&agent.name))
            });
            let mark = conversation_agent_mark(agent_name.as_deref());
            let open_ix = ix;
            let ui_id = session.ui_id;
            let open_entity = entity.clone();
            let close_entity = entity.clone();
            let rename_entity = entity.clone();
            rows.push(
                ListRow::new(format!("product-conversation-{sid}"))
                    .selected(selected)
                    .group(CONVERSATION_ROW_HOVER)
                    .relative()
                    .h(crate::ui_theme::height::md())
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(crate::ui_theme::row_radius())
                    .when(self.sidebar_flash == Some(ui_id), |row| {
                        row.child(crate::session_list::row::new_row_flash((
                            "sidebar-new-row",
                            ui_id,
                        )))
                    })
                    .when(self.sidebar_reveal.target() == Some(ui_id), |row| {
                        row.child(crate::session_list::reveal_anchor(
                            self.sidebar_reveal.clone(),
                            ui_id,
                            conv_scroll.clone(),
                            entity.clone(),
                        ))
                    })
                    .when(status == AgentStatus::NeedsYou, |row| {
                        row.child(crate::session_list::attention_anchor(
                            self.sidebar_reveal.clone(),
                            ui_id,
                        ))
                    })
                    .when(focused_key == Some(ui_id), |row| {
                        row.child(crate::session_list::row::focus_ring())
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(18.))
                            .rounded_full()
                            .bg(rgb(crate::ui_theme::bg_selected()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_medium()
                            .text_color(Workspace::session_icon_color(status, None))
                            .child(mark),
                    )
                    // 跟项目下的会话行一样一行：标题占满，行尾一格写时间或状态。
                    .child(match renaming.as_ref().filter(|(renaming, _)| *renaming == ix) {
                        Some((_, input)) => {
                            crate::session_list::row::inline_rename_input(input, entity.clone())
                                .into_any_element()
                        }
                        // 选中跟项目下的会话行同口径：选中字色加中等字重。
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .when(selected, |title| title.font_medium())
                            .text_color(rgb(if selected {
                                crate::ui_theme::text_selected()
                            } else {
                                crate::ui_theme::text()
                            }))
                            .overflow_hidden()
                            .child(truncated_line(title))
                            .into_any_element(),
                    })
                    .child(crate::session_list::row::row_trailing_slot(
                        CONVERSATION_ROW_HOVER,
                        Some(meta),
                        Button::new(format!("close-product-conversation-{sid}"))
                            .dismiss()
                            .icon(IconName::Close)
                            .icon_action("关闭")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                close_entity.update(cx, |workspace, cx| {
                                    workspace.request_close_session(open_ix, window, cx)
                                });
                            }),
                    ))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    // 双击改名：第一下已经打开了，第二下就地改名。
                    .on_click(move |ev, window, cx| {
                        open_entity.update(cx, |workspace, cx| {
                            if ev.click_count() >= 2 {
                                workspace.start_rename(
                                    crate::RenameTarget::Session(open_ix),
                                    window,
                                    cx,
                                );
                            } else {
                                workspace.open_agent_conversation(open_ix, window, cx);
                            }
                        });
                    })
                    .context_menu(move |menu, _window, _cx| {
                        let rename_entity = rename_entity.clone();
                        menu.item(
                            PopupMenuItem::new("重命名")
                                .icon(Icon::empty().path("smelt-icons/square-pen.svg"))
                                .on_click(move |_ev, window, cx| {
                                    rename_entity.update(cx, |workspace, cx| {
                                        workspace.start_rename(
                                            crate::RenameTarget::Session(open_ix),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                    })
                    .into_any_element(),
            );
        }
        // 对话缩进一档挂在分组下面，左边一根细线，跟项目下的会话同一套尺寸。
        let body = div()
            .mx_2()
            .relative()
            .flex()
            .flex_col()
            .gap(px(1.))
            .pb_1()
            .pl(px(crate::session_list::PROJECT_SESSION_INDENT))
            .child(
                div()
                    .absolute()
                    .left(px(crate::session_list::PROJECT_GUIDE_LEFT))
                    .top_0()
                    .bottom(px(4.))
                    .w(px(1.))
                    .bg(crate::ui_theme::hairline()),
            )
            .children(rows)
            .into_any_element();

        Some((header_elem, Some(body)))
    }

    pub(crate) fn render_product_conversation_nav(
        &self,
        entity: Entity<Workspace>,
        statuses: &[AgentStatus],
        focused_key: Option<u64>,
        cx: &App,
    ) -> AnyElement {
        let Some((header, body)) =
            self.render_product_conversation_parts(entity, statuses, focused_key, None, cx)
        else {
            return div().into_any_element();
        };
        if let Some(body) = body {
            if !self.sidebar_conversations_collapsed {
                div()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(body)
                    .into_any_element()
            } else {
                header
            }
        } else {
            header
        }
    }

    /// 舞台上的对话：「对话」路由对上的那一段。对不上（冷启动还没补位、刚被移走）时
    /// 先空着，`align_visible_route` 会纠正。
    pub(crate) fn render_conversation_stage(&self, cx: &App) -> AnyElement {
        let Some((_, view)) = self.selected_conversation_view(cx) else {
            return div()
                .flex_1()
                .bg(rgb(crate::ui_theme::bg_stage()))
                .into_any_element();
        };
        let appearance = cx.try_global::<settings::Appearance>();
        let background_image = appearance.and_then(|appearance| appearance.bg_image.clone());
        let bg_image_opacity = appearance
            .map(|appearance| appearance.bg_image_opacity)
            .unwrap_or(0.25);
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .bg(rgb(crate::ui_theme::bg_stage()))
            .children(background_image.as_deref().map(|path| {
                crate::workspace_frame::background_image_layer(path, bg_image_opacity, cx)
            }))
            .child(view.cached(gpui::StyleRefinement::default().size_full()))
            .into_any_element()
    }

    /// 换到一个宿主入口（远程桌面、项目会话）。这些页都画在舞台上：舞台得露出来。
    pub(crate) fn activate_workspace_tab(
        &mut self,
        tab: WorkspaceRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab != WorkspaceRoute::Desktop {
            self.desktop = None;
        }
        self.nav.set_active(tab);
        self.show_view(crate::builtin_views::Builtin::Stage.id(), window, cx);
        window.focus(&self.focus_handle, cx);
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    pub(crate) fn open_agent_conversation(
        &mut self,
        session_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get(session_ix) else {
            return;
        };
        if !session.is_agent_conversation(cx) {
            return;
        }
        let view = session.active_acp().cloned();
        let Some(sid) = view
            .as_ref()
            .map(|view| view.read(cx).session_id().to_string())
        else {
            return;
        };
        if self.active_session != session_ix {
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
            self.active_session = session_ix;
            // 换到的对话在侧栏「对话」组里得看得见：组折着就展开，再滚到它。
            self.sidebar_conversations_collapsed = false;
            self.sidebar_reveal.request(self.sessions[session_ix].ui_id);
        }
        self.note_session_viewed(session_ix);
        // 右侧文件树/变更/历史/技能都读 active_project_ref；对话的托管工作目录
        // 就是它这一刻的上下文，不写进去工具面板会停在上一个项目上。
        if let Some(cwd) = self
            .sessions
            .get(session_ix)
            .and_then(|session| session.cwd(cx))
            .filter(|cwd| !cwd.is_empty())
        {
            self.active_project = Some(crate::ProjectRef {
                runtime: self.sessions[session_ix].runtime(cx).key().clone(),
                root: cwd.trim_end_matches('/').to_string(),
            });
        }
        self.desktop = None;
        self.nav.open_conversation(sid);
        self.schedule_visible_refresh(cx);
        self.sync_session_ui(cx);
        if let Some(view) = view {
            view.update(cx, |view, cx| {
                view.maybe_auto_resume(window, cx);
                view.focus_input(window, cx);
            });
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 「对话」路由正在看的那段对话。
    pub(crate) fn selected_conversation_view(
        &self,
        cx: &App,
    ) -> Option<(usize, Entity<acp_view::AcpView>)> {
        let sid = self.nav.conversation_sid()?;
        self.sessions.iter().enumerate().find_map(|(ix, session)| {
            if !session.is_agent_conversation(cx) {
                return None;
            }
            let view = session.active_acp()?;
            (view.read(cx).session_id() == sid).then(|| (ix, view.clone()))
        })
    }

    /// 对话里「管理能力」（不按智能体管）：去能力库的插件页签。能力库插件没装就不动。
    pub(crate) fn open_conversation_capabilities(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page(
            CAPABILITIES_VIEW,
            serde_json::json!({ "tab": "plugins" }),
            window,
            cx,
        );
    }

    /// 对话里按智能体管能力、点了记忆：去智能体插件，有绑定的智能体就打开它（`tab` 停在
    /// 哪个页签）。智能体已经删了、插件没装，由插件停在列表或宿主什么都不做。
    pub(crate) fn open_conversation_agent(
        &mut self,
        machine: &crate::RuntimeKey,
        agent: Option<&str>,
        tab: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut args = serde_json::json!({
            "machine": smelt_plugin_api::machine_to_json(machine),
        });
        if let Some(agent) = agent {
            args["agent"] = agent.into();
        }
        if let Some(tab) = tab {
            args["tab"] = tab.into();
        }
        self.open_page(AGENTS_VIEW, args, window, cx);
    }

    /// 对话里记忆卡片上的采纳、删除。成功后卡片随 smeltd 推来的快照更新；开着的
    /// 记忆页由智能体插件按 `agent_memory.changed` 刷新。
    pub(crate) fn run_conversation_memory_action(
        &mut self,
        view: Entity<crate::acp_view::AcpView>,
        runtime: crate::RuntimeKey,
        action: crate::acp_view::AcpMemoryAction,
        cx: &mut Context<Self>,
    ) {
        let client = crate::runtime_call::target(&runtime, cx);
        let params = smelt_model::control_api::AgentMemoryFileParams {
            agent_id: action.agent_id,
            layer: action.layer,
            file_name: action.file_name,
        };
        let kind = action.kind;
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let client = client?;
                    let timeout = crate::runtime_call::RUNTIME_CALL_TIMEOUT;
                    match kind {
                        crate::acp_view::AcpMemoryActionKind::Approve => {
                            crate::runtime_call::call_on::<
                                smelt_model::control_api::AgentMemoryApprove,
                            >(&client, params, timeout)
                            .map(|_| ())
                        }
                        crate::acp_view::AcpMemoryActionKind::Delete => {
                            crate::runtime_call::call_on::<
                                smelt_model::control_api::AgentMemoryDelete,
                            >(&client, params, timeout)
                            .map(|_| ())
                        }
                    }
                    .map_err(crate::runtime_call::error_text)
                })
                .await;
            let failure = match kind {
                crate::acp_view::AcpMemoryActionKind::Approve => "采纳失败",
                crate::acp_view::AcpMemoryActionKind::Delete => "删除失败",
            };
            let error = result.err().map(|error| format!("{failure}：{error}"));
            view.update(cx, |view, cx| view.finish_memory_action(error, cx));
        })
        .detach();
    }
}

/// 侧栏最顶的「新对话」。开哪个智能体、没有时去哪选由智能体插件定；插件没装就没有这个
/// 按钮。悬停提示带上命令现在的快捷键。
fn new_conversation_button(cx: &App) -> Option<AnyElement> {
    smelt_plugin_api::command(NEW_CONVERSATION, cx)?;
    Some(
        Button::new("workbench-chat")
            .normal()
            .w_full()
            .icon(IconName::Plus)
            .label("新对话")
            .tooltip_with_action(
                "新对话",
                &smelt_plugin_api::RunCommand::new(NEW_CONVERSATION),
                None,
            )
            .on_click(|_, _, cx| {
                smelt_plugin_api::run(NEW_CONVERSATION, serde_json::Value::Null, cx).detach();
            })
            .into_any_element(),
    )
}

/// 对话行行尾：机器用不了时写它怎么了，否则写时间。
fn conversation_row_meta(runtime: &crate::RuntimeKey, updated: String, cx: &App) -> String {
    crate::runtimes::runtime_unavailable_label(runtime, cx)
        .map(str::to_string)
        .unwrap_or(updated)
}

/// 对话行的悬停提示：完整标题，在远程机器上的再带上机器名。
fn conversation_row_tooltip(title: &str, runtime: &crate::RuntimeKey, cx: &App) -> SharedString {
    match runtime {
        crate::RuntimeKey::Local => title.to_string().into(),
        crate::RuntimeKey::Remote(_) => {
            format!("{title} · {}", crate::runtimes::runtime_name(runtime, cx)).into()
        }
    }
}

/// 这段对话属于哪个智能体：只看创建时写下的绑定。
///
/// 项目里选智能体开的也算。旧的工作台裸对话没有绑定，不算任何智能体。
/// 自动化 Run 虽然可能带着同一个 id，但那不是对话。不从 cwd 反推——项目
/// 路径说明不了是谁，space 子目录也不是另一个智能体。
fn bound_agent_definition_id<'a>(
    definition_id: Option<&'a str>,
    automation_id: Option<&str>,
) -> Option<&'a str> {
    if automation_id.is_some() {
        return None;
    }
    definition_id.map(str::trim).filter(|id| !id.is_empty())
}

/// 侧栏「对话」分组：按最近活动排，新的在前。
pub(crate) fn recent_conversation_order(updated_at: &[u64]) -> Vec<usize> {
    let mut ixes: Vec<usize> = (0..updated_at.len()).collect();
    ixes.sort_by(|left, right| {
        updated_at[*right]
            .cmp(&updated_at[*left])
            .then(left.cmp(right))
    });
    ixes
}

/// 绑了智能体的会话在侧栏上的身份：行首头像（显示名第一个字）和悬停时的名字。
/// 没绑（裸引擎）返回 `None`。定义找不到时退回 id。
pub(crate) fn bound_agent_identity(
    definition_id: Option<&str>,
    automation_id: Option<&str>,
    agents: &[settings::AgentDefinition],
) -> Option<(String, String)> {
    let id = bound_agent_definition_id(definition_id, automation_id)?;
    let name = agents
        .iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent_display_name(&agent.name))
        .unwrap_or_else(|| id.to_string());
    Some((conversation_agent_mark(Some(&name)), name))
}

/// 行首头像：取名字第一个字；没有名字的对话标 Pi。
fn conversation_agent_mark(agent_name: Option<&str>) -> String {
    match agent_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => name.chars().next().unwrap_or('智').to_string(),
        None => "Pi".to_string(),
    }
}

/// 对话行只展示用户命名或对话自己的自动标题。
fn conversation_row_title(custom_title: Option<&str>, auto_title: Option<&str>) -> String {
    custom_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| auto_title.map(str::trim).filter(|title| !title.is_empty()))
        .unwrap_or("新对话")
        .to_string()
}

fn agent_display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "未命名".to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bound_agent_definition_id, bound_agent_identity, conversation_agent_mark,
        conversation_row_title, recent_conversation_order,
    };
    use crate::{AgentStatus, Workspace};
    use gpui::rgb;

    #[test]
    fn conversation_belongs_to_the_bound_agent_not_the_cwd() {
        assert_eq!(bound_agent_definition_id(Some("a"), None), Some("a"));
        assert_eq!(bound_agent_definition_id(Some("  a  "), None), Some("a"));
        assert_eq!(bound_agent_definition_id(Some(""), None), None);
        assert_eq!(bound_agent_definition_id(None, None), None);
        assert_eq!(
            bound_agent_definition_id(Some("a"), Some("auto-1")),
            None,
            "自动化 Run 不是对话"
        );
    }

    #[test]
    fn recent_conversations_put_the_newest_first() {
        assert_eq!(recent_conversation_order(&[10, 30, 20]), vec![1, 2, 0]);
        assert_eq!(
            recent_conversation_order(&[5, 5]),
            vec![0, 1],
            "同一秒保持原来的顺序"
        );
    }

    #[test]
    fn conversation_mark_uses_the_first_character_or_pi() {
        assert_eq!(conversation_agent_mark(Some("写作助手")), "写");
        assert_eq!(conversation_agent_mark(Some("  研究助理")), "研");
        assert_eq!(conversation_agent_mark(None), "Pi");
        assert_eq!(conversation_agent_mark(Some("  ")), "Pi");
    }

    /// 侧栏悬停显示智能体的名字，不是底层引擎名；定义已删时退回 id。
    #[test]
    fn bound_agent_identity_pairs_the_mark_with_the_agent_name() {
        let agents = [crate::settings::AgentDefinition {
            id: "writer".into(),
            name: "写作助手".into(),
            ..Default::default()
        }];
        assert_eq!(
            bound_agent_identity(Some("writer"), None, &agents),
            Some(("写".to_string(), "写作助手".to_string()))
        );
        assert_eq!(
            bound_agent_identity(Some("gone"), None, &agents),
            Some(("g".to_string(), "gone".to_string()))
        );
        assert_eq!(bound_agent_identity(None, None, &agents), None);
        assert_eq!(
            bound_agent_identity(Some("writer"), Some("auto-1"), &agents),
            None,
            "自动化 Run 不是对话"
        );
    }

    #[test]
    fn conversation_row_title_uses_title_source_not_agent_name_equality() {
        assert_eq!(
            conversation_row_title(Some("量化交易助手"), Some("自动标题")),
            "量化交易助手",
            "用户把会话命名成智能体名时不能被自动标题覆盖"
        );
        assert_eq!(
            conversation_row_title(None, Some("季度复盘")),
            "季度复盘",
            "智能体改名不能影响已经生成的对话标题"
        );
        assert_eq!(conversation_row_title(None, None), "新对话");
    }

    #[test]
    fn selected_idle_conversation_icon_does_not_look_like_running() {
        // 选中由行底色表达。图标如果因为选中而走强调色，就会和运行中撞色。
        let idle = Workspace::session_icon_color(AgentStatus::Idle, None);
        let running = Workspace::session_icon_color(AgentStatus::Running, None);
        assert_ne!(idle, running, "空闲不能和运行中撞色");
        assert_ne!(
            idle,
            rgb(crate::ui_theme::accent()),
            "空闲不能走强调色，否则选中行看起来像带了状态色"
        );
    }
}
