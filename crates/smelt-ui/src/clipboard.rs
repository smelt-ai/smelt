//! 文本复制按钮。状态按元素路径和 id 保存；同一层的按钮必须使用不同的稳定 id。

use rust_i18n::t;
use std::time::{Duration, Instant};

use gpui::{App, ClipboardItem, ElementId, IntoElement, RenderOnce, SharedString, Window};

use crate::{IconName, button::Button, icon_action::IconAction, motion};

const ACK_DURATION: Duration = Duration::from_millis(motion::COPY_ACK_MS);
const ACK_HOLD: Duration = Duration::from_millis(motion::COPY_ACK_MS - 400);

#[derive(Default)]
struct CopyState {
    started: Option<Instant>,
    generation: u64,
}

impl CopyState {
    fn begin(&mut self, now: Instant) -> u64 {
        self.started = Some(now);
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn finish(&mut self, generation: u64) -> bool {
        if self.generation != generation {
            return false;
        }
        self.started = None;
        true
    }
}

/// 复制固定文本的图标按钮。反馈期间仍可再次复制，并从头显示对勾。
#[derive(IntoElement)]
pub struct Clipboard {
    id: ElementId,
    value: SharedString,
    tooltip: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    disabled: bool,
}

impl Clipboard {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: SharedString::default(),
            tooltip: None,
            accessibility_label: None,
            disabled: false,
        }
    }

    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = value.into();
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl IconAction for Clipboard {
    fn icon_action(self, name: impl Into<SharedString>) -> Self {
        let name = name.into();
        self.accessibility_label(name.clone()).tooltip(name)
    }
}

/// 返回对勾透明度；减弱动态效果或窗口不在前台时保持静态对勾。
fn feedback(started: Option<Instant>, now: Instant, animate: bool) -> Option<f32> {
    let elapsed = now.saturating_duration_since(started?);
    if elapsed >= ACK_DURATION {
        None
    } else if animate {
        Some(motion::copy_ack_opacity(elapsed))
    } else {
        Some(1.0)
    }
}

impl RenderOnce for Clipboard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| CopyState::default());
        let now = cx.background_executor().now();
        let animate = !cx.reduce_motion()
            && motion::ambient_motion_enabled(
                motion::ambient_application_active(cx),
                window.is_window_active(),
                true,
            );
        let started = state.read(cx).started;
        let opacity = feedback(started, now, animate);
        if animate
            && started.is_some_and(|started| {
                let elapsed = now.saturating_duration_since(started);
                elapsed >= ACK_HOLD && elapsed < ACK_DURATION
            })
        {
            window.request_animation_frame();
        }

        let mut button = Button::new(self.id)
            .quiet()
            .small()
            .icon(if opacity.is_some() {
                IconName::Check
            } else {
                IconName::Copy
            })
            .accessibility_label(
                self.accessibility_label
                    .unwrap_or_else(|| t!("action.copy").into()),
            )
            .disabled(self.disabled)
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(self.value.to_string()));
                let started = cx.background_executor().now();
                let generation = state.update(cx, |state, cx| {
                    let generation = state.begin(started);
                    cx.notify();
                    generation
                });

                // 只在保持期结束和反馈结束时唤醒；旧计时器不能清掉较新的反馈。
                let state = state.downgrade();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(ACK_HOLD).await;
                    let _ = state.update(cx, |state, cx| {
                        if state.generation == generation {
                            cx.notify();
                        }
                    });
                    cx.background_executor()
                        .timer(ACK_DURATION - ACK_HOLD)
                        .await;
                    let _ = state.update(cx, |state, cx| {
                        if state.finish(generation) {
                            cx.notify();
                        }
                    });
                })
                .detach();
            });
        if let Some(opacity) = opacity {
            button = button.icon_opacity(opacity);
        }
        if let Some(tooltip) = self.tooltip {
            button = button.tooltip(tooltip);
        }
        button
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Context, InteractiveElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ParentElement,
        Render, StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, div, point,
        px,
    };
    use std::{cell::Cell, rc::Rc};

    struct Harness {
        value: &'static str,
        disabled: bool,
        parent_clicks: Rc<Cell<usize>>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let parent_clicks = self.parent_clicks.clone();
            div()
                .id("parent")
                .tab_group()
                .size(px(100.))
                .on_click(move |_, _, _| parent_clicks.set(parent_clicks.get() + 1))
                .child(
                    Clipboard::new("copy")
                        .value(self.value)
                        .icon_action("复制全部")
                        .disabled(self.disabled),
                )
        }
    }

    fn press_key(cx: &mut VisualTestContext, key: &str) {
        let keystroke =
            cx.update(|_, cx| Keystroke::parse_with_os(key, cx.platform_info().os).unwrap());
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    // 在同一元素路径里先调用真正的 Clipboard::render，再取它刚创建的状态。
    // 初始化闭包必须不可达，避免测试误用另一个同名状态。
    #[derive(IntoElement)]
    struct ObservedClipboard {
        state: Rc<std::cell::RefCell<Option<gpui::Entity<CopyState>>>>,
    }

    impl RenderOnce for ObservedClipboard {
        fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
            let button = Clipboard::new("copy")
                .value("测试复制")
                .render(window, cx)
                .into_any_element();
            let state = window.use_keyed_state("copy", cx, |_, _| -> CopyState {
                panic!("Clipboard 应已创建此路径下的状态")
            });
            let mut observed = self.state.borrow_mut();
            if let Some(previous) = observed.as_ref() {
                assert_eq!(
                    previous.entity_id(),
                    state.entity_id(),
                    "重绘不能换掉复制状态"
                );
            }
            *observed = Some(state);
            button
        }
    }

    struct FeedbackHarness {
        state: Rc<std::cell::RefCell<Option<gpui::Entity<CopyState>>>>,
    }

    impl Render for FeedbackHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size(px(100.)).child(ObservedClipboard {
                state: self.state.clone(),
            })
        }
    }

    fn feedback_harness(
        cx: &mut TestAppContext,
    ) -> (
        &mut VisualTestContext,
        gpui::Entity<CopyState>,
        Rc<Cell<usize>>,
        gpui::Subscription,
    ) {
        cx.update(gpui_base::init);
        let state = Rc::new(std::cell::RefCell::new(None));
        let (_, cx) = cx.add_window_view({
            let state = state.clone();
            move |_, _| FeedbackHarness { state }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let state = state.borrow().as_ref().expect("复制按钮应已绘制").clone();
        let notifications = Rc::new(Cell::new(0));
        let subscription = cx.update(|_, cx| {
            let notifications = notifications.clone();
            cx.observe(&state, move |_, _| {
                notifications.set(notifications.get() + 1);
            })
        });
        (cx, state, notifications, subscription)
    }

    fn advance_feedback_clock(cx: &mut VisualTestContext, duration: Duration) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn click_wakes_at_hold_and_clears_feedback_at_expiry(cx: &mut TestAppContext) {
        let (cx, state, notifications, _subscription) = feedback_harness(cx);
        assert_eq!(cx.update(|_, cx| state.read(cx).started), None);
        let start = cx.executor().now();
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("测试复制".into())
        );
        assert_eq!(cx.update(|_, cx| state.read(cx).started), Some(start));
        assert_eq!(notifications.get(), 1);

        advance_feedback_clock(cx, ACK_HOLD - Duration::from_millis(1));
        assert_eq!(notifications.get(), 1, "保持期内不应提前唤醒");
        advance_feedback_clock(cx, Duration::from_millis(1));
        assert_eq!(notifications.get(), 2, "保持期结束时应通知重绘");
        assert_eq!(cx.update(|_, cx| state.read(cx).started), Some(start));

        advance_feedback_clock(cx, ACK_DURATION - ACK_HOLD - Duration::from_millis(1));
        assert_eq!(notifications.get(), 2);
        assert_eq!(cx.update(|_, cx| state.read(cx).started), Some(start));
        advance_feedback_clock(cx, Duration::from_millis(1));
        assert_eq!(notifications.get(), 3, "到期时应通知重绘");
        assert_eq!(cx.update(|_, cx| state.read(cx).started), None);
    }

    #[gpui::test]
    fn earlier_click_timer_cannot_clear_later_click_feedback(cx: &mut TestAppContext) {
        let (cx, state, notifications, _subscription) = feedback_harness(cx);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        advance_feedback_clock(cx, Duration::from_millis(500));
        // 用反馈状态重绘后再点，确保第二次点击仍走真实按钮事件。
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let second_start = cx.executor().now();
        cx.write_to_clipboard(ClipboardItem::new_string("其他".into()));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("测试复制".into())
        );
        assert_eq!(
            cx.update(|_, cx| state.read(cx).started),
            Some(second_start)
        );
        assert_eq!(notifications.get(), 2);

        advance_feedback_clock(cx, ACK_HOLD - Duration::from_millis(500));
        assert_eq!(notifications.get(), 2, "旧保持期计时器不能通知新反馈");
        advance_feedback_clock(cx, ACK_DURATION - ACK_HOLD);
        assert_eq!(notifications.get(), 2, "旧到期计时器不能通知新反馈");
        assert_eq!(
            cx.update(|_, cx| state.read(cx).started),
            Some(second_start)
        );

        advance_feedback_clock(cx, Duration::from_millis(500) - (ACK_DURATION - ACK_HOLD));
        assert_eq!(notifications.get(), 3, "新保持期结束时应唤醒");
        assert_eq!(
            cx.update(|_, cx| state.read(cx).started),
            Some(second_start)
        );
        advance_feedback_clock(cx, ACK_DURATION - ACK_HOLD);
        assert_eq!(notifications.get(), 4, "新反馈到期时应复原");
        assert_eq!(cx.update(|_, cx| state.read(cx).started), None);
    }

    #[test]
    fn icon_action_sets_tooltip_and_accessible_name() {
        let copy = Clipboard::new("copy").icon_action("复制代码");
        assert_eq!(copy.tooltip.as_deref(), Some("复制代码"));
        assert_eq!(copy.accessibility_label.as_deref(), Some("复制代码"));
        assert_eq!(copy.value.as_ref(), "");
        assert!(!copy.disabled);
    }

    #[test]
    fn feedback_holds_fades_and_expires_even_without_motion() {
        let start = Instant::now();
        assert_eq!(feedback(None, start, true), None);
        assert_eq!(feedback(Some(start), start + ACK_HOLD, true), Some(1.0));
        let mid = feedback(Some(start), start + Duration::from_millis(1000), true).unwrap();
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(mid, motion::copy_ack_opacity(Duration::from_millis(1000)));
        assert_eq!(
            feedback(Some(start), start + Duration::from_millis(1000), false),
            Some(1.0)
        );
        assert_eq!(feedback(Some(start), start + ACK_DURATION, true), None);
        assert_eq!(feedback(Some(start), start + ACK_DURATION, false), None);
    }

    #[test]
    fn old_timer_cannot_finish_a_new_copy() {
        let mut state = CopyState::default();
        let start = Instant::now();
        let old = state.begin(start);
        let new = state.begin(start + Duration::from_millis(500));
        assert!(!state.finish(old));
        assert_eq!(state.started, Some(start + Duration::from_millis(500)));
        assert!(state.finish(new));
        assert_eq!(state.started, None);
    }

    #[gpui::test]
    fn pointer_and_keyboard_copy_again_during_feedback(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| Harness {
                value: "第一段",
                disabled: false,
                parent_clicks: clicks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("第一段".into())
        );
        assert_eq!(clicks.get(), 0);

        view.update(cx, |view, cx| {
            view.value = "第二段";
            cx.notify();
        });
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        press_key(cx, "enter");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("第二段".into())
        );
        cx.write_to_clipboard(ClipboardItem::new_string("其他".into()));
        press_key(cx, "space");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("第二段".into())
        );
    }

    #[gpui::test]
    fn disabled_focused_button_cannot_copy(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let (view, cx) = cx.add_window_view(|_, _| Harness {
            value: "不应复制",
            disabled: false,
            parent_clicks: Rc::new(Cell::new(0)),
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
        });
        // 先触发反馈，再禁用已有焦点的按钮。
        press_key(cx, "enter");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("不应复制".into())
        );
        view.update(cx, |view, cx| {
            view.disabled = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.write_to_clipboard(ClipboardItem::new_string("原值".into()));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        press_key(cx, "enter");
        press_key(cx, "space");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原值".into())
        );
    }
}
