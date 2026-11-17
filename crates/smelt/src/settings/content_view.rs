//! 设置内容（设置那页右边一栏的视图）：标题行写着停着的那页的名字，下面放那一页。每一页
//! 都是登记的视图（宿主自己的也是），这里只按 [`SettingsSelection`] 取来摆上，不认识哪一页。

use super::*;
use smelt_plugin_api::HostEvent;
use smelt_ui::kit::{TYPE, TypeStyled as _};

pub(super) struct SettingsContentView {
    /// 停着的那页：视图 id、导航里写的名字、建好的视图。换了页、视图登记变了时在事件里
    /// 重取（见 [`Self::sync`]），render 只取。
    page: Option<(SharedString, SharedString, AnyView)>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsContentView {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe_in(
                &smelt_plugin_api::events(cx),
                window,
                |this: &mut Self, _, event: &HostEvent, window, cx| {
                    if matches!(event, HostEvent::ViewsChanged) {
                        this.sync(window, cx);
                    }
                },
            ),
            cx.observe_global_in::<SettingsSelection>(window, |this, window, cx| {
                this.sync(window, cx)
            }),
            // 视图放掉（登记被替换）时，露着的设置页随之清空。
            cx.on_release(|_, cx| crate::plugin_host::set_settings_shown(None, cx)),
        ];
        // 建它的地方（打开设置、装布局）可能正更新着别的实体，推后一拍再建页。
        cx.defer_in(window, |this, window, cx| this.sync(window, cx));
        Self {
            page: None,
            _subscriptions: subscriptions,
        }
    }

    /// 取停着的那页：还没建就建，新建的打开一次；再告诉插件设置停在哪页上。设置视图本身
    /// 露没露着由工作台报，两样并起来才是这一页看不看得见（见 `ShownViews`）。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nav = settings_nav(&smelt_plugin_api::views(cx));
        let leaf = resolve_page(&nav, chosen_page(cx));
        let page = leaf.and_then(|leaf| {
            let (view, built) = smelt_plugin_api::ensure_view(&leaf.id, window, cx)?;
            if built {
                view.open(&serde_json::Value::Null, window, cx);
            }
            Some((
                leaf.id.clone(),
                SharedString::from(leaf.title.clone()),
                view.view(),
            ))
        });
        crate::plugin_host::set_settings_shown(page.as_ref().map(|(id, ..)| id.clone()), cx);
        self.page = page;
        cx.notify();
    }
}

impl Render for SettingsContentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 工具栏就是标题栏那一行，只写这一页的名字（返回在左边导航的最上面）。
        let header =
            crate::workspace_frame::page_chrome(&smelt_plugin_api::safe_area(cx.entity_id(), cx))
                .gap_2()
                .children(self.page.as_ref().map(|(_, title, _)| {
                    div()
                        .min_w_0()
                        .truncate()
                        .type_style(TYPE.ui_medium)
                        .text_color(rgb(crate::ui_theme::text()))
                        .child(title.clone())
                }));
        // 限宽归各页自己（`SettingsPage`、插件的 JS 设置页）：滚动区铺满这一栏，滚动条
        // 贴着栏边，不贴着限宽后的那一列。
        let body = match &self.page {
            Some((.., view)) => view
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            None => div()
                .p_6()
                .type_style(TYPE.ui)
                .text_color(rgb(crate::ui_theme::muted()))
                .child("这一页已经撤下了：登记它的插件卸载了，或重新加载时出错。")
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().min_w_0().child(body))
    }
}

impl smelt_plugin_api::PluginView for SettingsContentView {
    /// 停在哪一页由 [`SettingsSelection`] 定，参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}
