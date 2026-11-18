//! 设置导航（设置那页左边一栏的视图）：返回、搜索框，和按组列出的各页（按搜索词筛过）。
//! 点一页就停到那一页（[`super::select_page`]）。分组只是一行小标题，不折叠、不缩进。

use super::*;
use rust_i18n::t;
use smelt_plugin_api::HostEvent;
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_ui::truncated_line::truncated_line;
use smelt_ui::{Icon, IconName, ListRow, TextInput};

/// 选中哪页、哪页有要处理的事都在事件里算好（视图登记变了、角标变了、换了页），render 只取。
pub(super) struct SettingsNavView {
    /// 弱引用：「返回应用」要回工作台做。视图记在全局的视图注册表里，不能替关掉的主窗口
    /// 留着工作台。
    workspace: WeakEntity<Workspace>,
    search: Entity<smelt_ui::field::InputState>,
    /// 分组列表的滚动位置。返回和搜索框钉在顶上，不跟着滚。
    scroll: ScrollHandle,
    /// 没筛过的整棵导航。
    nav: Vec<SettingsNavCategory>,
    selected: Option<SharedString>,
    attention: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsNavView {
    pub(super) fn new(
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            smelt_ui::field::InputState::new(window, cx).placeholder(t!("settings.search"))
        });
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event, cx| {
                if matches!(event, smelt_ui::field::InputEvent::Change) {
                    cx.notify();
                }
            }),
            // 设置页登记、撤下了，导航跟着变；设置页挂上、摘掉角标，红点跟着变；换了语言，
            // 组名、页名和搜索框的占位跟着变。
            cx.subscribe_in(
                &smelt_plugin_api::events(cx),
                window,
                |this: &mut Self, _, event: &HostEvent, window, cx| match event {
                    HostEvent::ViewsChanged | HostEvent::ViewBadgesChanged => this.sync(cx),
                    HostEvent::LocaleChanged => {
                        this.search.update(cx, |search, cx| {
                            search.set_placeholder(t!("settings.search"), window, cx)
                        });
                        this.sync(cx);
                    }
                    _ => {}
                },
            ),
            cx.observe_global::<SettingsSelection>(|this, cx| this.sync(cx)),
        ];
        let mut view = Self {
            workspace,
            search,
            scroll: ScrollHandle::new(),
            nav: Vec::new(),
            selected: None,
            attention: None,
            _subscriptions: subscriptions,
        };
        view.sync(cx);
        view
    }

    /// 重读导航、选中的那页、有要处理的事的那页。只读内存。
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.nav = settings_nav(&smelt_plugin_api::views(cx));
        self.selected = resolve_page(&self.nav, chosen_page(cx)).map(|leaf| leaf.id.clone());
        self.attention = attention(cx);
        cx.notify();
    }

    /// 返回放在导航最上面，跟 Codex、Linear 的设置一样：设置换掉了整个工作台，侧栏的第一行
    /// 就是回去的路。长得和下面的导航项一样，只是不会被选中。
    fn header(&self, query: &str, result_count: usize) -> Div {
        let workspace = self.workspace.clone();
        let back = nav_row(
            "settings-nav-back",
            IconName::ChevronLeft.path(),
            t!("settings.back").into(),
            false,
        )
        .on_click(move |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| workspace.go_back(window, cx));
        });
        let header = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .pt_3()
            .px_3()
            .pb_2()
            .child(back)
            .child(TextInput::new(&self.search).prefix(IconName::Search));
        if query.trim().is_empty() {
            return header;
        }
        header.child(
            div()
                .px_1()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(crate::ui_theme::muted()))
                .child(SharedString::from(if result_count == 1 {
                    t!("settings.results.one")
                } else if result_count == 0 {
                    t!("settings.results.none")
                } else {
                    t!("settings.results.other", count = result_count)
                })),
        )
    }

    fn group(&self, category: &SettingsNavCategory) -> Div {
        let rows = category.children.iter().map(|child| {
            let id = child.id.clone();
            let row = nav_row(
                SharedString::from(format!("settings-nav:{}", child.id)),
                child.icon.clone(),
                child.title.clone().into(),
                self.selected.as_ref() == Some(&child.id),
            )
            .on_click(move |_, _, cx| select_page(id.clone(), cx));
            // 有要处理的事的那一页标个红点（见 [`attention`]）。
            if self.attention.as_ref() == Some(&child.id) {
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
                    .type_style(TYPE.text_xsmall_medium)
                    .text_color(rgb(crate::ui_theme::muted()))
                    .child(category.title.clone()),
            )
            .children(rows)
    }
}

impl Render for SettingsNavView {
    /// 宽度由布局给（两栏之间的分隔条能拖），这里铺满这一栏；不铺底，底归所在的组
    /// （`SmeltTabGroup::frame`）。返回和搜索框钉在顶上，下面的分组在滚动区里滚。
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 顶上只是标题栏那一行：红黄绿按钮、拖窗口。侧栏从上到下是一整块，不画分隔线；
        // 页名在右边内容的标题行里。
        let top =
            crate::workspace_frame::page_chrome(&smelt_plugin_api::safe_area(cx.entity_id(), cx))
                .border_b_0();
        let query = self.search.read(cx).value().to_string();
        let nav = filter_settings_nav(&self.nav, &query);
        let result_count = nav
            .iter()
            .map(|category| category.children.len())
            .sum::<usize>();
        let list = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .px_3()
            .pb_3()
            .children(nav.iter().map(|category| self.group(category)));
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(top)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(self.header(&query, result_count))
                    .child(Scrollable::new(
                        "settings-nav-list",
                        list,
                        &self.scroll,
                        ScrollbarAxis::Vertical,
                    )),
            )
    }
}

impl smelt_plugin_api::PluginView for SettingsNavView {
    /// 停在哪一页由 [`SettingsSelection`] 定，参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
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
        .type_style(TYPE.ui)
        .child(
            Icon::path(icon)
                .size_4()
                .text_color(rgb(crate::ui_theme::row_muted(selected))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .when(selected, |name| name.font_weight(TYPE.ui_medium.weight))
                .child(truncated_line(title)),
        )
}
