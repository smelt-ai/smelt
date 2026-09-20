//! 舞台和右栏共用的 34px 顶栏：窗口开关 + 可见舞台身份。
//! 用其他应用打开在项目右键；文件/变更/技能/历史在右抽屉里。
//!
//! 标题槽只填当前舞台没有自己页头的那一面（项目会话、智能体对话、插件面）。
//! 智能体/自动化目录和钻取已有页头，铬不重复写，也不回落到上一会话。
//!
//! 跟 file_tree 模块（`crates/smelt/src/file_tree/`）同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::*;

use crate::{StageCover, Workspace, WorkspaceRoute, ui_theme, workspace_frame};

/// 共用顶栏标题槽。目录/编辑器/自动化钻取都有页头，那些面必须是 `Hidden`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChromeTitle {
    Hidden,
    Visible {
        title: String,
        model: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChromeSessionTitle {
    pub title: String,
    pub model: Option<String>,
}

impl ChromeTitle {
    fn from_session(session: Option<ChromeSessionTitle>) -> Self {
        match session {
            Some(session) if !session.title.trim().is_empty() => Self::Visible {
                title: session.title,
                model: session.model,
            },
            _ => Self::Hidden,
        }
    }
}

/// 铬标题跟可见面走，不读裸的 `active_session`。
///
/// `project_session` 是项目舞台上那条会话；切到智能体目录后它往往还在，
/// 但不能再显示——那就是「顶栏还停在上一场 Grok」这条 bug。
pub(crate) fn chrome_title_for_stage(
    route: &WorkspaceRoute,
    agent_conversation: Option<ChromeSessionTitle>,
    project_session: Option<ChromeSessionTitle>,
    plugin_title: Option<String>,
) -> ChromeTitle {
    match route {
        WorkspaceRoute::Session => ChromeTitle::from_session(project_session),
        WorkspaceRoute::Agents => ChromeTitle::from_session(agent_conversation),
        WorkspaceRoute::Automations => ChromeTitle::Hidden,
        WorkspaceRoute::Plugin { .. } => match plugin_title {
            Some(title) if !title.trim().is_empty() => ChromeTitle::Visible { title, model: None },
            _ => ChromeTitle::Hidden,
        },
    }
}

/// 首次 IDE 扫描完成后需要更新的菜单。菜单可能已经被用户关闭，因此只持有弱引用。
pub(crate) struct IdePopupWaiter {
    popup: WeakEntity<PopupMenu>,
    root: String,
}

impl Workspace {
    /// 构建「使用其他应用打开」菜单。Finder 始终立即可用；IDE 首次发现留在后台，
    /// `PopupMenu::rebuild` 会在结果回来后把其余应用原地补进菜单。
    pub(crate) fn build_ide_popup_menu(
        &mut self,
        menu: PopupMenu,
        root: String,
        popup: Entity<PopupMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let snapshot = self.ide_catalog.snapshot();
        if snapshot.installed.is_none() {
            self.ide_popup_waiters.push(IdePopupWaiter {
                popup: popup.downgrade(),
                root: root.clone(),
            });
            if !snapshot.scanning {
                self.refresh_ide_catalog(window, cx);
            }
        }
        Self::ide_popup_items(menu, root, snapshot, cx.entity())
    }

    /// 后台刷新本机 IDE 目录。首次扫描结束后会原地补齐所有仍打开的应用菜单。
    fn refresh_ide_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ide_catalog.begin_refresh() {
            return;
        }
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let installed = cx
                .background_executor()
                .spawn(async move { crate::ide::detect_installed() })
                .await;

            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.ide_catalog.finish_refresh(installed);
                let snapshot = workspace.ide_catalog.snapshot();
                let waiters = std::mem::take(&mut workspace.ide_popup_waiters);
                let workspace_entity = cx.entity();

                for waiter in waiters {
                    let snapshot = snapshot.clone();
                    let workspace_entity = workspace_entity.clone();
                    let _ = waiter.popup.update(cx, |popup_menu, popup_cx| {
                        popup_menu.rebuild(window, popup_cx, |menu, _, _| {
                            Self::ide_popup_items(menu, waiter.root, snapshot, workspace_entity)
                        });
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Finder 是确定存在的系统应用，单独在后台预取它的图标。这个请求不会枚举 IDE，
    /// 也不在 UI 线程调用 AppKit，因此左侧主操作会尽早显示 Finder 的真实图标。
    pub(crate) fn preload_file_manager_icon(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ide_catalog.begin_file_manager_icon_load() {
            return;
        }

        cx.spawn_in(window, async move |this, cx| {
            let icon = cx
                .background_executor()
                .spawn(async move { crate::ide::detect_file_manager_icon() })
                .await;

            let _ = this.update_in(cx, |workspace, _window, cx| {
                workspace.ide_catalog.finish_file_manager_icon_load(icon);
                cx.notify();
            });
        })
        .detach();
    }

    /// 等首屏稳定后再预热一次应用目录。macOS 查询的是 Launch Services 已维护的注册
    /// 表，仍放在后台；用户若先点开菜单，`begin_refresh` 的去重闸门会立刻接管。
    pub(crate) fn prewarm_ide_catalog_after_idle(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ide_catalog.schedule_idle_prewarm() {
            return;
        }

        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(750))
                .await;
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.refresh_ide_catalog(window, cx);
            });
        })
        .detach();
    }

    /// 把某个缓存快照转成菜单项。这个函数只操作已在内存里的数据，因此既可由首次
    /// 点击构建，也可由异步结果通过 `PopupMenu::rebuild` 复用。
    fn ide_popup_items(
        mut menu: PopupMenu,
        root: String,
        snapshot: crate::ide::IdeCatalogSnapshot,
        workspace: Entity<Self>,
    ) -> PopupMenu {
        let crate::ide::IdeCatalogSnapshot {
            installed,
            file_manager_icon,
            ..
        } = snapshot;

        let Some(installed) = installed else {
            return menu.item(Self::file_manager_menu_item(
                root,
                file_manager_icon,
                workspace,
            ));
        };

        if installed.is_empty() {
            menu = menu.item(Self::file_manager_menu_item(
                root,
                file_manager_icon,
                workspace,
            ));
        } else {
            for installed_ide in installed {
                let workspace = workspace.clone();
                let root = root.clone();
                let icon = installed_ide.icon.clone();
                let label = installed_ide.label.to_string();
                menu = menu.item(
                    // ElementItem 不经过 PopupMenu 的 Icon/SVG 通道，直接把
                    // NSWorkspace 返回的彩色 Image 作为菜单行的一部分。
                    PopupMenuItem::element(move |_window, _cx| {
                        let mut row = h_flex().flex_1().items_center().gap_x_2();
                        row = if let Some(icon) = icon.clone() {
                            row.child(img(icon).size(px(17.)).object_fit(ObjectFit::Contain))
                        } else {
                            row.child(Icon::new(IconName::SquareTerminal).size(px(16.)))
                        };
                        row.child(label.clone())
                    })
                    .on_click(move |_ev, _window, cx| {
                        let result =
                            crate::ide::open_in(&installed_ide, std::path::Path::new(&root));
                        workspace.update(cx, |ws, workspace_cx| {
                            if let Err(error) = result {
                                ws.background_error = Some(error);
                                workspace_cx.notify();
                            }
                        });
                    }),
                );
            }
            menu = menu.item(Self::file_manager_menu_item(
                root,
                file_manager_icon,
                workspace,
            ));
        }
        menu
    }

    /// 文件管理器是稳定的系统能力，跟按需发现的 IDE 共用同一菜单，但不依赖扫描
    /// 结果；图标尚未从后台缓存回来时，使用线性文件夹图标作无阻塞回退。
    fn file_manager_menu_item(
        root: String,
        icon: Option<std::sync::Arc<gpui::Image>>,
        workspace: Entity<Self>,
    ) -> PopupMenuItem {
        let label = crate::ide::file_manager_label();
        PopupMenuItem::element(move |_window, _cx| {
            let mut row = h_flex().flex_1().items_center().gap_x_2();
            row = if let Some(icon) = icon.clone() {
                row.child(img(icon).size(px(17.)).object_fit(ObjectFit::Contain))
            } else {
                row.child(Icon::new(IconName::FolderOpen).size(px(16.)))
            };
            row.child(label)
        })
        .on_click(move |_ev, _window, cx| {
            workspace.update(cx, |ws, workspace_cx| {
                ws.open_project_in_file_manager(&root, workspace_cx);
            });
        })
    }

    pub(crate) fn open_project_in_file_manager(&mut self, root: &str, cx: &mut Context<Self>) {
        if let Err(error) = crate::ide::open_in_file_manager(std::path::Path::new(root)) {
            self.background_error = Some(error);
            cx.notify();
        }
    }

    fn chrome_title(&self, cx: &App) -> ChromeTitle {
        let agent_conversation =
            self.selected_agent_conversation_view(cx)
                .map(|(ix, view)| ChromeSessionTitle {
                    title: self.sessions[ix].title(cx),
                    model: view.read(cx).model_name(),
                });
        let project_session = self.sessions.get(self.active_session).and_then(|session| {
            if session.is_product_conversation(cx) {
                return None;
            }
            Some(ChromeSessionTitle {
                title: session.title(cx),
                model: session
                    .active_acp()
                    .and_then(|view| view.read(cx).model_name()),
            })
        });
        let plugin_title = match self.active_tab() {
            WorkspaceRoute::Plugin { key } => Some(self.workspace_surface_display_title(key)),
            _ => None,
        };
        chrome_title_for_stage(
            self.active_tab(),
            agent_conversation,
            project_session,
            plugin_title,
        )
    }

    /// 舞台和右栏共用的 34px 顶栏：可见舞台身份 + 窗口开关。
    /// 用其他应用打开在项目右键；文件/变更/技能/历史在抽屉里。
    pub(crate) fn render_shared_right_chrome(
        &mut self,
        left_guard: Pixels,
        cx: &mut Context<Self>,
    ) -> Div {
        let (title, model) = match self.chrome_title(cx) {
            ChromeTitle::Visible { title, model } => (Some(title), model),
            ChromeTitle::Hidden => (None, None),
        };
        workspace_frame::with_window_drag(
            workspace_frame::top_bar()
                .w_full()
                .h(workspace_frame::TOP_BAR_HEIGHT)
                .flex_none()
                .flex()
                .items_center()
                .gap_2p5()
                .bg(rgb(ui_theme::bg_stage()))
                .border_b_1()
                .border_color(ui_theme::hairline())
                .when(left_guard > px(0.), |d| d.pl(left_guard))
                .when(left_guard == px(0.), |d| d.pl_4())
                .pr(px(18.))
                .children(title.map(|title| {
                    let tip = title.clone();
                    div()
                        .id("stage-title")
                        .min_w(px(56.))
                        .flex_shrink(1.)
                        .overflow_hidden()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(ui_theme::text_bright()))
                        .truncate()
                        .child(title)
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                }))
                .children(model.map(|m| {
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_mid()))
                        .child(m)
                }))
                .child(div().flex_1().min_w_0())
                .child(self.render_window_trailing_toggles(cx)),
        )
    }

    /// 窗口级开关：全屏 / 右侧抽屉。挂在共用顶栏右侧，不进浮层。
    pub(crate) fn render_window_trailing_toggles(&self, cx: &mut Context<Self>) -> Div {
        let promoted = self.tool_panel_promoted();
        let panel_visible = self.tool_panel_open || promoted;
        h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children((promoted || self.tool_panel_open).then(|| {
                div()
                    .id("tool-panel-fullscreen-toggle")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_6()
                    .rounded_full()
                    .cursor_pointer()
                    .text_color(rgb(ui_theme::text_mid()))
                    .when(promoted, |s| s.bg(ui_theme::overlay(0x18)))
                    .hover(|s| s.bg(ui_theme::overlay(0x18)))
                    .child(
                        Icon::new(if promoted {
                            IconName::Minimize
                        } else {
                            IconName::Maximize
                        })
                        .size_4(),
                    )
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(if promoted {
                            "收回右侧面板"
                        } else {
                            "全屏显示右侧面板"
                        })
                        .build(window, cx)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if promoted {
                                this.set_stage_cover(None, window, cx);
                                this.set_tool_panel_open(true);
                            } else {
                                this.set_stage_cover(Some(StageCover::ToolPanel), window, cx);
                            }
                            this.save_state(cx);
                            cx.notify();
                        }),
                    )
            }))
            .child(
                div()
                    .id("tool-panel-toggle")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_6()
                    .rounded_full()
                    .cursor_pointer()
                    .text_color(rgb(ui_theme::text_mid()))
                    .when(panel_visible, |s| s.bg(ui_theme::overlay(0x18)))
                    .hover(|s| s.bg(ui_theme::overlay(0x18)))
                    .child(
                        if panel_visible {
                            Icon::empty().path("smelt-icons/panel-right-filled.svg")
                        } else {
                            Icon::new(IconName::PanelRight)
                        }
                        .size_4(),
                    )
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("切换右侧面板  ⌥⌘B").build(window, cx)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.toggle_tool_panel(window, cx);
                        }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{ChromeSessionTitle, ChromeTitle, chrome_title_for_stage};
    use crate::WorkspaceRoute;

    fn grok() -> ChromeSessionTitle {
        ChromeSessionTitle {
            title: ".: - Waiting for response... - PI-Desktop 插件架构与 UI 扩展边界 - grok".into(),
            model: None,
        }
    }

    fn agent_chat() -> ChromeSessionTitle {
        ChromeSessionTitle {
            title: "查登录".into(),
            model: Some("grok-4".into()),
        }
    }

    #[test]
    fn leaving_a_session_for_the_agent_catalog_clears_the_chrome_title() {
        assert_eq!(
            chrome_title_for_stage(&WorkspaceRoute::Agents, None, Some(grok()), None),
            ChromeTitle::Hidden,
        );
    }

    #[test]
    fn automations_do_not_keep_the_previous_session() {
        assert_eq!(
            chrome_title_for_stage(&WorkspaceRoute::Automations, None, Some(grok()), None),
            ChromeTitle::Hidden,
        );
    }

    #[test]
    fn agent_conversation_uses_that_thread_not_the_project_session() {
        assert_eq!(
            chrome_title_for_stage(
                &WorkspaceRoute::Agents,
                Some(agent_chat()),
                Some(grok()),
                None,
            ),
            ChromeTitle::Visible {
                title: "查登录".into(),
                model: Some("grok-4".into()),
            },
        );
    }

    #[test]
    fn project_stage_still_shows_the_session_title() {
        assert_eq!(
            chrome_title_for_stage(&WorkspaceRoute::Session, None, Some(grok()), None),
            ChromeTitle::Visible {
                title: grok().title,
                model: None,
            },
        );
        assert_eq!(
            chrome_title_for_stage(&WorkspaceRoute::Session, Some(agent_chat()), None, None),
            ChromeTitle::Hidden,
        );
    }

    #[test]
    fn plugin_surface_does_not_fall_back_to_the_previous_session() {
        assert_eq!(
            chrome_title_for_stage(
                &WorkspaceRoute::Plugin {
                    key: "com.example/board".into(),
                },
                None,
                Some(grok()),
                Some("看板".into()),
            ),
            ChromeTitle::Visible {
                title: "看板".into(),
                model: None,
            },
        );
        assert_eq!(
            chrome_title_for_stage(
                &WorkspaceRoute::Plugin {
                    key: "com.example/board".into(),
                },
                None,
                Some(grok()),
                None,
            ),
            ChromeTitle::Hidden,
        );
    }

    #[test]
    fn blank_titles_do_not_occupy_the_chrome() {
        assert_eq!(
            chrome_title_for_stage(
                &WorkspaceRoute::Session,
                None,
                Some(ChromeSessionTitle {
                    title: "   ".into(),
                    model: None,
                }),
                None,
            ),
            ChromeTitle::Hidden,
        );
    }
}
