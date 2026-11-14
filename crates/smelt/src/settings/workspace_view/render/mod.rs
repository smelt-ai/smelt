//! 设置页主体元素树的渲染实现。
//!
//! 快照构建和独立窗口生命周期留在父模块；这里只组装各设置页。
//! 各页实现按主题拆到同目录的 sibling 模块。
//!
//! 左侧导航按分组列出各页：分组只是一行小标题，不折叠、不缩进；点一页只渲染那一页，
//! 不把整组设置堆进一个可滚动长页。

use super::*;
use gpui_component::input::{Input, InputState, Textarea};
use gpui_component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};

mod agent;
mod appearance;
mod collaboration;
mod maintenance;
mod notify;
mod panel_plugin;
mod shortcuts;

/// 渲染独立设置页面：左侧分组导航切换内容页，右侧只显示当前选中项。选中插件的页时
/// `plugin_page` 是建好的那一页（由设置内容视图在事件里建，见 `SettingsContentView`）。
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

#[derive(IntoElement)]
struct SettingsShell {
    entity: Entity<Workspace>,
    snapshot: SettingsRenderSnapshot,
    plugin_page: Option<AnyView>,
}

impl RenderOnce for SettingsShell {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let search = window.use_keyed_state("settings-nav-search", cx, |window, cx| {
            InputState::new(window, cx).placeholder("搜索设置、功能或命令")
        });
        let query = search.read(cx).value().to_string();
        let nav = filter_settings_nav(&settings_nav(&smelt_plugin_api::settings_pages(cx)), &query);
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

        h_flex()
            .size_full()
            .child(render_nav(
                self.entity.clone(),
                &nav,
                &selected,
                self.snapshot.attention.as_ref(),
                &query,
                search,
                cx,
            ))
            .child(div().flex_1().min_w_0().h_full().child(content))
    }
}

fn render_nav(
    entity: Entity<Workspace>,
    nav: &[SettingsNavCategory],
    selected: &SettingsSection,
    attention: Option<&SettingsSection>,
    query: &str,
    search: Entity<InputState>,
    cx: &App,
) -> impl IntoElement {
    let result_count = nav
        .iter()
        .map(|category| category.children.len())
        .sum::<usize>();
    let mut header = v_flex()
        .w_full()
        .gap_1p5()
        .child(Input::new(&search).prefix(IconName::Search));
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

    Sidebar::new("settings-nav")
        .w(px(260.))
        .flex_none()
        .h_full()
        .border_0()
        .border_r_1()
        .border_color(cx.theme().border)
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
                                .bg(rgb(crate::ui_theme::red()))
                        })
                    } else {
                        item
                    }
                },
            )))
        }))
}

fn select_section(entity: &Entity<Workspace>, section: SettingsSection, cx: &mut App) {
    entity.update(cx, |workspace, cx| {
        if workspace.settings_section != section {
            // 远程页的延迟平时只在连接换路径时读，切过去重读一次。
            let remote = section == SettingsSection::Remote;
            // 资源页只在看得见时采样：切过去要叫醒采样循环；离开也叫醒，循环才能确认
            // 看不见了，马上睡下，不用再等一轮。
            let resources = section == SettingsSection::Resources
                || workspace.settings_section == SettingsSection::Resources;
            workspace.settings_section = section;
            if resources {
                workspace.wake_resource_monitor();
            }
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
        SettingsSection::Notifications => notify::notify_page(),
        SettingsSection::Shortcuts => shortcuts::shortcuts_page(entity, snapshot, cx),
        SettingsSection::PanelPlugins => panel_plugin::panel_plugin_page(entity, snapshot),
        SettingsSection::Agents => {
            let page = agent::agents_page(entity, snapshot, cx);
            // 这一页改的都是本机运行时上的东西（CLI、hooks、启动命令）。
            if crate::runtimes::has_remote_runtimes(cx) {
                page.title_suffix(|_, _| crate::runtimes::local_runtime_scope_chip())
            } else {
                page
            }
        }
        SettingsSection::Remote => collaboration::remote_page(entity, snapshot, cx),
        SettingsSection::Resources => {
            crate::resource_monitor::resources_page(entity, snapshot.resources.clone())
        }
        SettingsSection::Maintenance => maintenance::maintenance_page(entity, snapshot, cx),
        SettingsSection::About => maintenance::about_page(entity, snapshot, cx),
        SettingsSection::Plugin(_) => return None,
    })
}
