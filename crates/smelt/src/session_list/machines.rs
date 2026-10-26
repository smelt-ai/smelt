//! 机器能做的事，和侧栏底部的机器面板。
//!
//! 一台机器的操作只有一份（[`machine_actions`]）：侧栏分段标题的 `⋯`、右键菜单和机器面板
//! 都从它生成，同一件事在哪儿点都是同样的字、同样的结果。
//!
//! 机器面板是一个浮层，操作不能再套一层下拉菜单：浮层在自己范围外一按下就关，菜单从面板
//! 边上弹出去，点菜单项时面板先关、菜单跟着没了，点击落空。所以面板里点一台机器是翻到
//! 它的操作页，全在面板里点完。

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::popover::{Popover, PopoverState};
use gpui_component::{Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use smelt_ui::IconAction as _;
use smelt_ui::{Button, ListRow};

use crate::runtimes::{LinkKind, LinkTone, Runtime, RuntimeHub};
use crate::{RenameTarget, RuntimeKey, Workspace, ui_theme};

type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// 机器的一项操作。
#[derive(Clone)]
pub(crate) struct MachineAction {
    label: &'static str,
    icon: Icon,
    /// 和前一项之间隔一条线。
    separated: bool,
    run: ActionHandler,
}

/// 一台机器能做的事。远程机器多出重命名和移除。
pub(crate) fn machine_actions(
    runtime: Runtime,
    workspace: Entity<Workspace>,
    cx: &App,
) -> Vec<MachineAction> {
    let key = runtime.key().clone();
    let mut actions = vec![
        MachineAction {
            label: "打开项目…",
            icon: Icon::empty().path("smelt-icons/folder-plus.svg"),
            separated: false,
            run: Rc::new({
                let runtime = runtime.clone();
                let workspace = workspace.clone();
                move |_window, cx| {
                    let runtime = runtime.clone();
                    workspace.update(cx, |ws, cx| ws.open_project_on(runtime, cx));
                }
            }),
        },
        MachineAction {
            label: "能力库",
            icon: IconName::LayoutDashboard.into(),
            separated: false,
            run: Rc::new({
                let key = key.clone();
                let workspace = workspace.clone();
                move |window, cx| {
                    let key = key.clone();
                    workspace.update(cx, |ws, cx| ws.open_capabilities_on(key, window, cx));
                }
            }),
        },
    ];
    // 只有装了远程桌面、眼下连着的机器才有。
    if crate::runtimes::runtime_has_desktop(&key, cx) {
        actions.push(MachineAction {
            label: "远程桌面",
            icon: Icon::empty().path("smelt-icons/monitor.svg"),
            separated: false,
            run: Rc::new({
                let workspace = workspace.clone();
                move |window, cx| {
                    let runtime = runtime.clone();
                    workspace.update(cx, |ws, cx| ws.open_desktop(runtime, window, cx));
                }
            }),
        });
    }
    // 连不上、版本对不上时不必等退避：立刻再试，运行时旧就再请它升级一次。
    if crate::runtimes::runtime_can_reconnect(&key, cx) {
        actions.insert(
            0,
            MachineAction {
                label: "重新连接",
                icon: IconName::Redo.into(),
                separated: false,
                run: Rc::new({
                    let key = key.clone();
                    move |_window, cx| crate::runtime_events::reconnect(&key, cx)
                }),
            },
        );
    }
    let RuntimeKey::Remote(runtime_id) = key else {
        return actions;
    };
    actions.push(MachineAction {
        label: "重命名…",
        icon: Icon::empty().path("smelt-icons/square-pen.svg"),
        separated: true,
        run: Rc::new({
            let runtime_id = runtime_id.clone();
            let workspace = workspace.clone();
            move |window, cx| {
                let target = RenameTarget::Runtime(runtime_id.clone());
                workspace.update(cx, |ws, cx| ws.start_rename(target, window, cx));
            }
        }),
    });
    actions.push(MachineAction {
        label: "中继…",
        icon: IconName::Globe.into(),
        separated: false,
        run: Rc::new({
            let runtime_id = runtime_id.clone();
            let workspace = workspace.clone();
            move |window, cx| {
                let runtime_id = runtime_id.clone();
                workspace.update(cx, |ws, cx| ws.start_relay_edit(runtime_id, window, cx));
            }
        }),
    });
    actions.push(MachineAction {
        label: "移除…",
        icon: Icon::empty().path("smelt-icons/trash.svg"),
        separated: false,
        run: Rc::new(move |_window, cx| {
            let runtime_id = runtime_id.clone();
            workspace.update(cx, |ws, cx| ws.request_remove_runtime(runtime_id, cx));
        }),
    });
    actions
}

/// 侧栏分段标题的 `⋯` 和右键用的机器菜单。
pub(crate) fn machine_menu(
    menu: PopupMenu,
    runtime: Runtime,
    workspace: Entity<Workspace>,
    cx: &App,
) -> PopupMenu {
    machine_actions(runtime, workspace, cx)
        .into_iter()
        .fold(menu, |menu, action| {
            let menu = if action.separated {
                menu.separator()
            } else {
                menu
            };
            let run = action.run;
            menu.item(
                PopupMenuItem::new(action.label)
                    .icon(action.icon)
                    .on_click(move |_ev, window, cx| run(window, cx)),
            )
        })
}

/// 侧栏底部「设置」旁边的机器按钮：点开是所有机器的连接情况，点一台进它的操作页，
/// 「添加机器」也在这里。有机器离线、版本不对、走中继还很慢时按钮上挂个点，平时不打扰。
pub(super) fn machines_button(workspace: Entity<Workspace>, cx: &App) -> impl IntoElement + use<> {
    let needs_attention = crate::runtimes::any_runtime_needs_attention(cx);
    let trigger = Button::new("sidebar-machines-btn")
        .quiet()
        .compact()
        .icon(Icon::new(gpui_kit_assets::IconName::Server))
        .icon_action("机器");
    div()
        .relative()
        .flex_none()
        .child(
            Popover::new("sidebar-machines")
                .anchor(Anchor::BottomLeft)
                .w(px(PANEL_WIDTH))
                .trigger(trigger)
                .content(move |_, window, cx| {
                    let page = machines_page(window, cx);
                    let popover = cx.entity().downgrade();
                    machines_panel(&workspace, page, popover, cx)
                }),
        )
        .when(needs_attention, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .size(px(6.))
                    .rounded_full()
                    .bg(rgb(ui_theme::yellow())),
            )
        })
}

const PANEL_WIDTH: f32 = 320.;

/// 面板停在哪一页：`None` 是机器列表，`Some` 是那台机器的操作页。放在面板内容的元素状态
/// 里，面板一关就跟着丢掉，下次打开回到列表。
#[derive(Default)]
struct MachinesPage(Option<RuntimeKey>);

fn machines_page(window: &mut Window, cx: &mut App) -> Entity<MachinesPage> {
    window.use_keyed_state("sidebar-machines-page", cx, |_, _| MachinesPage::default())
}

/// 面板里的一台机器。
struct MachineRow {
    key: RuntimeKey,
    name: SharedString,
    detail: SharedString,
    tone: LinkTone,
    actions: Vec<MachineAction>,
}

/// 停在操作页、且那台机器还在时返回它；机器已经被移除就回到列表。
fn selected_machine(
    rows: &mut Vec<MachineRow>,
    page: &Entity<MachinesPage>,
    cx: &App,
) -> Option<MachineRow> {
    let key = page.read(cx).0.as_ref()?;
    let ix = rows.iter().position(|row| &row.key == key)?;
    Some(rows.swap_remove(ix))
}

/// 机器面板：本机在前，远程机器按添加顺序；或者停在某台机器的操作页。
fn machines_panel(
    workspace: &Entity<Workspace>,
    page: Entity<MachinesPage>,
    popover: WeakEntity<PopoverState>,
    cx: &App,
) -> Div {
    let mut runtimes = vec![Runtime::local()];
    let mut any_relay = false;
    let mut load_error = None;
    if let Some(hub) = cx.try_global::<RuntimeHub>() {
        load_error = hub.load_error.clone();
        for remote in &hub.remotes {
            any_relay |= matches!(
                remote.link.as_ref().map(|link| &link.kind),
                Some(LinkKind::Relay(_))
            );
            runtimes.push(remote.runtime.clone());
        }
    }
    let mut rows: Vec<MachineRow> = runtimes
        .into_iter()
        .map(|runtime| {
            let key = runtime.key().clone();
            let (detail, tone) = crate::runtimes::runtime_link_detail(&key, cx);
            MachineRow {
                name: crate::runtimes::runtime_name(&key, cx).into(),
                detail: detail.into(),
                tone,
                actions: machine_actions(runtime, workspace.clone(), cx),
                key,
            }
        })
        .collect();
    if let Some(row) = selected_machine(&mut rows, &page, cx) {
        return machine_actions_page(row, page, popover);
    }

    let add_workspace = workspace.clone();
    let mut panel = v_flex().p_2().gap_1().child(
        h_flex()
            .px_1()
            .pb_1()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_bright()))
                    .child("机器"),
            )
            .child(
                Button::new("machines-add")
                    .quiet()
                    .icon_size()
                    .label("添加机器")
                    .on_click(move |_ev, _window, cx| {
                        add_workspace.update(cx, |ws, cx| {
                            ws.open_settings_section(
                                crate::settings::SettingsSection::CollaborationRemote,
                                cx,
                            );
                        });
                    }),
            ),
    );
    if let Some(error) = load_error {
        panel = panel.child(
            div()
                .px_1()
                .pb_1()
                .text_xs()
                .text_color(rgb(ui_theme::yellow()))
                .child(error),
        );
    }
    panel = panel.children(rows.into_iter().map(|row| machine_list_row(row, &page)));
    if any_relay {
        panel = panel.child(
            div()
                .px_1()
                .pt_1()
                .text_xs()
                .text_color(rgb(ui_theme::text_faint()))
                .child("走中继会慢：给 Smelt 设 UDP 直连规则，或者自建一个离两边都近的中继。"),
        );
    }
    panel
}

/// 列表里的一行：名字、完整的连接说明。点整行进它的操作页。
fn machine_list_row(row: MachineRow, page: &Entity<MachinesPage>) -> impl IntoElement + use<> {
    let page = page.clone();
    let key = row.key.clone();
    ListRow::new(ElementId::from(SharedString::from(format!(
        "machines-row-{:?}",
        row.key
    ))))
    .card()
    .px_1()
    .py_1()
    .gap_2()
    .flex()
    .items_center()
    .rounded(ui_theme::row_radius())
    .debug_selector({
        let name = row.name.clone();
        move || format!("machines-row-{name}")
    })
    .on_click(move |_ev, _window, cx| {
        let key = key.clone();
        page.update(cx, |page, cx| {
            page.0 = Some(key);
            cx.notify();
        });
    })
    .child(
        v_flex()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(ui_theme::text_mid()))
                    .child(row.name),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(match row.tone {
                        LinkTone::Warning => ui_theme::yellow(),
                        LinkTone::Muted => ui_theme::text_faint(),
                    }))
                    .child(row.detail),
            ),
    )
    .child(
        Icon::new(IconName::ChevronRight)
            .small()
            .text_color(rgb(ui_theme::text_faint())),
    )
}

/// 一台机器的操作页：顶上返回和机器名，下面是它的操作。点了先关面板再执行，确认框、
/// 改名框不会被面板压着；先关也让面板把焦点交还之后，改名框再拿焦点。
fn machine_actions_page(
    row: MachineRow,
    page: Entity<MachinesPage>,
    popover: WeakEntity<PopoverState>,
) -> Div {
    let back = Button::new("machines-back")
        .quiet()
        .icon_size()
        .icon(IconName::ChevronLeft)
        .icon_action("返回")
        .on_click(move |_ev, _window, cx| {
            page.update(cx, |page, cx| {
                page.0 = None;
                cx.notify();
            });
        });
    let mut panel = v_flex().p_2().gap_1().child(
        h_flex()
            .pb_1()
            .gap_1()
            .items_center()
            .child(back)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_bright()))
                    .child(row.name),
            ),
    );
    for (ix, action) in row.actions.into_iter().enumerate() {
        if action.separated {
            panel = panel.child(div().mx_1().my_1().h(px(1.)).bg(ui_theme::hairline()));
        }
        let popover = popover.clone();
        let run = action.run;
        panel = panel.child(
            ListRow::new(("machines-action", ix))
                .card()
                .px_2()
                .py_1()
                .gap_2()
                .flex()
                .items_center()
                .rounded(ui_theme::row_radius())
                .debug_selector(move || format!("machines-action-{}", action.label))
                .on_click(move |_ev, window, cx| {
                    if let Some(popover) = popover.upgrade() {
                        popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                    }
                    run(window, cx);
                })
                .child(
                    action
                        .icon
                        .small()
                        .text_color(rgb(ui_theme::text_mid())),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(ui_theme::text_mid()))
                        .child(action.label),
                ),
        );
    }
    panel
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        Anchor, Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _,
        Render, Styled as _, TestAppContext, Window, div, px,
    };
    use gpui_component::popover::Popover;
    use gpui_component::{Icon, v_flex};
    use smelt_ui::Button;

    // 不用 `use super::*;`：父模块 glob 导入了 `gpui::*`，连带把 gpui 的 `test` 宏带进来，
    // `#[gpui::test]` 展开出的 `#[test]` 又解析回它自己，编译期无限递归。
    use super::{
        MachineAction, MachineRow, PANEL_WIDTH, machine_actions_page, machine_list_row,
        machines_page, selected_machine,
    };
    use crate::RuntimeKey;
    use crate::runtimes::LinkTone;

    /// 只有机器面板的浮层和页面，机器和操作是假的：点了只记一笔。
    struct Harness {
        clicks: Rc<Cell<usize>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            div().size_full().child(
                Popover::new("machines-test")
                    .anchor(Anchor::TopLeft)
                    .w(px(PANEL_WIDTH))
                    .trigger(
                        Button::new("machines-test-trigger")
                            .label("机器")
                            .debug_selector(|| "machines-trigger".into()),
                    )
                    .content(move |_, window, cx| {
                        let page = machines_page(window, cx);
                        let popover = cx.entity().downgrade();
                        let clicks = clicks.clone();
                        let mut rows = vec![MachineRow {
                            key: RuntimeKey::Remote("far".into()),
                            name: "far".into(),
                            detail: "版本不兼容".into(),
                            tone: LinkTone::Warning,
                            actions: vec![MachineAction {
                                label: "移除…",
                                icon: Icon::empty().path("smelt-icons/trash.svg"),
                                separated: true,
                                run: Rc::new(move |_, _| clicks.set(clicks.get() + 1)),
                            }],
                        }];
                        match selected_machine(&mut rows, &page, cx) {
                            Some(row) => machine_actions_page(row, page, popover),
                            None => v_flex()
                                .children(rows.into_iter().map(|row| machine_list_row(row, &page))),
                        }
                    }),
            )
        }
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let at = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("找不到 {selector}"))
            .center();
        cx.simulate_click(at, Modifiers::default());
        cx.run_until_parked();
    }

    /// 面板里点一台机器进它的操作页，点「移除…」要真的执行，而且面板随之关掉。以前操作是
    /// 面板里再套的下拉菜单，菜单弹到面板外，按下时面板先关，点击就落空了。
    #[gpui::test]
    fn an_action_picked_in_the_panel_runs_and_closes_it(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| Harness { clicks }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        click(cx, "machines-trigger");
        click(cx, "machines-row-far");
        click(cx, "machines-action-移除…");

        assert_eq!(clicks.get(), 1, "操作没有执行");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("machines-action-移除…").is_none(),
            "点完操作面板应该关掉"
        );
    }
}
