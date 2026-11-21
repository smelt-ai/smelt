//! 显示器事件驱动的像素节拍。只通知实际可见的动作；有限动作结束后不再申请帧。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{App, Bounds, Pixels, WeakEntity, Window};
use smelt_plugin_api::ui::motion::{ambient_application_active, ambient_motion_enabled};

const FPS: f64 = 12.;

pub(crate) fn tick(epoch: Instant, now: Instant) -> u64 {
    (now.saturating_duration_since(epoch).as_secs_f64() * FPS) as u64
}

pub(crate) fn enabled(visible: bool, window: &Window, cx: &App) -> bool {
    ambient_motion_enabled(
        ambient_application_active(cx),
        window.is_window_active(),
        visible,
    ) && !cx.reduce_motion()
}

#[derive(Default)]
pub(crate) struct Pacer {
    waiting: Cell<bool>,
    drawn: Cell<u64>,
    visible_animation: Cell<bool>,
}

impl Pacer {
    pub(crate) fn begin(&self, tick: u64) {
        self.drawn.set(tick);
        self.visible_animation.set(false);
    }

    /// 在角色的 prepaint 中知道裁剪区域后才申请帧。滚出屏幕的角色不会维持动画。
    pub(crate) fn painted<V: 'static>(
        self: &Rc<Self>,
        bounds: Bounds<Pixels>,
        epoch: Instant,
        view: WeakEntity<V>,
        window: &mut Window,
    ) {
        if !bounds.intersects(&window.content_mask().bounds) {
            return;
        }
        self.visible_animation.set(true);
        if !self.waiting.replace(true) {
            self.wait(epoch, view, window);
        }
    }

    fn wait<V: 'static>(self: &Rc<Self>, epoch: Instant, view: WeakEntity<V>, window: &Window) {
        let pacer = self.clone();
        window.on_next_frame(move |window, cx| {
            // 背景状态在帧回调里再查一次，避免失焦后继续排帧。
            if !pacer.visible_animation.get() || !enabled(true, window, cx) {
                pacer.waiting.set(false);
                return;
            }
            if tick(epoch, cx.background_executor().now()) <= pacer.drawn.get() {
                if view.upgrade().is_some() {
                    pacer.wait(epoch, view, window);
                } else {
                    pacer.waiting.set(false);
                }
                return;
            }
            pacer.waiting.set(false);
            let _ = view.update(cx, |_, cx| cx.notify());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pixel_clock_is_slow_and_saturates_before_epoch() {
        let epoch = Instant::now();
        assert_eq!(tick(epoch, epoch), 0);
        assert_eq!(tick(epoch, epoch + Duration::from_millis(83)), 0);
        assert_eq!(tick(epoch, epoch + Duration::from_millis(84)), 1);
        assert_eq!(tick(epoch, epoch + Duration::from_secs(2)), 24);
        assert_eq!(tick(epoch, epoch - Duration::from_secs(1)), 0);
    }

    #[test]
    fn each_render_clears_the_previous_visible_animation() {
        let pacer = Pacer::default();
        pacer.visible_animation.set(true);
        pacer.begin(6);
        assert!(!pacer.visible_animation.get());
        assert_eq!(pacer.drawn.get(), 6);
    }

    #[gpui::test]
    fn hidden_background_and_inactive_windows_disable_animation(cx: &mut gpui::TestAppContext) {
        let window = cx.add_empty_window();
        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        window.update(|window, cx| {
            assert!(enabled(true, window, cx));
            assert!(!enabled(false, window, cx));
            smelt_plugin_api::ui::motion::set_ambient_application_active(cx, false);
            assert!(!enabled(true, window, cx));
            smelt_plugin_api::ui::motion::set_ambient_application_active(cx, true);
        });
        window.deactivate_window();
        window.update(|window, cx| assert!(!enabled(true, window, cx)));
    }
}
