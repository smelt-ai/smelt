//! 对话框：卡片外观、正文排法和确认框。
//!
//! 所有对话框都是 kit 的窗口对话框（`window.open_dialog`），用 [`present`] 摆成 smelt 的样子，
//! 正文用 [`content`]、底部按钮行用 [`footer`]。确认框（[`Confirm`]）是「删除 / 丢弃 /
//! 重命名」这类二选一的问答，底部按钮用本库的 [`Button`]，不用 kit 自带的。
//!
//! 危险确认里 Enter 等于取消，只有点按钮才执行（macOS 的惯例：破坏性操作不是默认按钮）。
//! kit 的对话框把 Enter 绑成「确定」，不管焦点在哪个按钮上，删除确认不能这样。

use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, Div, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled, Window, div, prelude::FluentBuilder as _,
    px, rgb,
};
use gpui_base::actions::{Cancel, Confirm as ConfirmAction};
use gpui_component::dialog::{Dialog, DialogDescription, DialogEntrance, DialogTitle};
use gpui_component::{WindowExt as _, h_flex, v_flex};

use crate::{Button, ui_theme};

/// 卡片圆角。
pub const RADIUS: f32 = 20.;
/// 卡片内边距。
pub const PADDING: f32 = 24.;
/// 块之间的间距。
pub const GAP: f32 = 16.;
/// 确认框的宽度。
pub const CONFIRM_WIDTH: f32 = 440.;

/// 对话框卡片：`surface` 底、20 圆角、24 内边距、块之间 [`GAP`]。
///
/// 外沿和阴影不在这里：kit 的 `Dialog` 自带 1px `border`（主题里就是 `hair`）和随入场
/// 动画淡入的阴影，盖不掉也不该盖。
pub fn surface<E: Styled>(element: E) -> E {
    element
        .bg(rgb(ui_theme::surface()))
        .rounded(px(RADIUS))
        .p(px(PADDING))
        .gap(px(GAP))
}

/// kit 的 `Dialog` 按 smelt 的样子摆：[`surface`] 卡片，垂直居中，原地淡入。
///
/// kit 默认停在离顶部十分之一窗高的地方、从窗口顶上滑下来，像网页的弹层；桌面上对话框
/// 居中出现。
pub fn present(dialog: Dialog) -> Dialog {
    surface(dialog).centered().entrance(DialogEntrance::Fade)
}

/// 对话框正文：标题和说明挨着放在最上面，往下每块隔 [`GAP`]。放进 `Dialog::child`，
/// 按钮行另外放进 `Dialog::footer`（见 [`footer`]），正文太长时只有它滚动。
pub fn content(title: impl Into<SharedString>, description: Option<SharedString>) -> Div {
    v_flex().gap(px(GAP)).child(
        v_flex()
            .gap_1()
            .child(DialogTitle::new().child(title.into()))
            .when_some(description, |heading, text| {
                heading.child(DialogDescription::new().child(text))
            }),
    )
}

/// 对话框底部的按钮行：靠右，从左到右是退出这一步的、其它选择、默认那个。
pub fn footer() -> Div {
    h_flex().w_full().justify_end().gap_2()
}

type Callback = Rc<dyn Fn(&mut Window, &mut App)>;
/// 确认的回调，返回框能不能关；返回 false 框留着（比如仓库还在跑别的写入，等它跑完再点）。
type ConfirmCallback = Rc<dyn Fn(&mut Window, &mut App) -> bool>;
type BodyBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// 确认框：标题、说明、可选的正文，底部「取消」和一个确认按钮。
///
/// ```ignore
/// Confirm::new("确定删除这条自动化吗？", "删除", move |_, cx| {
///     delete(cx);
///     true
/// })
///     .danger()
///     .description("删了就没了。")
///     .open(window, cx);
/// ```
pub struct Confirm {
    title: SharedString,
    description: Option<SharedString>,
    body: Option<BodyBuilder>,
    action: SharedString,
    danger: bool,
    on_confirm: ConfirmCallback,
    alternative: Option<(SharedString, Callback)>,
    on_close: Option<Callback>,
}

impl Confirm {
    /// `action` 是确认按钮上的字，写动词（「删除」「重命名」），不写「确定」。
    /// `on_confirm` 返回框能不能关。
    pub fn new(
        title: impl Into<SharedString>,
        action: impl Into<SharedString>,
        on_confirm: impl Fn(&mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            description: None,
            body: None,
            action: action.into(),
            danger: false,
            on_confirm: Rc::new(on_confirm),
            alternative: None,
            on_close: None,
        }
    }

    /// 不可撤销的确认：按钮换成危险实底，Enter 不再执行。
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// 标题下面的一段说明。
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// 说明下面的正文，比如重命名的输入框、一行警告。每次重绘都会重新构建。
    pub fn body(mut self, body: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        self.body = Some(Rc::new(body));
        self
    }

    /// 取消和确认之间的第三个选择（「不保存，直接切换」），次要按钮，点了就关。
    pub fn alternative(
        mut self,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.alternative = Some((label.into(), Rc::new(on_click)));
        self
    }

    /// 不管怎么关（确认、取消、Esc）都会跑，在确认的回调之后。
    pub fn on_close(mut self, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(on_close));
        self
    }

    pub fn open(self, window: &mut Window, cx: &mut App) {
        let this = Rc::new(self);
        window.open_alert_dialog(cx, move |alert, window, cx| {
            let on_ok = (!this.danger).then(|| this.on_confirm.clone());
            // 和 [`present`] 同样摆法；`AlertDialog` 是另一个类型，套不上那个函数。
            surface(alert)
                .centered()
                .entrance(DialogEntrance::Fade)
                .width(px(CONFIRM_WIDTH))
                .close_button(false)
                .title(this.title.clone())
                .when_some(this.description.clone(), |alert, text| {
                    alert.description(text)
                })
                .when_some(this.body.clone(), |alert, body| {
                    alert.child(body(window, cx))
                })
                // Enter 和确认按钮都走这里。危险确认里它只负责关掉，执行留给按钮自己。
                .on_ok(move |_, window, cx| match &on_ok {
                    Some(on_ok) => on_ok(window, cx),
                    None => true,
                })
                .when_some(this.on_close.clone(), |alert, on_close| {
                    alert.on_close(move |_, window, cx| on_close(window, cx))
                })
                .footer(confirm_footer(&this))
        });
    }
}

fn confirm_footer(confirm: &Confirm) -> impl IntoElement {
    let ok = Button::new("dialog-confirm")
        .large()
        .label(confirm.action.clone())
        .debug_selector(|| "dialog-confirm".into());
    let ok = if confirm.danger {
        ok.danger()
    } else {
        ok.primary()
    };
    footer()
        .child(DialogButton {
            anchor_key: "smelt-dialog-cancel-anchor",
            button: Button::new("dialog-cancel")
                .quiet()
                .large()
                .label("取消")
                .debug_selector(|| "dialog-cancel".into()),
            run: None,
            action: Rc::new(Cancel),
        })
        .when_some(confirm.alternative.clone(), |row, (label, run)| {
            row.child(DialogButton {
                anchor_key: "smelt-dialog-alternative-anchor",
                button: Button::new("dialog-alternative")
                    .large()
                    .label(label)
                    .debug_selector(|| "dialog-alternative".into()),
                run: Some(Rc::new(move |window: &mut Window, cx: &mut App| {
                    run(window, cx);
                    true
                })),
                action: Rc::new(Cancel),
            })
        })
        .child(DialogButton {
            anchor_key: "smelt-dialog-confirm-anchor",
            button: ok,
            // 危险确认的 `on_ok` 不执行，按钮自己先跑；普通确认交给 `on_ok`，和 Enter 同一条路。
            run: confirm.danger.then(|| confirm.on_confirm.clone()),
            action: Rc::new(ConfirmAction { secondary: false }),
        })
}

/// 对话框底部的按钮：先跑自己的回调，再把关闭动作派发给它所在的对话框，
/// 对话框据此跑 `on_ok` / `on_cancel` 和 `on_close`。
///
/// 动作从按钮里一个零尺寸的焦点节点派发，沿按钮自己的祖先冒到对话框，而不是从当前焦点
/// 派发：原生 WebView 会把焦点抢走，那时按焦点派发就到不了对话框，按钮点了没反应。
/// 直接调 `window.close_dialog` 也不行，它不跑 `on_close`。
#[derive(IntoElement)]
struct DialogButton {
    /// 同一个对话框里的几个按钮各用各的，状态才不会串。
    anchor_key: &'static str,
    button: Button,
    /// 先跑它，返回 false 就不派发，框留着。
    run: Option<ConfirmCallback>,
    action: Rc<dyn Action>,
}

impl RenderOnce for DialogButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let anchor = window
            .use_keyed_state(self.anchor_key, cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let run = self.run;
        let action = self.action;
        self.button
            .child(div().absolute().size_0().track_focus(&anchor))
            .on_click(move |_, window, cx| {
                if let Some(run) = &run
                    && !run(window, cx)
                {
                    return;
                }
                dispatch_from(&anchor, action.as_ref(), window, cx);
            })
    }
}

/// 从 `anchor` 派发；它这一帧没画出来时（不该发生）退回按焦点派发。
fn dispatch_from(anchor: &FocusHandle, action: &dyn Action, window: &mut Window, cx: &mut App) {
    if anchor.is_focused(window) || anchor.contains(anchor, window) {
        anchor.dispatch_action(action, window, cx);
    } else {
        window.dispatch_action(action.boxed_clone(), cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{
        AppContext as _, Context, Modifiers, Render, TestAppContext, VisualTestContext, size,
    };
    use gpui_component::WindowExt as _;

    use super::*;

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    #[derive(Clone, Default)]
    struct Counts {
        confirmed: Rc<Cell<usize>>,
        alternative: Rc<Cell<usize>>,
        closed: Rc<Cell<usize>>,
    }

    fn window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Host);
            gpui_component::Root::new(view, window, cx)
        });
        cx.simulate_resize(size(px(1000.), px(800.)));
        cx
    }

    fn open(cx: &mut VisualTestContext, danger: bool, counts: &Counts) {
        let confirmed = counts.confirmed.clone();
        let alternative = counts.alternative.clone();
        let closed = counts.closed.clone();
        cx.update(|window, cx| {
            let confirm = Confirm::new("标题", "删除", move |_, _| {
                confirmed.set(confirmed.get() + 1);
                true
            })
            .alternative("另一个", move |_, _| {
                alternative.set(alternative.get() + 1)
            })
            .on_close(move |_, _| closed.set(closed.get() + 1));
            let confirm = if danger { confirm.danger() } else { confirm };
            confirm.open(window, cx);
        });
        frame(cx);
        frame(cx);
    }

    fn frame(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn click(cx: &mut VisualTestContext, id: &'static str) {
        let bounds = cx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} was not painted"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        frame(cx);
    }

    fn is_open(cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| window.has_active_dialog(cx))
    }

    #[gpui::test]
    fn enter_on_a_danger_confirm_closes_without_running_it(cx: &mut TestAppContext) {
        let cx = window(cx);
        let counts = Counts::default();
        open(cx, true, &counts);

        cx.simulate_keystrokes("enter");
        frame(cx);

        assert_eq!(counts.confirmed.get(), 0);
        assert_eq!(counts.closed.get(), 1);
        assert!(!is_open(cx));
    }

    #[gpui::test]
    fn enter_on_a_plain_confirm_runs_it_then_closes(cx: &mut TestAppContext) {
        let cx = window(cx);
        let counts = Counts::default();
        open(cx, false, &counts);

        cx.simulate_keystrokes("enter");
        frame(cx);

        assert_eq!(counts.confirmed.get(), 1);
        assert_eq!(counts.closed.get(), 1);
        assert!(!is_open(cx));
    }

    #[gpui::test]
    fn the_danger_button_runs_once_then_closes(cx: &mut TestAppContext) {
        let cx = window(cx);
        let counts = Counts::default();
        open(cx, true, &counts);

        click(cx, "dialog-confirm");

        assert_eq!(counts.confirmed.get(), 1);
        assert_eq!(counts.closed.get(), 1);
        assert!(!is_open(cx));
    }

    #[gpui::test]
    fn a_confirm_that_returns_false_stays_open(cx: &mut TestAppContext) {
        let cx = window(cx);
        let closed = Rc::new(Cell::new(0));
        let on_close = closed.clone();
        cx.update(|window, cx| {
            Confirm::new("标题", "丢弃", |_, _| false)
                .danger()
                .on_close(move |_, _| on_close.set(on_close.get() + 1))
                .open(window, cx);
        });
        frame(cx);
        frame(cx);

        click(cx, "dialog-confirm");

        assert!(is_open(cx));
        assert_eq!(closed.get(), 0);
    }

    #[gpui::test]
    fn cancel_and_the_alternative_close_without_confirming(cx: &mut TestAppContext) {
        let cx = window(cx);
        let counts = Counts::default();
        open(cx, false, &counts);
        click(cx, "dialog-cancel");
        assert!(!is_open(cx));

        open(cx, false, &counts);
        click(cx, "dialog-alternative");
        assert!(!is_open(cx));

        assert_eq!(counts.confirmed.get(), 0);
        assert_eq!(counts.alternative.get(), 1);
        assert_eq!(counts.closed.get(), 2);
    }
}
