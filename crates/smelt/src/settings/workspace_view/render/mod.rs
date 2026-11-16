//! 设置页主体元素树的渲染实现。
//!
//! 快照构建和设置视图留在父模块；这里只组装各设置页。
//! 各页实现按主题拆到同目录的 sibling 模块。
//!
//! 导航（设置那页左边一栏的视图）按分组列出各页：分组只是一行小标题，不折叠、不缩进；
//! 点一页，内容（右边一栏的视图）只画那一页，不把整组设置堆进一个可滚动长页。

use super::*;
use gpui_component::input::InputState;
use gpui_component::sidebar::{Sidebar, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem};
use smelt_ui::{TextArea, TextInput};

mod agent;
mod appearance;
mod collaboration;
mod maintenance;
mod panel_plugin;
mod shortcuts;

/// 画设置的内容：只画导航选中的那一页。选中插件的页时 `plugin_page` 是建好的那一页（由
/// 内容视图在事件里建，见 `SettingsContentView`）。
pub(super) fn render_settings_content(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    plugin_page: Option<AnyView>,
    _cx: &App,
) -> Div {
    div().size_full().child(SettingsShell {
        entity,
        snapshot: snapshot.clone(),
        plugin_page,
    })
}

/// 设置内容一栏最宽多少（rem）。
const SETTINGS_CONTENT_MAX_WIDTH: f32 = 45.;

#[derive(IntoElement)]
struct SettingsShell {
    entity: Entity<Workspace>,
    snapshot: SettingsRenderSnapshot,
    plugin_page: Option<AnyView>,
}

impl RenderOnce for SettingsShell {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selected = self.snapshot.settings_section.clone();
        let content = match section_page(self.entity.clone(), &self.snapshot, &selected, cx) {
            Some(page) => smelt_plugin_api::settings_pane(
                SharedString::from(format!(
                    "settings-pane:{}:{}",
                    selected.element_key(),
                    self.snapshot.settings_page_nonce
                )),
                page,
            )
            .into_any_element(),
            None => match self.plugin_page {
                Some(page) => page.into_any_element(),
                None => div()
                    .p_6()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("这一页已经撤下了：登记它的插件卸载了，或重新加载时出错。")
                    .into_any_element(),
            },
        };

        // 表单不跟着窗口无限拉宽：一行的名字和开关离太远就对不上了。窄于这个宽度时照常
        // 跟着缩；宽出来的部分两边平分。
        h_flex().size_full().min_w_0().justify_center().child(
            div()
                .h_full()
                .w_full()
                .min_w_0()
                .max_w(rems(SETTINGS_CONTENT_MAX_WIDTH))
                .child(content),
        )
    }
}

/// 画设置的导航：搜索框，和按组列出的各页（按搜索词筛过）。点一页就停到那一页。
pub(super) fn render_settings_nav(
    entity: Entity<Workspace>,
    selected: &SettingsSection,
    attention: Option<&SettingsSection>,
    search: &Entity<InputState>,
    cx: &App,
) -> AnyElement {
    let query = search.read(cx).value().to_string();
    let nav = filter_settings_nav(&settings_nav(&smelt_plugin_api::views(cx)), &query);
    let result_count = nav
        .iter()
        .map(|category| category.children.len())
        .sum::<usize>();
    // 返回放在导航最上面，跟 Codex、Linear 的设置一样：设置换掉了整个工作台，侧栏的第一行
    // 就是回去的路。长得和下面的导航项一样，只是不会被选中。
    let back_entity = entity.clone();
    let back = BackRow(
        SidebarMenu::new().child(
            SidebarMenuItem::new("返回应用")
                .icon(IconName::ChevronLeft)
                .on_click(move |_, window, cx| {
                    back_entity.update(cx, |workspace, cx| workspace.go_back(window, cx));
                }),
        ),
    );
    let mut header = v_flex()
        .w_full()
        .gap_1p5()
        .child(back)
        .child(TextInput::new(search).prefix(IconName::Search));
    if !query.trim().is_empty() {
        header = header.child(
            div()
                .px_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if result_count == 0 {
                    "没有找到相关设置，试试更短的关键词".to_string()
                } else {
                    format!("{result_count} 个相关页面")
                }),
        );
    }

    // 宽度由布局给（两栏之间的分隔条能拖），这里铺满这一栏。上游 Sidebar 自己铺一层实色的
    // `sidebar` 底，会盖住窗口材质；这一栏的底归所在的组（`SmeltTabGroup::frame`），这里清掉。
    Sidebar::new("settings-nav")
        .bg(gpui::transparent_black())
        .w_full()
        .flex_1()
        .min_h_0()
        .border_0()
        .collapsible(false)
        .collapsed(false)
        .header(header)
        .children(nav.iter().enumerate().map(|(ix, category)| {
            // 组与组之间空一截：上游 Sidebar 的列表项之间没有间距，小标题也不带上边距，
            // 不空开的话两组贴在一起像一组。
            let menu = if ix + 1 == nav.len() {
                SidebarMenu::new()
            } else {
                SidebarMenu::new().pb_4()
            };
            SidebarGroup::new(category.title).child(menu.children(category.children.iter().map(
                |child| {
                    let entity = entity.clone();
                    let section = child.section.clone();
                    let item = SidebarMenuItem::new(child.title.clone())
                        .icon(child.icon.clone())
                        .active(selected == &child.section)
                        .on_click(move |_, _, cx| select_section(&entity, section.clone(), cx));
                    // 有要处理的事的那一页标个红点，跟侧栏「设置」上的是同一件事。
                    if attention == Some(&child.section) {
                        item.suffix(|_, _| {
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(rgb(crate::ui_theme::bad()))
                        })
                    } else {
                        item
                    }
                },
            )))
        }))
        .into_any_element()
}

/// 上游的 `SidebarMenu` 只是 `SidebarItem`，只能放进 `Sidebar` 的列表，放不进 `header`；
/// 列表又只收同一种项（下面是 `SidebarGroup`）。包一层借它自己的画法，返回这一行和下面
/// 的导航项长得一样。
#[derive(IntoElement)]
struct BackRow(SidebarMenu);

impl RenderOnce for BackRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        SidebarItem::render(self.0, "settings-nav-back", window, cx)
    }
}

fn select_section(entity: &Entity<Workspace>, section: SettingsSection, cx: &mut App) {
    entity.update(cx, |workspace, cx| {
        if workspace.settings_section != section {
            // 远程页的延迟平时只在连接换路径时读，切过去重读一次。
            let remote = section == SettingsSection::Remote;
            workspace.settings_section = section;
            if remote {
                crate::runtimes::sample_links(cx);
            }
            cx.notify();
        }
    });
}

/// 宿主自己的页。插件的页是 `None`：它是插件建好的视图，不是 `SettingPage`。
fn section_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    selected: &SettingsSection,
    cx: &App,
) -> Option<SettingPage> {
    Some(match selected {
        SettingsSection::Appearance => appearance::appearance_page(entity, snapshot, cx),
        SettingsSection::Shortcuts => shortcuts::shortcuts_page(entity, snapshot, cx),
        SettingsSection::PanelPlugins => panel_plugin::panel_plugin_page(entity, snapshot),
        SettingsSection::Agents => agent::agents_page(entity, snapshot, cx),
        SettingsSection::Remote => collaboration::remote_page(entity, snapshot, cx),
        SettingsSection::Maintenance => maintenance::maintenance_page(entity, snapshot, cx),
        SettingsSection::About => maintenance::about_page(entity, snapshot, cx),
        SettingsSection::Plugin(_) => return None,
    })
}
