//! 宿主自带的视图：会话列表、舞台。
//!
//! 过渡期的适配层：这几块的状态和画法还在 `Workspace` 上，没搬进各自的插件 crate（迁移
//! 顺序见 `docs/app-plugins.md`）。这里把每一块包成一个视图，登记进和插件同一张视图注册表，
//! 由布局按 id 摆放，宿主的外壳不再认识它们。拆走一块，就从这里删掉一项。

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div,
};
use gpui_kit_assets::IconName;
use smelt_plugin_api::{self as api, PluginView, ViewHandle, ViewSpec};

use crate::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Sessions,
    Stage,
}

impl Builtin {
    pub(crate) const ALL: [Self; 2] = [Self::Sessions, Self::Stage];

    /// 视图 id，布局文件里引用的就是它。
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Stage => "stage",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Sessions => "会话",
            Self::Stage => "舞台",
        }
    }

    fn icon(self) -> SharedString {
        match self {
            Self::Sessions => IconName::MessagesSquare,
            Self::Stage => IconName::AppWindow,
        }
        .path()
    }

    /// 会话列表和舞台自己管顶上那一条（拖窗口、让红黄绿按钮、标题胶囊）：单独一组时
    /// 外壳不给它们画标题带，也不让放大——舞台本来就是主角，会话列表铺满没有意义。
    fn owns_top_bar(self) -> bool {
        matches!(self, Self::Sessions | Self::Stage)
    }
}

/// 一个自带视图。内容和它的状态都在工作台上，这里只是壳。
struct BuiltinView {
    workspace: WeakEntity<Workspace>,
    kind: Builtin,
    /// 数据都在工作台上，跟着它的通知重画。
    _refresh: Subscription,
}

impl BuiltinView {
    fn new(workspace: WeakEntity<Workspace>, kind: Builtin, cx: &mut Context<Self>) -> Self {
        let _refresh = match workspace.upgrade() {
            Some(host) => cx.observe(&host, |_, _, cx| cx.notify()),
            None => Subscription::new(|| {}),
        };
        Self {
            workspace,
            kind,
            _refresh,
        }
    }
}

impl Render for BuiltinView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 内容由工作台画，和它自己的 render 一样不许碰库。
        let _render_scope = smelt_client_store::render_guard::RenderScope::enter();
        let kind = self.kind;
        let _timing = smelt_ui::frame_stats::RenderTiming::start(kind.id());
        let content = self.workspace.update(cx, |workspace, cx| match kind {
            Builtin::Sessions => workspace.render_sidebar_dock(window, cx),
            Builtin::Stage => workspace.render_stage_dock(window, cx),
        });
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .children(content.ok())
    }
}

impl PluginView for BuiltinView {
    /// 打开时要看的东西由工作台定（当前会话、当前项目），参数没有用处。
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 把自带视图登记进视图注册表。工作台建好时调；窗口重开时新工作台再登记一遍，替换掉
/// 指着旧工作台的那份。
pub(crate) fn register(workspace: WeakEntity<Workspace>, cx: &mut App) {
    for kind in Builtin::ALL {
        let workspace = workspace.clone();
        let spec = ViewSpec {
            id: kind.id().into(),
            title: kind.title().into(),
            icon: kind.icon(),
            per_machine: false,
            draws_top_bar: kind.owns_top_bar(),
            build: Rc::new(move |_window, cx| -> Rc<dyn ViewHandle> {
                let workspace = workspace.clone();
                Rc::new(cx.new(|cx| BuiltinView::new(workspace, kind, cx)))
            }),
        };
        if let Err(error) = api::register_view(spec, cx) {
            eprintln!("[plugins] 自带视图 {} 登记失败：{error}", kind.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Builtin;

    #[test]
    fn builtin_ids_are_distinct_and_not_reserved() {
        let ids: std::collections::HashSet<_> = Builtin::ALL.iter().map(|b| b.id()).collect();
        assert_eq!(ids.len(), Builtin::ALL.len());
        for id in ids {
            assert!(!smelt_plugin_api::RESERVED_VIEW_IDS.contains(&id));
        }
    }
}
