//! 弹出卡片：点触发点开、点外面或 Esc 关的一块浮层，放一组就地的设置、一段详情。
//!
//! 行为（开关、焦点进出、Esc、点外面关、Enter / 空格开关）用 gpui-base 的 `Popover`，关掉后
//! 焦点回到触发点。这里只给外观：`surface` 底、1px 细边、`md` 投影、
//! 6 圆角、12 内边距、宽 200~320，离触发点 4px。打开时从触发点那侧淡入
//! （[`crate::motion::overlay_enter`]）；gpui-base 关掉就卸下内容，出场淡出做不了。
//!
//! `Styled` 改的是浮层卡片（宽、内边距）；触发点外框（参与外面排版、浮层对齐它）用
//! [`Popover::trigger_style`]。悬停出现的详情用 [`crate::hover_card::HoverCard`]。

use gpui::{
    Anchor, AnyElement, App, Context, Div, ElementId, FocusHandle, IntoElement, ParentElement as _,
    RenderOnce, StyleRefinement, Styled, Window, div, px, rgb,
};
use gpui_base::{Selectable, StyledExt as _};

pub use gpui_base::PopoverState;

use crate::kit::{RADIUS, elevation};
use crate::motion::{EnterFrom, overlay_enter};
use crate::ui_theme;

/// 浮层离触发点的距离。
pub(crate) const GAP: f32 = 4.;
/// 卡片内边距。
const PADDING: f32 = 12.;
/// 弹出卡片、悬停卡片的宽度范围。
const MIN_WIDTH: f32 = 200.;
const MAX_WIDTH: f32 = 320.;

/// 浮层卡片的底：实色面、1px 半透明边、`md` 投影，竖排，
/// 宽 200~320（调用方的宽高样式在后面，盖得过）。弹出卡片和悬停卡片共用，圆角、内边距各自定。
pub(crate) fn surface() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(MIN_WIDTH))
        .max_w(px(MAX_WIDTH))
        .bg(rgb(ui_theme::surface()))
        .text_color(rgb(ui_theme::text()))
        // 设计是标准边框色的一半透明度；主题还没有这一位，先用最近的 hair。
        .border_1()
        .border_color(ui_theme::hair())
        .shadow(elevation::md())
}

type ContentBuilder =
    Box<dyn FnOnce(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> AnyElement>;

/// 点开的浮层卡片。默认开在触发点下方、左边对齐（[`Anchor::TopLeft`]：锚的是卡片自己的
/// 左上角）；放不下时开到上方用 `Anchor::BottomLeft` 一类。
///
/// ```ignore
/// Popover::new("capabilities")
///     .anchor(Anchor::BottomLeft)
///     .w(px(320.))
///     .trigger(Button::new("open").label("能力"))
///     .content(|popover, _, cx| {
///         let popover = cx.entity().downgrade();
///         panel(popover)
///     })
/// ```
#[derive(IntoElement)]
pub struct Popover {
    base: gpui_base::Popover,
    id: ElementId,
    anchor: Anchor,
    style: StyleRefinement,
    content: Option<ContentBuilder>,
}

impl Popover {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: gpui_base::Popover::new(id.clone()).offset(px(GAP)),
            id,
            anchor: Anchor::TopLeft,
            style: StyleRefinement::default(),
            content: None,
        }
    }

    /// 卡片的哪个角对着触发点，默认 [`Anchor::TopLeft`]。
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// 触发点：按钮、列表行这类能显示「开着」的控件。
    pub fn trigger<T: Selectable + IntoElement + 'static>(mut self, trigger: T) -> Self {
        self.base = self.base.trigger(trigger);
        self
    }

    /// 打开时把焦点交给卡片里的这个控件（搜索框），不给时交给卡片自己。
    pub fn track_focus(mut self, handle: &FocusHandle) -> Self {
        self.base = self.base.track_focus(handle);
        self
    }

    /// 受控：开不开由调用方定，用户点开、关掉经 [`Popover::on_open_change`] 交回，调用方存好
    /// 再画。不调就由它自己开关。
    pub fn open(mut self, open: bool) -> Self {
        self.base = self.base.open(open);
        self
    }

    /// 用户开、关它时跑（点触发点、Esc、点外面），参数是开没开。
    pub fn on_open_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.base = self.base.on_open_change(handler);
        self
    }

    /// 触发点外框的样式，比如在横排里让它能缩（`min_w_0`）。
    pub fn trigger_style(mut self, style: StyleRefinement) -> Self {
        self.base = self.base.refine_style(&style);
        self
    }

    /// 卡片里的内容，开着时每帧搭一次。要在里面关掉自己时，拿 `cx.entity()` 存起来，
    /// 之后 `update` 调 [`PopoverState::dismiss`]。
    pub fn content<F, E>(mut self, content: F) -> Self
    where
        E: IntoElement,
        F: FnOnce(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> E + 'static,
    {
        self.content = Some(Box::new(move |state, window, cx| {
            content(state, window, cx).into_any_element()
        }));
        self
    }
}

impl Styled for Popover {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Popover {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let base = self.base.anchor(self.anchor);
        let Some(content) = self.content else {
            return base;
        };
        let style = self.style;
        let id = self.id;
        let from = EnterFrom::anchored(self.anchor);
        base.content(move |state, window, cx| {
            let enter = overlay_enter((id, "enter"), from, window, cx);
            enter
                .apply(surface().rounded(px(RADIUS.md)).p(px(PADDING)))
                .refine_style(&style)
                .child(content(state, window, cx))
        })
    }
}
