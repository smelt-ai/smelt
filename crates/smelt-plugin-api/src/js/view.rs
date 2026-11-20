//! JS 插件登记的视图：在插件那一份脚本里，按 `DockArea.register_panel(name, Class)` 交出的
//! 类建（`docs/app-plugins.md`「一个插件一份脚本实例」）。
//!
//! 宿主函数收不了类，gpui-shell 的面板登记是现成的「脚本交出类、宿主按名字在同一份实例里建」：
//! 宿主经 `PanelRegistry::build_panel` 建出一个 `ScriptPanel`，取出里面的脚本视图来摆，
//! 不放进 DockArea。建是同步的，视图的 `init` 跑在里面，所以打开参数在建之前放进插件的
//! 身份里，`init` 用 `opened_view()` 取（见 [`JsPlugin::begin_building`]）。再次打开时宿主
//! 找不到具体哪份脚本视图，发 `view_opened` 给插件的事件流，由插件转给那一份。

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui::*;
use gpui_base::dock::{
    DockArea, PanelBuildContext, PanelInfo, PanelRegistry, PanelState, PanelView,
};
use gpui_shell::ScriptView;
use gpui_shell::dock::ScriptPanel;
use rust_i18n::t;
use serde_json::Value;
use smelt_ui::kit::{TYPE, TypeStyled as _};

use super::{JsPlugin, Opening, PluginState};
use crate::{PluginView, ViewBuilder, ViewHandle};

/// 视图 `view` 怎么建：按 `panel`（`gpui_shell::dock::panel_name` 给的面板名）在插件那份
/// 脚本里建。按实例建的等第一次打开、有了参数再建；别的建视图时就建，参数是 `null`。
pub(super) fn builder(
    plugin: &JsPlugin,
    panel: &'static str,
    view: SharedString,
    settings: bool,
    instances: bool,
) -> ViewBuilder {
    // 登记拿着建视图的函数，插件的身份又拿着登记（经宿主模块）：这里只留弱引用，插件卸下就放得掉。
    let plugin = Rc::downgrade(&plugin.0);
    Rc::new(move |window, cx| -> Rc<dyn ViewHandle> {
        let (plugin, view) = (plugin.clone(), view.clone());
        Rc::new(cx.new(|cx| {
            cx.on_release(|this: &mut JsView, cx| this.release(cx))
                .detach();
            let mut this = JsView {
                plugin,
                panel,
                view,
                settings,
                content: None,
                opened_once: false,
            };
            if !instances {
                this.content = Some(this.build(Value::Null, window, cx));
            }
            this
        }))
    })
}

/// 一份建好的 JS 视图，或建不出来的原因。
pub(crate) struct JsView {
    plugin: Weak<RefCell<PluginState>>,
    panel: &'static str,
    view: SharedString,
    /// 设置页按设置的宽度摆。
    settings: bool,
    /// `None`：按实例建的，还没打开过。
    content: Option<Result<Built, String>>,
    /// 已经打开过一次。宿主建好视图马上打开一次，参数和建的时候一样的那一次不算「又打开」。
    opened_once: bool,
}

struct Built {
    /// 建的时候是哪一份、带的什么参数。
    opening: Opening,
    script: Entity<ScriptView>,
    /// 建出来的面板拿着脚本视图的句柄，放掉要经它（`on_removed`），见 [`JsView::release`]。
    panel: Arc<dyn PanelView>,
    window: AnyWindowHandle,
}

impl JsView {
    fn build(&self, args: Value, window: &mut Window, cx: &mut App) -> Result<Built, String> {
        let plugin = self
            .plugin
            .upgrade()
            .map(JsPlugin)
            .ok_or_else(|| t!("js.plugin_unloaded").into_owned())?;
        let no_class = || t!("js.view_no_class", name = self.view).into_owned();
        // 插件一个类都没交出过时，面板登记表可能还没建。
        if cx.try_global::<PanelRegistry>().is_none() {
            return Err(no_class());
        }
        let opening = plugin.begin_building(&self.view, args);
        let state = PanelState::new(self.panel);
        let info = PanelInfo::panel(Value::Null);
        // 不放进 DockArea，没有所属的那一块。
        let context = PanelBuildContext::new(WeakEntity::<DockArea>::new_invalid(), &state, &info);
        let panel = PanelRegistry::build_panel(self.panel, context, window, cx);
        plugin.end_building();
        let panel = panel.ok_or_else(no_class)?;
        // 脚本建不出来时 gpui-shell 给的是占位面板，原因记在日志里。
        let script = panel
            .as_any()
            .downcast_ref::<Entity<ScriptPanel>>()
            .map(|panel| panel.read(cx).view().clone())
            .ok_or_else(|| t!("js.view_not_built").into_owned())?;
        plugin.track(opening.instance, &script);
        Ok(Built {
            opening,
            script,
            panel,
            window: window.window_handle(),
        })
    }

    /// 视图放掉了：让建它的面板放掉脚本视图的句柄，不然要留到插件卸下。要窗口，推到这轮
    /// 更新之后；窗口已经关了就留给插件卸下时一起放。
    fn release(&mut self, cx: &mut App) {
        if let Some(Ok(Built { panel, window, .. })) = self.content.take() {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| panel.on_removed(window, cx));
            });
        }
    }
}

impl Render for JsView {
    /// 页面底归宿主：窗口根按地方铺底，组和视图都不铺。插件只画内容，铺了就把毛玻璃盖住。
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let base = div().size_full().min_h_0().flex().flex_col();
        // 和内置的设置页一样宽（`smelt_ui::SettingsPage` 自己限宽，脚本的页由这里限）。
        let base = if self.settings {
            base.max_w(rems(smelt_ui::settings::PAGE_MAX_WIDTH))
                .mx_auto()
        } else {
            base
        };
        match &self.content {
            None => base,
            Some(Ok(built)) => base.child(built.script.clone()),
            Some(Err(error)) => base
                .p_4()
                .type_style(TYPE.ui)
                .text_color(rgb(smelt_ui::ui_theme::bad()))
                .child(if self.settings {
                    t!("js.settings_failed", error = error)
                } else {
                    t!("js.view_failed", error = error)
                }),
        }
    }
}

impl PluginView for JsView {
    /// 按实例建的第一次打开时带着参数建；建好了的发 `view_opened` 给插件。
    fn open(&mut self, args: &Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_none() {
            self.content = Some(self.build(args.clone(), window, cx));
            self.opened_once = true;
            cx.notify();
            return;
        }
        let first = !std::mem::replace(&mut self.opened_once, true);
        let Some(Ok(built)) = &self.content else {
            return;
        };
        if first && built.opening.args == *args {
            return;
        }
        if let Some(plugin) = self.plugin.upgrade() {
            JsPlugin(plugin).reopened(Opening {
                args: args.clone(),
                ..built.opening.clone()
            });
        }
    }
}

/// `view` 是 JS 插件的视图时，它建不出来、或脚本上一次画出错的原因。`smelt plugin check`
/// 建一遍各视图、画一帧后用它查。
pub fn script_view_error(view: &AnyView, cx: &App) -> Option<String> {
    let view = view.clone().downcast::<JsView>().ok()?;
    match &view.read(cx).content {
        None => None,
        Some(Err(error)) => Some(error.clone()),
        Some(Ok(built)) => built.script.read(cx).build_error().map(str::to_string),
    }
}
