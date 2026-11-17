//! 设置页主体元素树的渲染实现。
//!
//! 快照构建和设置视图留在父模块；这里只组装各设置页。
//! 各页实现按主题拆到同目录的 sibling 模块。
//!
//! 导航（设置那页左边一栏的视图）按分组列出各页：分组只是一行小标题，不折叠、不缩进；
//! 点一页，内容（右边一栏的视图）只画那一页，不把整组设置堆进一个可滚动长页。

use super::*;
use gpui_base::StyledExt as _;
use smelt_ui::field::InputState;
use smelt_ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_ui::truncated_line::truncated_line;
use smelt_ui::{Icon, IconName, ListRow, TextInput};
// 宿主的 `SettingsSection` 是导航选中的页，组件的分组改个短名。
use smelt_ui::{SettingsPage, SettingsRow as Row, SettingsSection as Section};

mod appearance;
mod panel_plugin;
mod shortcuts;

pub(super) use appearance::theme_options;

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

#[derive(IntoElement)]
struct SettingsShell {
    entity: Entity<Workspace>,
    snapshot: SettingsRenderSnapshot,
    plugin_page: Option<AnyView>,
}

impl RenderOnce for SettingsShell {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selected = self.snapshot.settings_section.clone();
        // 换页、重新打开同一页（nonce 变了）都换 id，滚动从头开始。
        let id = SharedString::from(format!(
            "settings-pane:{}:{}",
            selected.element_key(),
            self.snapshot.settings_page_nonce
        ));
        let content = match section_page(id, self.entity.clone(), &self.snapshot, &selected, cx) {
            Some(page) => page.into_any_element(),
            None => match self.plugin_page {
                Some(page) => page.into_any_element(),
                None => div()
                    .p_6()
                    .text_sm()
                    .text_color(rgb(crate::ui_theme::muted()))
                    .child("这一页已经撤下了：登记它的插件卸载了，或重新加载时出错。")
                    .into_any_element(),
            },
        };

        // 限宽归各页自己（`SettingsPage`、插件的 JS 设置页）：滚动区铺满这一栏，滚动条
        // 贴着栏边，不贴着限宽后的那一列。
        div().size_full().min_w_0().child(content)
    }
}

/// 画设置的导航：返回、搜索框，和按组列出的各页（按搜索词筛过）。点一页就停到那一页。
/// 宽度由布局给（两栏之间的分隔条能拖），这里铺满这一栏；不铺底，底归所在的组
/// （`SmeltTabGroup::frame`）。返回和搜索框钉在顶上，下面的分组在 `scroll` 里滚。
pub(super) fn render_settings_nav(
    entity: Entity<Workspace>,
    selected: &SettingsSection,
    attention: Option<&SettingsSection>,
    search: &Entity<InputState>,
    scroll: &ScrollHandle,
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
    let back = nav_row(
        "settings-nav-back",
        IconName::ChevronLeft.path(),
        "返回应用".into(),
        false,
    )
    .on_click(move |_, window, cx| {
        back_entity.update(cx, |workspace, cx| workspace.go_back(window, cx));
    });
    let mut header = div()
        .flex()
        .flex_col()
        .gap_1p5()
        .pt_3()
        .px_3()
        .pb_2()
        .child(back)
        .child(TextInput::new(search).prefix(IconName::Search));
    if !query.trim().is_empty() {
        header = header.child(
            div()
                .px_1()
                .text_xs()
                .text_color(rgb(crate::ui_theme::muted()))
                .child(if result_count == 0 {
                    "没有找到相关设置，试试更短的关键词".to_string()
                } else {
                    format!("{result_count} 个相关页面")
                }),
        );
    }

    let groups = nav.iter().map(|category| {
        let rows = category.children.iter().map(|child| {
            let entity = entity.clone();
            let section = child.section.clone();
            let row = nav_row(
                SharedString::from(format!("settings-nav:{}", child.section.element_key())),
                child.icon.clone(),
                child.title.clone().into(),
                selected == &child.section,
            )
            .on_click(move |_, _, cx| select_section(&entity, section.clone(), cx));
            // 有要处理的事的那一页标个红点（`Workspace::settings_attention`）。
            if attention == Some(&child.section) {
                row.child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(crate::ui_theme::bad())),
                )
            } else {
                row
            }
        });
        // 组名是给下面分区的，不是要读的名字：小一号、中等字重、`muted`，不带图标。
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(crate::ui_theme::height::xs())
                    .px_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .font_medium()
                    .text_color(rgb(crate::ui_theme::muted()))
                    .child(category.title),
            )
            .children(rows)
    });
    let list = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_4()
        .px_3()
        .pb_3()
        .children(groups);

    div()
        .flex()
        .flex_col()
        .w_full()
        .flex_1()
        .min_h_0()
        .child(header)
        .child(Scrollable::new(
            "settings-nav-list",
            list,
            scroll,
            ScrollbarAxis::Vertical,
        ))
        .into_any_element()
}

/// 导航里的一行：图标、页名。名字用正文色，选中加中等字重；图标是附属，按选中底配色。
fn nav_row(
    id: impl Into<ElementId>,
    icon: SharedString,
    title: SharedString,
    selected: bool,
) -> ListRow {
    ListRow::new(id)
        .selected(selected)
        .h(crate::ui_theme::height::md())
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(
            Icon::path(icon)
                .size_4()
                .text_color(rgb(crate::ui_theme::row_muted(selected))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(selected, |name| name.font_medium())
                .child(truncated_line(title)),
        )
}

fn select_section(entity: &Entity<Workspace>, section: SettingsSection, cx: &mut App) {
    entity.update(cx, |workspace, cx| {
        if workspace.settings_section != section {
            workspace.settings_section = section;
            cx.notify();
        }
    });
}

/// 宿主自己的页。插件的页是 `None`：它是插件建好的视图，不在这里建。
fn section_page(
    id: SharedString,
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    selected: &SettingsSection,
    cx: &App,
) -> Option<SettingsPage> {
    Some(match selected {
        SettingsSection::Appearance => appearance::appearance_page(id, entity, snapshot, cx),
        SettingsSection::Shortcuts => shortcuts::shortcuts_page(id, cx),
        SettingsSection::PanelPlugins => panel_plugin::panel_plugin_page(id, entity, snapshot),
        SettingsSection::Plugin(_) => return None,
    })
}
