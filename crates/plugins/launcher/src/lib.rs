//! 启动栏：侧栏顶上的「新对话」和入口列表，侧栏底下的「设置」。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。功能插件不声明自己摆在哪：布局摆了的
//! 视图在那页看；没摆的、不用参数就能开的（不是设置页、不按实例建、不是哪页出口的默认内容），
//! 在入口列表里一行，点了用 [`api::OPEN_VIEW`] 打开——宿主放进出口。不想要这些的布局不摆
//! 这两个视图，换成自己的入口。
//!
//! 两个视图：
//! - [`NAV_VIEW`]：新对话按钮（命令登记了才有）和入口列表。高度跟着内容，布局里给它一格
//!   按内容定高的位置。
//! - [`SETTINGS_VIEW`]：「设置」按钮（命令登记了才有），有设置页挂着角标时带红点。
//!
//! 什么时候重算全靠事件：视图、页、命令的登记变了，摆在布局里的视图变了，角标、看得见的视图
//! 变了。算好的结果留在视图上，render 只读它。

use std::collections::HashSet;
use std::rc::Rc;

use api::serde_json::Value;
use api::ui::truncated_line::truncated_line;
use api::ui::window_bar::window_bar;
use api::ui::{Button, ListRow, ui_theme};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api::ui::IconName;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};
use smelt_plugin_api::{
    self as api, HostEvent, OpenView, PluginView, RunCommand, ViewHandle, ViewSpec, text,
};

rust_i18n::i18n!("locales", fallback = "en");

const OWNER: &str = "smelt.launcher";
/// 新对话和入口列表。不带参数。
const NAV_VIEW: &str = "smelt.launcher/nav";
/// 「设置」按钮。不带参数。
const SETTINGS_VIEW: &str = "smelt.launcher/settings";
/// 宿主的「设置…」。
const SETTINGS: &str = "smelt.app/settings";
/// 跟默认智能体开新对话，标题里写着是哪个智能体。
// 约定：smelt.agents 的 NEW_CONVERSATION，权威定义在 crates/plugins/agents/src/contract.rs。
const NEW_CONVERSATION: &str = "smelt.agents/new-conversation";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![
            ViewSpec {
                name: "nav".into(),
                title: text!("view.nav"),
                icon: IconName::LayoutGrid.path(),
                // 一列入口，不要外壳的标题带。
                draws_top_bar: true,
                instances: false,
                settings: None,
                fills: Vec::new(),
                build: Rc::new(|_, cx| Rc::new(cx.new(Nav::new)) as Rc<dyn ViewHandle>),
            },
            ViewSpec {
                name: "settings".into(),
                title: text!("view.settings"),
                icon: IconName::Settings.path(),
                draws_top_bar: true,
                instances: false,
                settings: None,
                fills: Vec::new(),
                build: Rc::new(|_, cx| Rc::new(cx.new(SettingsButton::new)) as Rc<dyn ViewHandle>),
            },
        ],
        cx,
    )
    .expect("启动栏的视图表写错了");
}

/// 这两个视图要据此重算的事件。
fn affects_launcher(event: &HostEvent) -> bool {
    matches!(
        event,
        HostEvent::ViewsChanged
            | HostEvent::LaidOutViewsChanged
            | HostEvent::PagesChanged
            | HostEvent::CommandsChanged
            | HostEvent::ViewBadgesChanged
            | HostEvent::ShownViewsChanged
            // 入口上的名字是视图标题的快照。
            | HostEvent::LocaleChanged
    )
}

// ---- 入口 ----

/// 一个视图的入口。
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: SharedString,
    title: SharedString,
    icon: SharedString,
    /// 挂着角标：有要处理的事。
    attention: bool,
    /// 正露着（没摆的视图开在出口里，露着就是出口正显示它）。
    shown: bool,
}

/// 入口列表上有什么。
#[derive(Clone, Debug, Default, PartialEq)]
struct NavState {
    /// 新对话命令的标题（「新对话 · 写作助手」）；命令没登记是 `None`，不出按钮。
    new_conversation: Option<SharedString>,
    entries: Vec<Entry>,
}

/// 从各注册表算入口。只读内存，但要整份克隆视图表，所以在事件里做，不在 render 里。
fn nav_state(cx: &App) -> NavState {
    let indexes: HashSet<String> = api::pages(cx)
        .into_iter()
        .filter_map(|page| page.index.map(String::from))
        .collect();
    let host = api::host(cx);
    NavState {
        new_conversation: api::command(NEW_CONVERSATION, cx).map(|info| info.title),
        entries: api::views(cx)
            .into_iter()
            .filter(|view| {
                launchable(&view.id, view.settings.is_some(), view.instances, &indexes)
                    && !host.view_laid_out(&view.id, cx)
            })
            .map(|view| Entry {
                attention: api::view_badge(&view.id, cx).is_some(),
                shown: host.view_shown(&view.id, cx),
                id: view.id,
                title: view.title,
                icon: view.icon,
            })
            .collect(),
    }
}

/// 不看布局时，这个视图能不能在入口列表里一行：不是设置页（设置里开），不按实例建（要别的
/// 插件带参数建），不是出口本身、不是启动栏自己，也不是哪页出口的默认内容（点会话就回去）。
fn launchable(id: &str, settings: bool, instances: bool, indexes: &HashSet<String>) -> bool {
    !settings
        && !instances
        && id != api::OUTLET_VIEW
        && id != NAV_VIEW
        && id != SETTINGS_VIEW
        && !indexes.contains(id)
}

struct Nav {
    /// 算好的入口，render 只读它。
    state: NavState,
    _subscriptions: Vec<Subscription>,
}

impl Nav {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let subscriptions = vec![cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            if affects_launcher(event) {
                let state = nav_state(cx);
                if state != this.state {
                    this.state = state;
                    cx.notify();
                }
            }
        })];
        Self {
            state: nav_state(cx),
            _subscriptions: subscriptions,
        }
    }
}

impl PluginView for Nav {
    fn open(&mut self, _: &Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for Nav {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 贴着窗口顶时先画标题栏那一条（让开红黄绿按钮、拖窗口），入口从它下面开始。
        let area = api::safe_area(cx.entity_id(), cx);
        // 不撑满：高度跟着内容，布局按内容给这一格定高。
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(area.title_bar, |nav| {
                nav.child(window_bar("launcher-nav-bar", &area))
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(
                        self.state
                            .new_conversation
                            .clone()
                            .map(new_conversation_button),
                    )
                    .children(self.state.entries.iter().map(entry_row)),
            )
    }
}

/// 占满一行的「新对话」：名字写着默认智能体，悬停带快捷键。
fn new_conversation_button(title: SharedString) -> AnyElement {
    Button::new("launcher-new-conversation")
        .w_full()
        .min_w_0()
        .icon(IconName::Plus)
        .accessibility_label(title.clone())
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .child(truncated_line(title.clone())),
        )
        .tooltip_with_action(title, &RunCommand::new(NEW_CONVERSATION), None)
        .on_click(|_, _, cx| run(NEW_CONVERSATION, cx))
        .into_any_element()
}

/// 一个视图的入口：图标、名字、要处理时的红点；出口正显示着它时亮着。
fn entry_row(entry: &Entry) -> AnyElement {
    let selected = entry.shown;
    let id = entry.id.clone();
    ListRow::new(SharedString::from(format!("launcher-{}", entry.id)))
        .selected(selected)
        .h(ui_theme::height::md())
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(ui_theme::row_radius())
        .child(
            svg()
                .path(entry.icon.clone())
                .flex_shrink_0()
                .size(px(16.))
                .text_color(rgb(ui_theme::row_muted(selected))),
        )
        // 名字的字色由 `ListRow` 给；选中加中等字重。
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE.ui)
                .when(selected, |label| label.font_weight(TYPE.ui_medium.weight))
                .child(truncated_line(entry.title.clone())),
        )
        .when(entry.attention, |row| {
            row.child(
                div()
                    .flex_shrink_0()
                    .size(px(6.))
                    .rounded_full()
                    .bg(rgb(ui_theme::bad())),
            )
        })
        .on_click(move |_, _, cx| open(&id, cx))
        .into_any_element()
}

// ---- 设置 ----

/// 设置按钮上有什么。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SettingsState {
    /// 设置命令登记了。
    available: bool,
    /// 有设置页挂着角标。
    attention: bool,
}

fn settings_state(cx: &App) -> SettingsState {
    SettingsState {
        available: api::command(SETTINGS, cx).is_some(),
        attention: api::views(cx)
            .iter()
            .any(|view| view.settings.is_some() && api::view_badge(&view.id, cx).is_some()),
    }
}

struct SettingsButton {
    /// 算好的状态，render 只读它。
    state: SettingsState,
    _subscriptions: Vec<Subscription>,
}

impl SettingsButton {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let subscriptions = vec![cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            if affects_launcher(event) {
                let state = settings_state(cx);
                if state != this.state {
                    this.state = state;
                    cx.notify();
                }
            }
        })];
        Self {
            state: settings_state(cx),
            _subscriptions: subscriptions,
        }
    }
}

impl PluginView for SettingsButton {
    fn open(&mut self, _: &Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for SettingsButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let attention = self.state.attention;
        // 自己就是一行：贴着窗口下面的角时两头给窗口圆角让位，不贴时留标准边距。
        let insets = api::safe_area(cx.entity_id(), cx).bar_insets();
        // 普通入口，不做成通栏的大按钮。
        div()
            .size_full()
            .min_w_0()
            .pl(insets.left)
            .pr(insets.right)
            .flex()
            .items_center()
            .when(self.state.available, |bar| {
                bar.child(
                    div()
                        .id("launcher-settings")
                        .relative()
                        .flex_shrink_0()
                        .child(
                            Button::new("launcher-settings-button")
                                .quiet()
                                .icon(IconName::Settings)
                                .label(t!("settings"))
                                .on_click(|_, _, cx| run(SETTINGS, cx)),
                        )
                        .when(attention, |slot| {
                            slot.child(
                                div()
                                    .absolute()
                                    .top(px(4.))
                                    .right(px(4.))
                                    .size(px(5.))
                                    .rounded_full()
                                    .bg(rgb(ui_theme::bad())),
                            )
                        }),
                )
            })
    }
}

// ---- 执行 ----

fn open(view: &str, cx: &mut App) {
    let open = OpenView {
        view: view.to_string(),
        args: Value::Null,
    };
    report_failure(api::OPEN_VIEW.run(&open, cx), cx);
}

fn run(command: &str, cx: &mut App) {
    report_failure(api::run(command, Value::Null, cx), cx);
}

/// 等命令跑完，失败了报出来（命令自己不报）。
fn report_failure<T: 'static>(ran: Task<Result<T, String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| notify_error(error, cx));
        }
    })
    .detach();
}

fn notify_error(message: String, cx: &mut App) {
    eprintln!("[launcher] {message}");
    let notify = api::Notify {
        subtitle: t!("notify.error").to_string(),
        body: message,
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` 带进了 gpui 的 glob，其中的 `test` 宏会顶掉标准库的 `#[test]`，展开时无限递归。
    use std::prelude::rust_2024::test;

    #[test]
    fn only_views_openable_without_arguments_get_an_entry() {
        let indexes = HashSet::from(["smelt.sessions/session".to_string()]);
        assert!(launchable("smelt.archive/main", false, false, &indexes));
        assert!(
            !launchable("smelt.agents/settings", true, false, &indexes),
            "设置页"
        );
        assert!(
            !launchable("smelt.terminal/terminal", false, true, &indexes),
            "按实例建"
        );
        assert!(
            !launchable(api::OUTLET_VIEW, false, false, &indexes),
            "出口"
        );
        assert!(!launchable(NAV_VIEW, false, false, &indexes), "自己");
        assert!(!launchable(SETTINGS_VIEW, false, false, &indexes), "自己");
        assert!(
            !launchable("smelt.sessions/session", false, false, &indexes),
            "出口默认内容"
        );
    }
}
