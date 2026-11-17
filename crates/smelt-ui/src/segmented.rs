//! 分段控件：28 高、6 圆角的槽，每段左右 12，字 13/440；选中段是一块带 0.5 细圈的
//! 滑块，按控件档弹簧滑到位。单选和多选分别使用 gpui-base 的 Radio 和 Toggle，交互与
//! 无障碍由原件处理。多选没有滑块，每段独立使用同一套选中和交互样式。

use std::rc::Rc;

#[cfg(test)]
use gpui::InteractiveElement as _;
use gpui::{
    App, BoxShadow, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{GlobalState, Radio, RadioGroup, StyledExt as _, Toggle, ToggleGroup};

use crate::kit::{MOTION, RADIUS};
use crate::{button::focus_ring, tooltip::Tooltip, ui_theme};

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

const SEGMENT_HEIGHT: f32 = 28.;
const SEGMENT_RADIUS: f32 = RADIUS.md;
const SEGMENT_PADDING_X: f32 = 12.;
const LABEL_SIZE: f32 = 13.;
const LABEL_WEIGHT: FontWeight = FontWeight(440.);
const DISABLED_OPACITY: f32 = 0.45;

fn outline(color: gpui::Rgba) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), color.into()).spread_radius(px(0.5))]
}

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

/// 滑动途中按相邻两段的位置和宽度插值。
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

/// 多选和单选首帧由段自己画底；单选量出位置后由独立层滑动。
fn paint_segment<E>(
    segment: E,
    selected: bool,
    raised: bool,
    interactive: bool,
    disabled: bool,
    #[cfg(test)] id: &ElementId,
    #[cfg(test)] ix: usize,
    label: SharedString,
) -> E
where
    E: Styled + ParentElement + StatefulInteractiveElement + IntoElement,
{
    #[cfg(test)]
    let segment = segment.debug_selector(|| format!("segmented-item-{id:?}-{ix}"));
    segment
        .flex()
        .items_center()
        .justify_center()
        .min_w_0()
        .h(px(SEGMENT_HEIGHT))
        .rounded(px(SEGMENT_RADIUS))
        .cursor_default()
        .text_size(px(LABEL_SIZE))
        .line_height(px(LABEL_SIZE))
        .font_weight(LABEL_WEIGHT)
        .text_color(rgb(if selected {
            ui_theme::text()
        } else {
            ui_theme::muted()
        }))
        .when(raised, |this| {
            this.bg(rgb(ui_theme::bg()))
                .shadow(outline(ui_theme::hair_strong()))
        })
        .when(interactive && !selected, |this| {
            this.hover(|style| {
                style
                    .bg(rgb(ui_theme::soft_hover()))
                    .text_color(rgb(ui_theme::text()))
            })
            .active(|style| style.bg(rgb(ui_theme::soft_press())))
        })
        .focus_visible(|style| style.shadow(focus_ring()))
        .when(disabled, |this| this.opacity(DISABLED_OPACITY))
        .when(interactive, |this| {
            this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                GlobalState::suppress_text_selection(cx);
            })
        })
        .child(
            div().min_w_0().overflow_hidden().child(
                div()
                    .px(px(SEGMENT_PADDING_X))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label),
            ),
        )
}

fn paint_indicator(left: f32, width: f32, disabled: bool) -> gpui::Div {
    div()
        .absolute()
        .top(px(0.))
        .left(px(left))
        .w(px(width))
        .h(px(SEGMENT_HEIGHT))
        .rounded(px(SEGMENT_RADIUS))
        .bg(rgb(ui_theme::bg()))
        .shadow(outline(ui_theme::hair_strong()))
        .when(disabled, |this| this.opacity(DISABLED_OPACITY))
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

/// 外沿不占布局空间，选中块和外框一样高。
fn paint_track<E: Styled + IntoElement>(track: E, count: usize) -> E {
    // 1fr 列在自然宽度下按最宽内容等分，在受限宽度下可收缩到零。
    // 超过 GPUI 的列数上限时退回普通 flex，避免截断段数。
    track
        .when_some(u16::try_from(count.max(1)).ok(), |this, count| {
            this.grid().grid_cols(count)
        })
        .when(count > u16::MAX as usize, |this| this.flex())
        .items_center()
        .h(px(SEGMENT_HEIGHT))
        .rounded(px(SEGMENT_RADIUS))
        .bg(rgb(ui_theme::soft()))
        .shadow(outline(ui_theme::hair()))
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
                let segments = segments
                    .into_iter()
                    .enumerate()
                    .map(|(ix, segment)| {
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
                            paint_segment(
                                toggle,
                                is_selected,
                                is_selected,
                                interactive,
                                disabled,
                                #[cfg(test)]
                                &id,
                                #[cfg(test)]
                                ix,
                                label,
                            ),
                            tooltip,
                        )
                    })
                    .collect::<Vec<_>>();
                return paint_track(ToggleGroup::new(id), count)
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
        let indicator = selected.and_then(|ix| {
            // 控件档弹簧：首帧直接到位，减弱动态效果时直接到位。
            let pos = gpui_base::motion::spring(
                ElementId::NamedChild(id.clone().into(), "indicator".into()),
                ix as f32,
                MOTION.control,
                window,
                cx,
            );
            let measured = layout.read(cx);
            (measured.0.len() == count)
                .then(|| indicator_at(&measured.0, pos))
                .flatten()
        });
        let segments = segments
            .into_iter()
            .enumerate()
            .map(|(ix, segment)| {
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
                with_tooltip(
                    paint_segment(
                        radio,
                        is_selected,
                        is_selected && indicator.is_none(),
                        interactive,
                        disabled,
                        #[cfg(test)]
                        &id,
                        #[cfg(test)]
                        ix,
                        label,
                    ),
                    tooltip,
                )
            })
            .collect::<Vec<_>>();
        // 普通子容器负责等宽与测量；不把绝对定位的滑块计入测量。
        let row = div()
            .min_w_0()
            .flex_grow(1.)
            .flex_shrink(1.)
            .when_some(u16::try_from(count.max(1)).ok(), |this, count| {
                this.grid().grid_cols(count)
            })
            .when(count > u16::MAX as usize, |this| this.flex())
            .items_center()
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
        let track = RadioGroup::new(id.clone())
            .axis(gpui::Axis::Horizontal)
            .flex()
            .items_center()
            .h(px(SEGMENT_HEIGHT))
            .rounded(px(SEGMENT_RADIUS))
            .bg(rgb(ui_theme::soft()))
            .shadow(outline(ui_theme::hair()));
        track
            .relative()
            .when_some(indicator, |this, (left, width)| {
                let indicator = paint_indicator(left, width, disabled);
                #[cfg(test)]
                let indicator = indicator.debug_selector(|| format!("segmented-indicator-{id:?}"));
                this.child(indicator)
            })
            .child(row)
            .refine_style(&style)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(debug_assertions)]
    use gpui::{
        Context, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext,
    };
    #[cfg(debug_assertions)]
    use std::cell::RefCell;

    #[cfg(debug_assertions)]
    const ITEM_SELECTORS: [&str; 7] = [
        "segmented-item-Name(\"test-segments\")-0",
        "segmented-item-Name(\"test-segments\")-1",
        "segmented-item-Name(\"test-segments\")-2",
        "segmented-item-Name(\"test-segments\")-3",
        "segmented-item-Name(\"test-segments\")-4",
        "segmented-item-Name(\"test-segments\")-5",
        "segmented-item-Name(\"test-segments\")-6",
    ];

    #[cfg(debug_assertions)]
    struct SegmentedHarness {
        labels: Vec<String>,
        multiple: bool,
        disabled: bool,
        width: Option<f32>,
        selected: Option<usize>,
        font: Option<&'static str>,
        picks: Rc<RefCell<Vec<usize>>>,
    }

    #[cfg(debug_assertions)]
    impl Render for SegmentedHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let picks = self.picks.clone();
            let control = Segmented::new("test-segments")
                .segments(self.labels.clone())
                .disabled(self.disabled)
                .on_select(move |ix, _, _| picks.borrow_mut().push(ix));
            let control = if self.multiple {
                control.multiple([true, false, true])
            } else {
                control.selected(self.selected)
            };
            div().tab_group().flex().child(
                control
                    .flex_none()
                    .when_some(self.font, |control, font| control.font_family(font))
                    .when_some(self.width, |control, width| control.w(px(width))),
            )
        }
    }

    #[cfg(debug_assertions)]
    fn harness(
        cx: &mut TestAppContext,
        multiple: bool,
        disabled: bool,
        width: Option<f32>,
    ) -> (
        Entity<SegmentedHarness>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<Vec<usize>>>,
    ) {
        harness_with_selection(cx, multiple, disabled, width, Some(0))
    }

    #[cfg(debug_assertions)]
    fn harness_with_selection(
        cx: &mut TestAppContext,
        multiple: bool,
        disabled: bool,
        width: Option<f32>,
        selected: Option<usize>,
    ) -> (
        Entity<SegmentedHarness>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<Vec<usize>>>,
    ) {
        cx.update(gpui_base::init);
        let picks = Rc::new(RefCell::new(Vec::new()));
        let (view, cx) = cx.add_window_view({
            let picks = picks.clone();
            move |_, _| SegmentedHarness {
                labels: ["一", "二", "三", "四", "五", "六", "日"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                multiple,
                disabled,
                width,
                selected,
                font: None,
                picks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (view, cx, picks)
    }

    #[cfg(debug_assertions)]
    fn activate_with_keyboard(cx: &mut gpui::VisualTestContext) {
        for key in ["enter", "space"] {
            let keystroke =
                cx.update(|_, cx| Keystroke::parse_with_os(key, cx.platform_info().os).unwrap());
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
        }
    }

    // gpui-pre 的 debug_selector 在 release 依赖中是 no-op。
    #[cfg(debug_assertions)]
    #[gpui::test]
    fn weekday_multi_select_fits_and_keeps_every_segment_independent(cx: &mut TestAppContext) {
        let (_, cx, picks) = harness(cx, true, false, Some(350.));
        let bounds: Vec<_> = ITEM_SELECTORS
            .iter()
            .map(|selector| cx.debug_bounds(*selector).unwrap())
            .collect();
        for (ix, item) in bounds.iter().enumerate() {
            assert!((f32::from(item.size.width) - 50.).abs() < 0.1);
            assert_eq!(item.size.height, px(28.));
            if ix > 0 {
                assert!((f32::from(item.origin.x - bounds[ix - 1].origin.x) - 50.).abs() < 0.1);
            }
        }
        // 已选和未选的星期都能独立切换；短于七项的选择数组默认为未选。
        for ix in [0, 1, 6] {
            cx.simulate_click(bounds[ix].center(), Modifiers::default());
        }
        assert_eq!(picks.borrow().as_slice(), &[0, 1, 6]);
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        activate_with_keyboard(cx);
        assert_eq!(picks.borrow().as_slice(), &[0, 1, 6, 0, 0]);
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn disabled_weekdays_do_not_activate(cx: &mut TestAppContext) {
        let (_, cx, picks) = harness(cx, true, true, Some(350.));
        for selector in ITEM_SELECTORS {
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), Modifiers::default());
        }
        activate_with_keyboard(cx);
        assert!(picks.borrow().is_empty());
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn single_select_and_disabled_keep_base_activation_rules(cx: &mut TestAppContext) {
        for disabled in [false, true] {
            let (_, cx, picks) = harness(cx, false, disabled, Some(350.));
            for ix in [0, 1] {
                let bounds = cx.debug_bounds(ITEM_SELECTORS[ix]).unwrap();
                cx.simulate_click(bounds.center(), Modifiers::default());
            }
            assert_eq!(
                picks.borrow().as_slice(),
                if disabled { &[][..] } else { &[1][..] }
            );
            if !disabled {
                cx.update(|window, cx| {
                    window.focus_next(cx);
                    assert!(window.focused(cx).is_some());
                    window.draw(cx).clear(cx);
                });
                // 焦点在第一项时已选项不会重复回调。
                activate_with_keyboard(cx);
                assert_eq!(picks.borrow().as_slice(), &[1]);
            }
        }
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn natural_fit_remeasures_dynamic_labels(cx: &mut TestAppContext) {
        let (view, cx, _) = harness(cx, false, false, None);
        let before = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap().size.width;
        view.update(cx, |view, cx| {
            view.labels[1] = "Much longer label 日本語".into();
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let first = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap();
        let second = cx.debug_bounds(ITEM_SELECTORS[1]).unwrap();
        assert!(first.size.width > before);
        assert_eq!(first.size.width, second.size.width);
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn dynamic_multiline_labels_lay_out_without_panicking(cx: &mut TestAppContext) {
        let (view, cx, _) = harness(cx, false, false, None);
        view.update(cx, |view, cx| {
            view.labels[1] = "first line\nsecond line".into();
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds(ITEM_SELECTORS[1]).is_some());
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn narrow_controls_keep_all_segments_inside_the_track(cx: &mut TestAppContext) {
        for multiple in [false, true] {
            let (_, cx, _) = harness(cx, multiple, false, Some(70.));
            let first = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap();
            let last = cx.debug_bounds(ITEM_SELECTORS[6]).unwrap();
            assert!(last.right() <= first.left() + px(70.));
            for selector in ITEM_SELECTORS {
                let item = cx.debug_bounds(selector).unwrap();
                assert!(item.size.width <= px(10.1));
            }
        }
    }

    // 这两个字体是 macOS 系统字体；其它平台不能假定它们已安装。
    #[cfg(all(debug_assertions, target_os = "macos"))]
    #[test]
    fn control_font_changes_natural_width() {
        let mut cx = TestAppContext::build_with_text_system(
            gpui::TestDispatcher::new(0),
            None,
            std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
        );
        let (view, cx, _) = harness(&mut cx, false, false, None);
        view.update(cx, |view, cx| {
            view.labels = vec!["iiiiiiiiiiiiiiiiiiii".into(), "i".into()];
            view.font = Some("Menlo");
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let mono = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap().size.width;
        view.update(cx, |view, cx| {
            view.font = Some("Times New Roman");
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let proportional = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap().size.width;
        assert!(mono > proportional);
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn removing_a_hovered_segment_and_reusing_its_index_still_works(cx: &mut TestAppContext) {
        for multiple in [false, true] {
            let (view, cx, picks) = harness(cx, multiple, false, Some(350.));
            let item = cx.debug_bounds(ITEM_SELECTORS[6]).unwrap();
            cx.simulate_mouse_move(item.center(), None, Modifiers::default());
            view.update(cx, |view, cx| {
                view.labels.pop();
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            // 原段不在时移开指针，再用相同下标加入新的段。
            cx.simulate_mouse_move(gpui::point(px(500.), px(500.)), None, Modifiers::default());
            view.update(cx, |view, cx| {
                view.labels.push("new".into());
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let replacement = cx.debug_bounds(ITEM_SELECTORS[6]).unwrap();
            cx.simulate_click(replacement.center(), Modifiers::default());
            assert_eq!(picks.borrow().as_slice(), &[6]);
        }
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn single_indicator_follows_layout_and_animates_at_natural_and_narrow_widths(
        cx: &mut TestAppContext,
    ) {
        use std::time::Duration;

        const INDICATOR: &str = "segmented-indicator-Name(\"test-segments\")";
        for width in [None, Some(70.)] {
            for disabled in [false, true] {
                let (view, cx, _) = harness_with_selection(cx, false, disabled, width, Some(2));
                // 量完后的下一帧应在初始选中段，而非从原点滑入。
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let track_start = cx.debug_bounds(ITEM_SELECTORS[0]).unwrap();
                let first = cx.debug_bounds(ITEM_SELECTORS[2]).unwrap();
                let last = cx.debug_bounds(ITEM_SELECTORS[6]).unwrap();
                let initial = cx.debug_bounds(INDICATOR).unwrap();
                assert!((f32::from(initial.left() - first.left())).abs() < 0.1);
                assert!((f32::from(initial.size.width - first.size.width)).abs() < 0.1);
                assert!(first.size.width > px(0.));
                if let Some(width) = width {
                    assert!(last.right() <= track_start.left() + px(width + 0.1));
                }

                view.update(cx, |view, cx| {
                    view.selected = Some(6);
                    cx.notify();
                });
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let start = cx.debug_bounds(INDICATOR).unwrap();
                assert!((f32::from(start.left() - first.left())).abs() < 0.1);
                cx.executor().advance_clock(Duration::from_millis(50));
                cx.update(|window, cx| assert!(window.simulate_next_frame(cx) > 0));
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let middle = cx.debug_bounds(INDICATOR).unwrap();
                assert!(middle.left() > first.left());
                assert!(middle.left() < last.left());
                assert!((f32::from(middle.size.width - first.size.width)).abs() < 0.1);
                cx.executor().advance_clock(Duration::from_secs(1));
                cx.update(|window, cx| assert!(window.simulate_next_frame(cx) > 0));
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let end = cx.debug_bounds(INDICATOR).unwrap();
                assert!((f32::from(end.left() - last.left())).abs() < 0.1);
                assert!((f32::from(end.size.width - last.size.width)).abs() < 0.1);
            }
        }
    }

    #[test]
    fn indicator_interpolates_and_clamps() {
        let layout = [(0., 40.), (42., 60.), (104., 50.)];
        assert_eq!(indicator_at(&layout, 0.5), Some((21., 50.)));
        assert_eq!(indicator_at(&layout, 2.), Some((104., 50.)));
        assert_eq!(indicator_at(&layout, -0.2), Some((0., 40.)));
        assert_eq!(indicator_at(&[], 0.), None);
    }

    #[test]
    fn disabled_segment_opacity_includes_the_selected_surface() {
        let id = ElementId::from("test");
        let mut indicator = paint_indicator(0., 40., true);
        assert_eq!(indicator.style().opacity, Some(DISABLED_OPACITY));
        assert!(indicator.style().box_shadow.is_some());
        for selected in [false, true] {
            let mut radio = paint_segment(
                Radio::new(0).checked(selected).disabled(true),
                selected,
                selected,
                false,
                true,
                &id,
                0,
                "label".into(),
            );
            assert_eq!(radio.style().opacity, Some(DISABLED_OPACITY));
            assert_eq!(radio.style().box_shadow.is_some(), selected);
            // 量出位置后，单选项本身不再画底，避免与滑块叠加。
            let mut radio_without_surface = paint_segment(
                Radio::new(0).checked(selected).disabled(true),
                selected,
                false,
                false,
                true,
                &id,
                0,
                "label".into(),
            );
            assert!(radio_without_surface.style().box_shadow.is_none());
            let mut toggle = paint_segment(
                Toggle::new(0).pressed(selected).disabled(true),
                selected,
                selected,
                false,
                true,
                &id,
                0,
                "label".into(),
            );
            assert_eq!(toggle.style().opacity, Some(DISABLED_OPACITY));
            assert_eq!(toggle.style().box_shadow.is_some(), selected);
        }
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn selectors_distinguish_controls_in_the_same_window(cx: &mut TestAppContext) {
        struct Pair;
        impl Render for Pair {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .flex()
                    .child(Segmented::new("first").segments(["A"]))
                    .child(Segmented::new("second").segments(["B"]))
            }
        }
        cx.update(gpui_base::init);
        let (_, cx) = cx.add_window_view(|_, _| Pair);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let first = cx.debug_bounds("segmented-item-Name(\"first\")-0").unwrap();
        let second = cx
            .debug_bounds("segmented-item-Name(\"second\")-0")
            .unwrap();
        assert!(first.right() <= second.left());
    }

    #[test]
    fn segment_metrics_and_real_call_site_builders() {
        assert_eq!(
            (SEGMENT_HEIGHT, SEGMENT_RADIUS, SEGMENT_PADDING_X),
            (28., 6., 12.)
        );
        assert_eq!(DISABLED_OPACITY, 0.45);
        let control = Segmented::new("memory-filters")
            .segments([
                Segment::new("全部").tooltip("显示全部"),
                Segment::new("记忆"),
            ])
            .selected(Some(1))
            .flex_none();
        assert_eq!(control.segments[0].tooltip.as_deref(), Some("显示全部"));
        assert!(control.selection.is_selected(1));
    }

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
