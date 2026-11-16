//! 远程桌面：运行时那台机器的整块屏幕，打开就能操作；切到「只看」时鼠标键盘不发（设计文档
//! D9）。登记一个视图（[`VIEW`]），打开时参数带机器（`{"machine": …}`，见
//! [`api::machine_to_json`]）。哪页都没摆它，打开时宿主放进出口。
//!
//! 看不见时连接整套放掉，不在后台推画面；再露出来连回上次那台。画面和连接见 [`session`]。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

mod session;

use std::rc::Rc;

use gpui::*;
use smelt_plugin_api::{self as api, HostEvent, PluginView, RuntimeKey, ViewHandle, ViewSpec};

pub(crate) use api::ui::ui_theme;

use session::{DesktopView, page_toolbar};

/// 本插件的来源 id。
const OWNER: &str = "smelt.desktop";
/// 远程桌面视图的 id，`<来源>/<名字>`。
pub const VIEW: &str = "smelt.desktop/desktop";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![ViewSpec {
            name: "desktop".into(),
            title: "远程桌面".into(),
            icon: "smelt-icons/monitor.svg".into(),
            // 不是每台机器都有：只有装了远程桌面、连着的机器才给入口。
            per_machine: false,
            // 顶上那条是外壳的标题带，连的哪台机器和「断开」在视图自己的工具行里。
            draws_top_bar: false,
            settings: None,
            build: Rc::new(|window, cx| -> Rc<dyn ViewHandle> {
                Rc::new(cx.new(|cx| DesktopPage::new(window, cx)))
            }),
        }],
        cx,
    )
    .expect("远程桌面的视图表写错了");
}

/// 那台机器眼下的名字；已经移除是 `None`。
fn machine_name(machine: &RuntimeKey, cx: &App) -> Option<String> {
    api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|candidate| &candidate.key == machine)
        .map(|machine| machine.name)
}

/// 远程桌面视图：工具行加上那台机器的画面。
struct DesktopPage {
    machine: Option<RuntimeKey>,
    /// 机器名，机器变了时重取，render 只读它。
    machine_name: String,
    view: Option<Entity<DesktopView>>,
    /// 画面建在哪扇窗口上。主窗口关了再开是新的一扇，视图本身留在注册表里：换了窗口就
    /// 重新订阅、重建画面（网页视图是贴在窗口上的原生子视图）。
    window: AnyWindowHandle,
    _host_events: Subscription,
}

impl DesktopPage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            machine: None,
            machine_name: String::new(),
            view: None,
            window: window.window_handle(),
            _host_events: Self::watch(window, cx),
        }
    }

    fn watch(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(
            &api::events(cx),
            window,
            |this: &mut Self, _, event: &HostEvent, window, cx| match event {
                HostEvent::ShownViewsChanged => this.sync_shown(window, cx),
                HostEvent::MachinesChanged => this.sync_machine(cx),
                _ => {}
            },
        )
    }

    /// 机器改名、移除了：名字跟着换；移除了就断开。
    fn sync_machine(&mut self, cx: &mut Context<Self>) {
        let Some(machine) = &self.machine else {
            return;
        };
        match machine_name(machine, cx) {
            Some(name) => {
                if name != self.machine_name {
                    self.machine_name = name;
                    cx.notify();
                }
            }
            None => {
                if self.view.take().is_some() {
                    cx.notify();
                }
            }
        }
    }

    /// 看得见就连着，看不见就断开。
    fn sync_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !api::host(cx).view_shown(VIEW, cx) {
            if self.view.take().is_some() {
                cx.notify();
            }
            return;
        }
        if self.view.is_none()
            && let Some(machine) = self.machine.clone()
            && let Some(client) = api::host(cx).client(&machine, cx)
        {
            self.view = Some(cx.new(|cx| DesktopView::new(machine, client, window, cx)));
            cx.notify();
        }
    }
}

impl PluginView for DesktopPage {
    /// 换一台机器就重连；参数里没有 `machine`（不是本机的 `null`，是没这个字段）时接着看
    /// 上次那台。
    fn open(&mut self, args: &api::serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        if window.window_handle() != self.window {
            self.window = window.window_handle();
            self._host_events = Self::watch(window, cx);
            self.view = None;
        }
        if let Some(machine) = args.get("machine").and_then(api::machine_from_json)
            && self.machine.as_ref() != Some(&machine)
        {
            self.machine_name = machine_name(&machine, cx).unwrap_or_default();
            self.machine = Some(machine);
            self.view = None;
        }
        self.sync_shown(window, cx);
        cx.notify();
    }
}

impl Render for DesktopPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 连着时工具行归里面的画面画（连接状态、接管、断开都在那一行）。
        if let Some(view) = &self.view {
            return view.clone().into_any_element();
        }
        let reason = if self.machine.is_none() {
            "还没选要连的机器"
        } else {
            api::MACHINE_REMOVED
        };
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(page_toolbar(self.machine_name.clone()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(ui_theme::muted()))
                    .child(reason),
            )
            .into_any_element()
    }
}
