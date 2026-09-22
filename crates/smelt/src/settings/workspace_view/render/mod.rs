//! 设置页主体元素树的渲染实现。
//!
//! 快照构建和独立窗口生命周期留在父模块；这里只组装各设置页。
//! 各页实现按主题拆到同目录的 sibling 模块。
//!
//! 左侧用默认全部展开的两级导航展示内容页；一级分类只负责分层与折叠，点击二级
//! 入口只渲染对应页面，不再把整类设置堆进一个可滚动长页。

use super::*;
use gpui_component::input::{Input, InputState};
use gpui_component::sidebar::{Sidebar, SidebarItem, SidebarMenuItem};

mod agent;
mod appearance;
mod collaboration;
mod dsh_model;
mod maintenance;
mod pi_auth;
mod pi_model;
mod pi_plugin;
mod plugins;
mod shortcuts;

/// 渲染独立设置页面：左侧两级菜单切换内容页，右侧只显示当前选中项。
pub(super) fn render_settings_content(
    entity: Entity<Workspace>,
    scope: SettingsScope,
    snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> Div {
    div().size_full().child(SettingsShell {
        entity,
        scope,
        snapshot: snapshot.clone(),
    })
}

#[derive(IntoElement)]
struct SettingsShell {
    entity: Entity<Workspace>,
    scope: SettingsScope,
    snapshot: SettingsRenderSnapshot,
}

impl RenderOnce for SettingsShell {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let search = window.use_keyed_state("settings-nav-search", cx, |window, cx| {
            InputState::new(window, cx).placeholder("搜索设置、功能或命令")
        });
        let query = search.read(cx).value().to_string();
        let plugins = cx
            .try_global::<PluginEnablementState>()
            .map(|state| state.catalog.clone())
            .unwrap_or_default();
        let scope_nav = self.scope.nav(&plugins);
        let nav = filter_settings_nav(&scope_nav, &query);
        // 选中页由 Workspace 单点持有，另一扇窗口切页会连带改到这里；不属于本窗口
        // 就回退到本窗口的首页，否则侧栏没有对应项、右侧却画着别的窗口的内容。
        let selected = if nav_contains(&scope_nav, &self.snapshot.settings_section) {
            self.snapshot.settings_section.clone()
        } else {
            self.scope.default_section()
        };
        let page = section_page(self.entity.clone(), &self.snapshot, &selected, &plugins, cx);

        h_flex()
            .size_full()
            .child(render_nav(
                self.entity.clone(),
                &nav,
                &selected,
                &query,
                search,
                cx,
            ))
            .child(
                div().flex_1().min_w_0().h_full().child(
                    // Settings 自带侧栏是按 group 滚动的，这里关掉它，只借用内容渲染。
                    Settings::new(SharedString::from(format!(
                        "settings-pane:{}:{}",
                        selected.element_key(),
                        self.snapshot.settings_page_nonce
                    )))
                    .sidebar_width(px(0.))
                    .sidebar_size_range(px(0.)..px(0.))
                    .pages(vec![page]),
                ),
            )
    }
}

fn render_nav(
    entity: Entity<Workspace>,
    nav: &[SettingsNavCategory],
    selected: &SettingsSection,
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
        .child(SettingsNavMenu {
            collapsed: false,
            items: nav
                .iter()
                .map(|category| {
                    (
                        category.id,
                        SidebarMenuItem::new(category.title)
                            .default_open(category.default_open)
                            .click_to_toggle(true)
                            .children(category.children.iter().map(|child| {
                                let is_active = selected == &child.section;
                                let entity = entity.clone();
                                let section = child.section.clone();
                                SidebarMenuItem::new(child.title.clone())
                                    .icon(child.icon.clone())
                                    .active(is_active)
                                    .on_click(move |_, _, cx| {
                                        select_section(&entity, section.clone(), cx)
                                    })
                            })),
                    )
                })
                .collect(),
        })
}

#[derive(Clone)]
struct SettingsNavMenu {
    collapsed: bool,
    items: Vec<(SettingsCategoryId, SidebarMenuItem)>,
}

impl Collapsible for SettingsNavMenu {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for SettingsNavMenu {
    fn render(
        self,
        _id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let mut items = Vec::with_capacity(self.items.len());
        for (id, item) in self.items {
            items.push(
                item.collapsed(self.collapsed)
                    .render(format!("settings-cat-{}", id.as_str()), window, cx)
                    .into_any_element(),
            );
        }
        v_flex().gap_2().children(items)
    }
}

fn select_section(entity: &Entity<Workspace>, section: SettingsSection, cx: &mut App) {
    entity.update(cx, |workspace, cx| {
        if workspace.settings_section != section {
            workspace.settings_section = section;
            cx.notify();
        }
    });
}

fn section_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    selected: &SettingsSection,
    plugins: &[InstalledPlugin],
    cx: &App,
) -> SettingPage {
    match selected {
        SettingsSection::AppearanceTheme => appearance::theme_page(entity, snapshot, cx),
        SettingsSection::AppearanceTerminal => appearance::terminal_page(entity, snapshot, cx),
        SettingsSection::AppearanceWindow => appearance::window_page(entity, snapshot, cx),
        SettingsSection::DshModel => dsh_model::dsh_model_page(entity, snapshot, cx),
        SettingsSection::PiModel => pi_model::pi_model_page(entity, snapshot, cx),
        SettingsSection::PiPlugins => pi_plugin::pi_plugin_page(entity, snapshot, cx),
        SettingsSection::AgentRuntime => agent::runtime_page(entity, snapshot, cx),
        SettingsSection::AgentLaunch => agent::launch_page(entity, snapshot, cx),
        SettingsSection::AgentWorkspace => agent::workspace_page(entity, snapshot, cx),
        SettingsSection::AgentNotify => agent::notify_page(entity, snapshot, cx),
        SettingsSection::AgentHooks => agent::hooks_page(entity, snapshot, cx),
        SettingsSection::CollaborationRemote => collaboration::remote_page(entity, snapshot, cx),
        SettingsSection::Plugin { id } => plugins::plugin_page(entity, snapshot, id, plugins, cx),
        SettingsSection::MaintenanceUpdate => maintenance::update_page(entity, snapshot, cx),
        SettingsSection::MaintenanceDaemon => maintenance::daemon_page(entity, snapshot, cx),
        SettingsSection::MaintenanceStorage => maintenance::storage_page(entity, snapshot, cx),
        SettingsSection::Shortcuts => shortcuts::shortcuts_page(entity, snapshot, cx),
    }
}
