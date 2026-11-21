//! 小人出帧的节拍：按 [`FPS`] 出帧，不跟显示器刷新率；看不见的不要帧。
//!
//! 动作一拍一秒多，会转的零件又是每 5° 一格，按刷新率（ProMotion 120Hz）出帧多数帧画出来
//! 一样，画了也是白画。
//!
//! 所有小人按同一个 [`Clock`] 的格取样：同一格里画面不变，换格时一起画，相位不错开。要下一帧
//! 时挂 `on_next_frame` 而不直接通知：回调跟着显示器刷新走，格没换就再挂一次、不通知视图，
//! gpui 这一帧只跑回调不重画；格换了才通知。整个过程靠显示器刷新事件驱动，不起定时器，
//! 不在动（停下、定格、看不见）就不再挂。

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Bounds, Pixels, WeakEntity, Window};

/// 小人每秒几帧。
const FPS: f64 = 30.;

/// 出帧的格：从 `epoch` 起每 1/[`FPS`] 秒一格。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    epoch: Instant,
}

impl Clock {
    pub(crate) fn new(epoch: Instant) -> Self {
        Self { epoch }
    }

    /// `now` 落在第几格。
    pub(crate) fn tick(self, now: Instant) -> u64 {
        (now.saturating_duration_since(self.epoch).as_secs_f64() * FPS) as u64
    }

    /// 第 `tick` 格开始的时刻：画这一格时按它取样。
    pub(crate) fn at(self, tick: u64) -> Instant {
        self.epoch + Duration::from_secs_f64(tick as f64 / FPS)
    }
}

/// 一个小人的出帧状态，视图和挂着的回调共用。
#[derive(Default)]
pub(crate) struct Pacer {
    /// 有没有挂着等换格的回调，免得重画几次就挂几条。
    waiting: Cell<bool>,
    /// 最近画的是哪一格。
    drawn: Cell<u64>,
}

impl Pacer {
    /// 画完第 `tick` 格后调（在 prepaint 里，`bounds` 是小人的画布）：还在动、又露在可见区里，
    /// 就等下一格到了通知 `view` 重画；否则什么都不挂，停在这一格。
    pub(crate) fn drew<V: 'static>(
        self: &Rc<Self>,
        tick: u64,
        animating: bool,
        bounds: Bounds<Pixels>,
        clock: Clock,
        view: WeakEntity<V>,
        window: &mut Window,
    ) {
        self.drawn.set(tick);
        // 可见区是祖先裁出来的区域（会话列表滚动区、窗口），滚出去的行这里不相交。
        let visible = bounds.intersects(&window.content_mask().bounds);
        if !animating || !visible || self.waiting.replace(true) {
            return;
        }
        wait(self.clone(), clock, view, window);
    }
}

/// 挂到下一次显示器刷新：格换了就通知 `view`，没换再挂一次。视图没了就不再挂。
fn wait<V: 'static>(pacer: Rc<Pacer>, clock: Clock, view: WeakEntity<V>, window: &Window) {
    window.on_next_frame(move |window, cx: &mut App| {
        if clock.tick(cx.background_executor().now()) <= pacer.drawn.get() {
            if view.upgrade().is_some() {
                wait(pacer, clock, view, window);
            } else {
                pacer.waiting.set(false);
            }
            return;
        }
        pacer.waiting.set(false);
        // 按 id 通知，不经 `update`：只有小人自己重画。经 `update` 通知算改了它的状态，读过它的
        // 所在视图（它在那儿比对参数）也要跟着重画。
        cx.notify(view.entity_id());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_step_at_the_frame_rate() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        assert_eq!(clock.tick(epoch), 0);
        assert_eq!(clock.tick(epoch + Duration::from_millis(30)), 0);
        assert_eq!(clock.tick(epoch + Duration::from_millis(34)), 1);
        assert_eq!(clock.tick(epoch + Duration::from_secs(2)), 60);
        // 早于起点的算第 0 格，不回绕。
        if let Some(before) = epoch.checked_sub(Duration::from_secs(1)) {
            assert_eq!(clock.tick(before), 0);
        }
    }

    #[test]
    fn a_tick_samples_at_its_start() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        for tick in [0, 1, 29, 30, 12_345] {
            let at = clock.at(tick);
            assert!(at >= epoch);
            assert_eq!(clock.tick(at + Duration::from_micros(10)), tick);
        }
    }
}
