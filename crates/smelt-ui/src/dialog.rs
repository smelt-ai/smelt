//! 对话框：smelt 自己的窗口对话框层，卡片外观、正文排法和确认框。
//!
//! 行为（焦点圈、Esc 取消、Enter 确认、点遮罩关）用 gpui-base 的 `Dialog`；对话框叠在窗口
//! 根（`gpui_base::Root`）上的一层 [`DialogLayer`] 里，[`open`] 往上压一张、[`close`] 关最上面
//! 那张、[`DialogId::close`] 关指定的那张。卡片（[`Card`]）每次重绘都由调用方重新搭，正文用 [`content`]、底部按钮行用
//! [`footer`]。确认框（[`Confirm`]）是「删除 / 丢弃 / 重命名」这类二选一的问答。
//!
//! 危险确认里 Enter 等于取消，只有点按钮才执行（macOS 的惯例：破坏性操作不是默认按钮）。
//!
//! 窗口的根要是 `gpui_base::Root`，且开窗口前调过 [`init`]（应用经 [`crate::init`]）。

use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, Context, Div, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, Render, RenderOnce, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, WeakFocusHandle, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::actions::{Cancel, Confirm as ConfirmAction};
use gpui_base::{
    DialogDescription, DialogTitle, Root, RootPlugin, StyledExt as _, TextSelection, h_flex, v_flex,
};

use crate::kit::{ControlSize, RADIUS as KIT_RADIUS, TYPE, TextStyle, elevation};
use crate::{Button, ui_theme};

/// 卡片圆角。
pub const RADIUS: f32 = KIT_RADIUS.md;
/// 卡片内边距（标题区同一值）。
pub const PADDING: f32 = 16.;
/// 块之间的间距。
pub const GAP: f32 = 16.;
/// 确认框的宽度。
pub const CONFIRM_WIDTH: f32 = 440.;
/// 卡片离窗口边至少留这么多，调用方要的宽高超出时夹进来。
const VIEWPORT_MARGIN: f32 = 32.;

/// 对话框卡片：`surface` 底、6 圆角、16 内边距、块之间 [`GAP`]。
pub fn surface<E: Styled>(element: E) -> E {
    element
        .bg(rgb(ui_theme::surface()))
        .rounded(px(RADIUS))
        .p(px(PADDING))
        .gap(px(GAP))
}

/// 对话框正文：标题和说明挨着放在最上面，往下每块隔 [`GAP`]。放进 [`Card`]，按钮行另外
/// 放进 [`Card::footer`]（见 [`footer`]），正文太长时只有正文滚动。
pub fn content(title: impl Into<SharedString>, description: Option<SharedString>) -> Div {
    // 有说明时标题放大到 15/20，没有时是 ui_medium 16 行高。
    let title_style = if description.is_some() {
        TextStyle {
            size: 15.,
            ..TYPE.ui_medium
        }
        .lh(20.)
    } else {
        TYPE.ui_medium.lh(16.)
    };
    v_flex().gap(px(GAP)).child(
        v_flex()
            .gap_1()
            .child(
                title_style
                    .apply(DialogTitle::new())
                    .text_color(rgb(ui_theme::text()))
                    .child(title.into()),
            )
            .when_some(description, |heading, text| {
                heading.child(
                    TYPE.ui
                        .apply(DialogDescription::new())
                        .text_color(rgb(ui_theme::muted()))
                        .child(text),
                )
            }),
    )
}

/// 对话框底部的按钮行：靠右，从左到右是退出这一步的、其它选择、默认那个。
pub fn footer() -> Div {
    h_flex().w_full().justify_end().gap_2()
}

/// 每次重绘调一次，返回这张卡片这一帧的样子和回调。
type Build = Rc<dyn Fn(Card, &mut Window, &mut App) -> Card>;
type Callback = Rc<dyn Fn(&mut Window, &mut App)>;
/// 返回框能不能关；返回 false 框留着（比如仓库还在跑别的写入，等它跑完再点）。
type Decision = Rc<dyn Fn(&mut Window, &mut App) -> bool>;

/// 一张对话框卡片。[`surface`] 外观加浮起阴影，宽高夹在窗口以内；子元素是正文（太长时
/// 滚动），[`Card::footer`] 是钉在底下的按钮行。
pub struct Card {
    style: StyleRefinement,
    children: Vec<AnyElement>,
    footer: Option<AnyElement>,
    on_ok: Option<Decision>,
    on_cancel: Option<Decision>,
    on_close: Option<Callback>,
    close_on_backdrop: bool,
    top: Option<Pixels>,
}

impl Card {
    fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            children: Vec::new(),
            footer: None,
            on_ok: None,
            on_cancel: None,
            on_close: None,
            close_on_backdrop: true,
            top: None,
        }
    }

    /// 钉在卡片底下的按钮行，正文滚动时它不动。
    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Enter 和派发「确认」动作时跑；返回 true 才关。不设就直接关。
    pub fn on_ok(mut self, on_ok: impl Fn(&mut Window, &mut App) -> bool + 'static) -> Self {
        self.on_ok = Some(Rc::new(on_ok));
        self
    }

    /// Esc、点遮罩、派发「取消」动作时跑；返回 true 才关。不设就直接关。
    pub fn on_cancel(
        mut self,
        on_cancel: impl Fn(&mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.on_cancel = Some(Rc::new(on_cancel));
        self
    }

    /// 不管怎么关（确认、取消、[`close`]）都会跑，在确认 / 取消的回调之后。
    pub fn on_close(mut self, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(on_close));
        self
    }

    /// 点遮罩关不关，默认关。要填东西、要人明确选一个的框关掉它。
    pub fn close_on_backdrop(mut self, close: bool) -> Self {
        self.close_on_backdrop = close;
        self
    }

    /// 卡片顶边钉在离窗口顶 `offset` 处，不再上下居中。高度随内容变的框（命令面板随搜索
    /// 结果变高变矮）用它，输入框才不跟着上下跳。
    pub fn top(mut self, offset: Pixels) -> Self {
        self.top = Some(offset);
        self
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

struct Entry {
    id: u64,
    focus: FocusHandle,
    /// 打开前焦点在哪，关掉后回去。
    previous_focus: Option<WeakFocusHandle>,
    build: Build,
    /// 上一帧卡片的 `on_close`，[`close`] 从外面关时跑它。
    on_close: Option<Callback>,
}

/// 窗口根上的对话框层：一摞开着的对话框，最上面那张拿焦点、有遮罩。
pub struct DialogLayer {
    stack: Vec<Entry>,
    next_id: u64,
}

impl DialogLayer {
    fn new(_: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            stack: Vec::new(),
            next_id: 0,
        }
    }

    /// 从摞里拿掉这张，返回它的 `on_close`。拿掉的是最上面那张，焦点回到它打开前的地方；
    /// 压在底下的被拿掉（[`DialogId::close`]），焦点留在上面那张，上面那张关掉后改回到
    /// 这张打开前的地方。
    fn remove(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> Option<Callback> {
        let ix = self.stack.iter().position(|entry| entry.id == id)?;
        let entry = self.stack.remove(ix);
        if let Some(above) = self.stack.get_mut(ix) {
            above.previous_focus = entry.previous_focus;
        } else if let Some(previous) = entry.previous_focus.and_then(|focus| focus.upgrade()) {
            previous.focus(window, cx);
        }
        TextSelection::clear(window, cx);
        cx.notify();
        entry.on_close
    }
}

impl RootPlugin for DialogLayer {}

impl Render for DialogLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let max_w = (viewport.width - px(VIEWPORT_MARGIN * 2.)).max(px(0.));
        let max_h = (viewport.height - px(VIEWPORT_MARGIN * 2.)).max(px(0.));
        let top = self.stack.len().saturating_sub(1);
        let layer = cx.weak_entity();
        let mut dialogs = Vec::with_capacity(self.stack.len());
        for (ix, entry) in self.stack.iter_mut().enumerate() {
            let card = (entry.build)(Card::new(), window, cx);
            entry.on_close = card.on_close.clone();
            dialogs.push(render_card(
                card,
                entry,
                (ix, ix == top),
                (max_w, max_h),
                layer.clone(),
                cx,
            ));
        }
        div().children(dialogs)
    }
}

fn render_card(
    card: Card,
    entry: &Entry,
    (ix, topmost): (usize, bool),
    (max_w, max_h): (Pixels, Pixels),
    layer: gpui::WeakEntity<DialogLayer>,
    cx: &mut App,
) -> AnyElement {
    let id = entry.id;
    let on_ok = card.on_ok;
    let on_cancel = card.on_cancel;
    let on_close = card.on_close;
    let top = card.top;
    // 顶边钉住时，底下仍留 VIEWPORT_MARGIN。
    let max_h = top.map_or(max_h, |top| (max_h + px(VIEWPORT_MARGIN) - top).max(px(0.)));
    // 遮罩只铺最底下那张：叠起来的几层不越压越黑。上面几张也要一块透明的接点击。
    let backdrop = div()
        .size_full()
        .when(ix == 0, |backdrop| backdrop.bg(ui_theme::scrim()));
    let body = div()
        .id(("smelt-dialog-body", id))
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .children(card.children);
    let surface = surface(div())
        .id(("smelt-dialog-card", id))
        .occlude()
        .flex()
        .flex_col()
        .max_w(max_w)
        .max_h(max_h)
        .shadow(elevation::overlay())
        .text_color(rgb(ui_theme::text()))
        .refine_style(&card.style)
        .child(body)
        .children(card.footer);
    gpui_base::Dialog::new(cx)
        .focus_handle(entry.focus.clone())
        .layer(ix, topmost)
        .close_on_backdrop_press(card.close_on_backdrop)
        .backdrop(backdrop)
        .when_some(top, |dialog, top| dialog.items_start().pt(top))
        .on_ok(move |_, window, cx| on_ok.as_ref().is_none_or(|on_ok| on_ok(window, cx)))
        .on_cancel(move |_, window, cx| {
            on_cancel
                .as_ref()
                .is_none_or(|on_cancel| on_cancel(window, cx))
        })
        .on_close(move |_, window, cx| {
            let _ = layer.update(cx, |layer, cx| layer.remove(id, window, cx));
            if let Some(on_close) = &on_close {
                on_close(window, cx);
            }
        })
        .child(surface)
        .into_any_element()
}

/// 把对话框层装到以后新开的窗口根上。应用启动、开窗口之前调一次（[`crate::init`]）。
pub fn init(cx: &mut App) {
    Root::register_plugin::<DialogLayer>(cx, DialogLayer::new);
}

fn layer(window: &Window, cx: &App) -> Option<gpui::Entity<DialogLayer>> {
    let layer = window
        .root::<Root>()
        .flatten()
        .and_then(|root| root.read(cx).plugin::<DialogLayer>());
    debug_assert!(
        layer.is_some(),
        "窗口根不是 gpui_base::Root，或开窗口前没调 smelt_ui::dialog::init"
    );
    layer
}

/// 一张开着的对话框。开关跟着调用方状态走的对话框留着它，状态没了时按它关掉这一张，
/// 不管上面还压着别的没有。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogId(u64);

impl DialogId {
    /// 关掉这一张，不问 `on_cancel`；已经关了就什么也不做。`on_close` 和 [`close`] 一样
    /// 推到这一轮事件之后跑。
    pub fn close(self, window: &mut Window, cx: &mut App) {
        let Some(layer) = layer(window, cx) else {
            return;
        };
        let on_close = layer.update(cx, |layer, cx| layer.remove(self.0, window, cx));
        if let Some(on_close) = on_close {
            window.defer(cx, move |window, cx| on_close(window, cx));
        }
    }
}

/// 开一张对话框，压在已开的上面，焦点交给它。`build` 每次重绘都跑，内容从调用方的状态读。
/// 窗口根上没有对话框层时返回 `None`。
pub fn open(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(Card, &mut Window, &mut App) -> Card + 'static,
) -> Option<DialogId> {
    let layer = layer(window, cx)?;
    let previous_focus = window.focused(cx).map(|focus| focus.downgrade());
    let focus = cx.focus_handle();
    focus.focus(window, cx);
    // 开着模态框时，背后残留的文字选区不该还能被复制。
    TextSelection::clear(window, cx);
    let id = layer.update(cx, |layer, cx| {
        let id = layer.next_id;
        layer.next_id += 1;
        layer.stack.push(Entry {
            id,
            focus,
            previous_focus,
            build: Rc::new(build),
            on_close: None,
        });
        cx.notify();
        id
    });
    Some(DialogId(id))
}

/// 关掉最上面那张，不问 `on_cancel`。没开着就什么也不做。
///
/// 它的 `on_close` 推到这一轮事件之后跑：调用方常在自己的实体更新里关框（输入框回车的
/// 订阅），`on_close` 又要改同一个实体，当场跑会撞上正在进行的更新。
pub fn close(window: &mut Window, cx: &mut App) {
    let top = layer(window, cx).and_then(|layer| layer.read(cx).stack.last().map(|e| e.id));
    if let Some(id) = top {
        DialogId(id).close(window, cx);
    }
}

/// 这扇窗口开着对话框。原生子视图（网页视图）盖得住它，开着时要让开。
pub fn is_open(window: &Window, cx: &App) -> bool {
    window
        .root::<Root>()
        .flatten()
        .and_then(|root| root.read(cx).plugin::<DialogLayer>())
        .is_some_and(|layer| !layer.read(cx).stack.is_empty())
}

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
    on_confirm: Decision,
    alternative: Option<(SharedString, Callback)>,
    on_close: Option<Callback>,
}

type BodyBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

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
        open(window, cx, move |card, window, cx| {
            // Enter 和确认按钮都走 `on_ok`。危险确认里它只负责关掉，执行留给按钮自己。
            let on_ok = (!this.danger).then(|| this.on_confirm.clone());
            let body = content(this.title.clone(), this.description.clone())
                .when_some(this.body.clone(), |content, body| {
                    content.child(body(window, cx))
                });
            let card = card
                .w(px(CONFIRM_WIDTH))
                .close_on_backdrop(false)
                .child(body)
                .footer(confirm_footer(&this))
                .on_ok(move |window, cx| on_ok.as_ref().is_none_or(|on_ok| on_ok(window, cx)));
            match this.on_close.clone() {
                Some(on_close) => card.on_close(move |window, cx| on_close(window, cx)),
                None => card,
            }
        });
    }
}

/// 对话框底部的动作是默认尺寸（Regular，24）。
const ACTION_SIZE: ControlSize = ControlSize::Regular;

fn confirm_footer(confirm: &Confirm) -> impl IntoElement {
    let ok = Button::new("dialog-confirm")
        .control_size(ACTION_SIZE)
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
                .control_size(ACTION_SIZE)
                .label("取消")
                .debug_selector(|| "dialog-cancel".into()),
            run: None,
            action: Rc::new(Cancel),
        })
        .when_some(confirm.alternative.clone(), |row, (label, run)| {
            row.child(DialogButton {
                anchor_key: "smelt-dialog-alternative-anchor",
                button: Button::new("dialog-alternative")
                    .control_size(ACTION_SIZE)
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
#[derive(IntoElement)]
struct DialogButton {
    /// 同一个对话框里的几个按钮各用各的，状态才不会串。
    anchor_key: &'static str,
    button: Button,
    /// 先跑它，返回 false 就不派发，框留着。
    run: Option<Decision>,
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

    use super::*;
    use gpui::{
        AppContext as _, Context, Modifiers, Render, TestAppContext, VisualTestContext, size,
    };

    #[test]
    fn surface_uses_card_radius_and_padding() {
        let mut actual = surface(div());
        let mut expected = div().rounded(px(6.)).p(px(16.));
        assert_eq!(actual.style().corner_radii, expected.style().corner_radii);
        assert_eq!(actual.style().padding, expected.style().padding);
    }

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
            gpui_base::init(cx);
            init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| Host);
            Root::new(view, window, cx)
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
        cx.update(|window, cx| is_open(window, cx))
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
    fn closing_a_buried_dialog_by_id_leaves_the_one_above(cx: &mut TestAppContext) {
        let cx = window(cx);
        let closed = Rc::new(Cell::new(0));
        let on_close = closed.clone();
        let (below, above) = cx.update(|window, cx| {
            let below = super::open(window, cx, move |card, _, _| {
                let on_close = on_close.clone();
                card.on_close(move |_, _| on_close.set(on_close.get() + 1))
            })
            .unwrap();
            let above = super::open(window, cx, |card, _, _| card).unwrap();
            (below, above)
        });
        frame(cx);

        cx.update(|window, cx| below.close(window, cx));
        frame(cx);
        assert_eq!(closed.get(), 1);
        assert!(is_open(cx));

        // 关过的再关一次什么也不做。
        cx.update(|window, cx| below.close(window, cx));
        cx.update(|window, cx| above.close(window, cx));
        frame(cx);
        assert_eq!(closed.get(), 1);
        assert!(!is_open(cx));
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
