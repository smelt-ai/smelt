//! 分段控件：在几项之间切换视图或取值。规则见 `docs/ui-design-language.md` 第 3、4、6、7 节。
//!
//! 一整条胶囊，底 `soft`；选中的段 `surface` 抬起，和外框同一个轮廓。选中只靠底色和
//! 字色表示，不加描边。它不是一组按钮：单选时每段是一个单选项，多选（如星期）时每段
//! 是一个开关，焦点、键盘、无障碍都交给 gpui-base 的无样式原件，样子在这里画。
//!
//! 单选时抬起的那块是独立的一层，换选中时从旧段滑到新段，宽度跟着变；多选各段各自
//! 亮灭，没有谁滑向谁，不做动效。

use std::rc::Rc;

use gpui::{
    App, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _,
    px, rgb,
};
use gpui_base::{GlobalState, Radio, RadioGroup, Toggle, ToggleGroup};
use gpui_component::StyledExt as _;
use gpui_component::tooltip::Tooltip;

use crate::button::{DISABLED_OPACITY, focus_ring};
use crate::{motion, ui_theme};

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

/// 各段相对第一段的左边和宽度，上一帧量出来的；选中块按它定位。
#[derive(Default)]
struct SegmentLayout(Vec<(f32, f32)>);

/// 选中块在 `pos` 处的左边和宽度。`pos` 是段下标，滑动途中落在两段之间，按相邻两段
/// 线性插值；跨过几段时依次经过中间的段，宽度一路跟着变。
fn indicator_at(layout: &[(f32, f32)], pos: f32) -> Option<(f32, f32)> {
    let last = layout.len().checked_sub(1)?;
    let pos = pos.clamp(0., last as f32);
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(last);
    let t = pos - lo as f32;
    let (x0, w0) = layout[lo];
    let (x1, w1) = layout[hi];
    Some((x0 + (x1 - x0) * t, w0 + (w1 - w0) * t))
}

/// 一段的样子：单选项和开关共用。`raised` 时这段自己画抬起的底：多选的选中段，
/// 以及单选还没量出各段位置的第一帧。
fn paint_segment<E>(
    segment: E,
    selected: bool,
    raised: bool,
    interactive: bool,
    label: SharedString,
) -> E
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
        .when(raised, |this| {
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Segmented {
            id,
            style,
            segments,
            selection,
            disabled,
            on_select,
        } = self;
        let count = segments.len();
        let interactive = !disabled && on_select.is_some();

        let selected = match &selection {
            Selection::Multiple(_) => {
                let segments = segments.into_iter().enumerate().map(|(ix, segment)| {
                    let is_selected = selection.is_selected(ix);
                    let Segment { label, tooltip } = segment;
                    let toggle = Toggle::new(ix)
                        .pressed(is_selected)
                        .disabled(disabled)
                        .accessibility_label(label.clone())
                        .when_some(on_select.clone(), |this, on_select| {
                            this.on_change(move |_, _, window, cx| on_select(ix, window, cx))
                        });
                    with_tooltip(
                        paint_segment(toggle, is_selected, is_selected, interactive, label),
                        tooltip,
                    )
                });
                return paint_track(ToggleGroup::new(id), disabled)
                    .children(segments)
                    .refine_style(&style)
                    .into_any_element();
            }
            Selection::Single(selected) => selected.filter(|ix| *ix < count),
        };

        let layout = window.use_keyed_state(
            ElementId::NamedChild(id.clone().into(), "layout".into()),
            cx,
            |_, _| SegmentLayout::default(),
        );
        // 只在有选中时取过渡值：从没选到选上，过渡状态是新建的，直接到位不滑。
        let indicator = selected.and_then(|ix| {
            let pos = gpui_base::motion::transition(
                ElementId::NamedChild(id.clone().into(), "indicator".into()),
                ix as f32,
                motion::snap_transition(),
                window,
                cx,
            );
            let measured = layout.read(cx);
            if measured.0.len() == count {
                indicator_at(&measured.0, pos)
            } else {
                None
            }
        });
        let segments = segments.into_iter().enumerate().map(|(ix, segment)| {
            let is_selected = selected == Some(ix);
            let Segment { label, tooltip } = segment;
            let radio = Radio::new(ix)
                .checked(is_selected)
                .disabled(disabled)
                .set_position(ix + 1, count)
                .accessibility_label(label.clone())
                .when_some(on_select.clone(), |this, on_select| {
                    this.on_change(move |_, _, window, cx| on_select(ix, window, cx))
                });
            let raised = is_selected && indicator.is_none();
            with_tooltip(
                paint_segment(radio, is_selected, raised, interactive, label),
                tooltip,
            )
        });
        // 各段放进一层普通容器，才能在布局后拿到它们的位置。位置相对第一段记，
        // 和窗口里的绝对位置、外层滚动都无关；变了就让视图下一帧按新位置画。
        let row = div()
            .flex()
            .items_center()
            .gap(px(SEGMENT_GAP))
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(first) = bounds.first() else {
                    return;
                };
                let next: Vec<(f32, f32)> = bounds
                    .iter()
                    .map(|b| {
                        (
                            f32::from(b.origin.x - first.origin.x),
                            f32::from(b.size.width),
                        )
                    })
                    .collect();
                layout.update(cx, |layout, cx| {
                    if layout.0 != next {
                        layout.0 = next;
                        cx.notify();
                    }
                });
            })
            .children(segments);
        paint_track(RadioGroup::new(id).axis(gpui::Axis::Horizontal), disabled)
            .relative()
            .when_some(indicator, |this, (left, width)| {
                this.child(
                    div()
                        .absolute()
                        .top(px(INSET))
                        .left(px(INSET + left))
                        .w(px(width))
                        .h(px(SEGMENT_HEIGHT))
                        .rounded_full()
                        .bg(rgb(ui_theme::surface()))
                        .shadow(ui_theme::lift_shadow()),
                )
            })
            .child(row)
            .refine_style(&style)
            .into_any_element()
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
    fn indicator_rests_on_a_segment_and_blends_between_neighbours() {
        let layout = [(0., 40.), (42., 60.), (104., 50.)];
        assert_eq!(indicator_at(&layout, 1.), Some((42., 60.)));
        assert_eq!(indicator_at(&layout, 0.5), Some((21., 50.)));
        assert_eq!(
            indicator_at(&layout, 2.),
            Some((104., 50.)),
            "最后一段不越界"
        );
        assert_eq!(
            indicator_at(&layout, -0.2),
            Some((0., 40.)),
            "缓动越过端点时钳住"
        );
        assert_eq!(indicator_at(&[], 0.), None);
    }

    #[test]
    fn selection_does_not_depend_on_builder_order() {
        let control = Segmented::new("s").selected(Some(1)).segments(["a", "b"]);
        assert!(control.selection.is_selected(1));
    }
}
