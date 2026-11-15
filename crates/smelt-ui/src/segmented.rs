//! 分段控件：在几项之间切换视图或取值。规则见 `docs/ui-design-language.md` 第 3、4、6、7 节。
//!
//! 一整条胶囊，底 `soft`；选中的段 `surface` 抬起，和外框同一个轮廓。选中只靠底色和
//! 字色表示，不加描边。它不是一组按钮：单选时每段是一个单选项，多选（如星期）时每段
//! 是一个开关，焦点、键盘、无障碍都交给 gpui-base 的无样式原件，样子在这里画。

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{GlobalState, Radio, RadioGroup, Toggle, ToggleGroup};
use gpui_component::StyledExt as _;
use gpui_component::tooltip::Tooltip;

use crate::button::{DISABLED_OPACITY, focus_ring};
use crate::ui_theme;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// 外框内边距：外框 32 高，段 26 高。
const INSET: f32 = 3.;
const SEGMENT_HEIGHT: f32 = 26.;
const SEGMENT_PADDING_X: f32 = 14.;
const SEGMENT_GAP: f32 = 2.;

/// 分段控件里的一段。
pub struct Segment {
    label: SharedString,
    tooltip: Option<SharedString>,
}

impl Segment {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tooltip: None,
        }
    }

    /// 标签说不全时，悬停补一句。
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

// 只有标签的段直接给字符串。不写成 `impl<T: Into<SharedString>>`：那会和标准库的
// `From<T> for T` 冲突。
impl From<&'static str> for Segment {
    fn from(label: &'static str) -> Self {
        Self::new(label)
    }
}

impl From<String> for Segment {
    fn from(label: String) -> Self {
        Self::new(label)
    }
}

impl From<SharedString> for Segment {
    fn from(label: SharedString) -> Self {
        Self::new(label)
    }
}

/// 分段控件。默认单选：`selected` 给当前那段，点别的段回调它的下标，点已选的段不回调。
/// [`Segmented::multiple`] 改成多选：每段各自开关，点哪段回调哪段。
#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    style: StyleRefinement,
    segments: Vec<Segment>,
    selection: Selection,
    disabled: bool,
    on_select: Option<SelectHandler>,
}

impl Segmented {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            segments: Vec::new(),
            selection: Selection::Single(None),
            disabled: false,
            on_select: None,
        }
    }

    pub fn segments(mut self, segments: impl IntoIterator<Item = impl Into<Segment>>) -> Self {
        self.segments.extend(segments.into_iter().map(Into::into));
        self
    }

    /// 单选：选中第 `ix` 段；`None` 是一段都没选。
    pub fn selected(mut self, ix: Option<usize>) -> Self {
        self.selection = Selection::Single(ix);
        self
    }

    /// 改成多选：按段的顺序给出每段选没选，少给的算没选。
    pub fn multiple(mut self, selected: impl IntoIterator<Item = bool>) -> Self {
        self.selection = Selection::Multiple(selected.into_iter().collect());
        self
    }

    /// 整条不可用：仍显示当前选择，点不动、也不进 Tab 顺序。
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

enum Selection {
    Single(Option<usize>),
    Multiple(Vec<bool>),
}

impl Selection {
    fn is_selected(&self, ix: usize) -> bool {
        match self {
            Selection::Single(selected) => *selected == Some(ix),
            Selection::Multiple(selected) => selected.get(ix).copied().unwrap_or(false),
        }
    }
}

impl Styled for Segmented {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// 一段的样子：单选项和开关共用。
fn paint_segment<E>(segment: E, selected: bool, interactive: bool, label: SharedString) -> E
where
    E: Styled + ParentElement + StatefulInteractiveElement + IntoElement,
{
    segment
        .flex()
        .items_center()
        .justify_center()
        .min_w_0()
        .h(px(SEGMENT_HEIGHT))
        .px(px(SEGMENT_PADDING_X))
        .rounded_full()
        .cursor_default()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(if selected {
            ui_theme::text()
        } else {
            ui_theme::muted()
        }))
        .when(selected, |this| {
            this.bg(rgb(ui_theme::surface()))
                .shadow(ui_theme::lift_shadow())
        })
        .when(interactive && !selected, |this| {
            this.hover(|style| style.text_color(rgb(ui_theme::text())))
        })
        .focus_visible(|style| style.shadow(focus_ring()))
        .when(interactive, |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                // 和按钮一样：点了不抢焦点，输入框里的光标留在原处。
                window.prevent_default();
                GlobalState::suppress_text_selection(cx);
            })
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        )
}

/// 段上的悬停说明。
fn with_tooltip<E: StatefulInteractiveElement + IntoElement>(
    segment: E,
    tooltip: Option<SharedString>,
) -> E {
    match tooltip {
        Some(text) => {
            segment.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
        }
        None => segment,
    }
}

/// 外框：胶囊、`soft` 底。
fn paint_track<E: Styled + IntoElement>(track: E, disabled: bool) -> E {
    track
        .flex()
        .items_center()
        .gap(px(SEGMENT_GAP))
        .p(px(INSET))
        .rounded_full()
        .bg(rgb(ui_theme::soft()))
        .when(disabled, |this| this.opacity(DISABLED_OPACITY))
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Segmented {
            id,
            style,
            segments,
            selection,
            disabled,
            on_select,
        } = self;
        let multiple = matches!(selection, Selection::Multiple(_));
        let count = segments.len();
        let interactive = !disabled && on_select.is_some();
        let segments = segments.into_iter().enumerate().map(|(ix, segment)| {
            let is_selected = selection.is_selected(ix);
            let on_select = on_select.clone();
            let Segment { label, tooltip } = segment;
            if multiple {
                let toggle = Toggle::new(ix)
                    .pressed(is_selected)
                    .disabled(disabled)
                    .accessibility_label(label.clone())
                    .when_some(on_select, |this, on_select| {
                        this.on_change(move |_, _, window, cx| on_select(ix, window, cx))
                    });
                with_tooltip(
                    paint_segment(toggle, is_selected, interactive, label),
                    tooltip,
                )
                .into_any_element()
            } else {
                let radio = Radio::new(ix)
                    .checked(is_selected)
                    .disabled(disabled)
                    .set_position(ix + 1, count)
                    .accessibility_label(label.clone())
                    .when_some(on_select, |this, on_select| {
                        this.on_change(move |_, _, window, cx| on_select(ix, window, cx))
                    });
                with_tooltip(
                    paint_segment(radio, is_selected, interactive, label),
                    tooltip,
                )
                .into_any_element()
            }
        });
        let segments: Vec<AnyElement> = segments.collect();
        if multiple {
            paint_track(ToggleGroup::new(id), disabled)
                .children(segments)
                .refine_style(&style)
                .into_any_element()
        } else {
            paint_track(RadioGroup::new(id).axis(gpui::Axis::Horizontal), disabled)
                .children(segments)
                .refine_style(&style)
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_selection_marks_only_the_given_segment() {
        let one = Selection::Single(Some(1));
        assert!(!one.is_selected(0));
        assert!(one.is_selected(1));
        assert!(!one.is_selected(2));
        assert!(!Selection::Single(None).is_selected(0));
    }

    #[test]
    fn multiple_selection_reads_each_segment_and_defaults_to_off() {
        let days = Selection::Multiple(vec![true, false, true]);
        assert!(days.is_selected(0));
        assert!(!days.is_selected(1));
        assert!(days.is_selected(2));
        assert!(!days.is_selected(3), "少给的算没选");
    }

    #[test]
    fn selection_does_not_depend_on_builder_order() {
        let control = Segmented::new("s").selected(Some(1)).segments(["a", "b"]);
        assert!(control.selection.is_selected(1));
    }
}
