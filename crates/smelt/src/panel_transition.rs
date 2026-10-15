use std::time::Instant;

use smelt_ui::motion::CriticalSpring;

/// 进度离目标不到这么多就算停稳。侧栏三百来像素宽，对应不到一个像素。
const PROGRESS_REST: f32 = 0.002;

/// 可复用的面板挂载过渡。过渡期间保持面板挂载，到关闭进度归零后才卸载。
///
/// 进度走临界阻尼弹簧：连按开合时，面板带着当前速度先减速再掉头，不会从正往一边冲
/// 瞬间变成往另一边冲。
pub(crate) struct PanelTransition {
    progress: f32,
    /// 最近一次改目标时的进度和速度（进度/秒），弹簧从这里出发。
    from: f32,
    velocity: f32,
    target: f32,
    started_at: Option<Instant>,
}

#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub(crate) progress: f32,
    pub(crate) mounted: bool,
    pub(crate) animating: bool,
}

impl PanelTransition {
    pub(crate) fn new(open: bool) -> Self {
        let progress = if open { 1.0 } else { 0.0 };
        Self {
            progress,
            from: progress,
            velocity: 0.0,
            target: progress,
            started_at: None,
        }
    }

    pub(crate) fn set_open(&mut self, open: bool) {
        self.set_open_at(open, Instant::now());
    }

    fn set_open_at(&mut self, open: bool, now: Instant) {
        let target = if open { 1.0 } else { 0.0 };
        if target == self.target {
            return;
        }
        let (progress, velocity) = self.state_at(now);
        self.progress = progress;
        self.from = progress;
        self.velocity = velocity;
        self.target = target;
        self.started_at = Some(now);
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }

    /// 是否正在执行「打开」动画（目标朝向展开）。开合动画期间面板 body 会
    /// 逐帧重建，调用方可据此在打开时换轻量骨架、关闭时保留真实内容——
    /// 关闭时提前换骨架会让内容在面板滑出前就消失。
    pub(crate) fn is_opening(&self) -> bool {
        self.started_at.is_some() && self.target > 0.5
    }

    pub(crate) fn frame(&mut self) -> Frame {
        self.frame_at(Instant::now())
    }

    fn frame_at(&mut self, now: Instant) -> Frame {
        let (progress, velocity) = self.state_at(now);
        self.progress = progress;
        if self.started_at.is_some()
            && CriticalSpring::SNAP.is_settled(progress - self.target, velocity, PROGRESS_REST)
        {
            self.progress = self.target;
            self.from = self.target;
            self.velocity = 0.0;
            self.started_at = None;
        }
        Frame {
            progress: self.progress.clamp(0.0, 1.0),
            mounted: self.progress > 0.0 || self.target > 0.0,
            animating: self.started_at.is_some(),
        }
    }

    /// 某一时刻的（进度, 速度）。连按开合时带着速度掉头，可能略冲过 0 或 1，
    /// 对外的 `Frame::progress` 会钳回区间。
    fn state_at(&self, now: Instant) -> (f32, f32) {
        let Some(start) = self.started_at else {
            return (self.progress, 0.0);
        };
        let t = now.saturating_duration_since(start).as_secs_f32();
        let (offset, velocity) =
            CriticalSpring::SNAP.sample(self.from - self.target, self.velocity, t);
        (self.target + offset, velocity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn closed_panel_starts_unmounted() {
        let mut transition = PanelTransition::new(false);
        let frame = transition.frame();
        assert!(!frame.mounted);
        assert_eq!(frame.progress, 0.0);
    }

    #[test]
    fn closing_panel_stays_mounted_until_animation_finishes() {
        let mut transition = PanelTransition::new(true);
        transition.set_open(false);
        let frame = transition.frame();
        assert!(frame.mounted);
        assert!(frame.animating);
    }

    #[test]
    fn transition_can_reverse_mid_flight() {
        let mut transition = PanelTransition::new(true);
        transition.set_open(false);
        transition.set_open(true);
        let frame = transition.frame();
        assert!(frame.mounted);
        assert!(frame.animating);
    }

    #[test]
    fn opening_settles_exactly_open_and_stops_requesting_frames() {
        let start = Instant::now();
        let mut transition = PanelTransition::new(false);
        transition.set_open_at(true, start);
        assert!(transition.is_opening());
        let frame = transition.frame_at(start + Duration::from_millis(400));
        assert_eq!(frame.progress, 1.0);
        assert!(!frame.animating);
        assert!(!transition.is_opening());
    }

    #[test]
    fn closing_unmounts_once_settled() {
        let start = Instant::now();
        let mut transition = PanelTransition::new(true);
        transition.set_open_at(false, start);
        let frame = transition.frame_at(start + Duration::from_millis(400));
        assert!(!frame.mounted);
        assert!(!frame.animating);
    }

    #[test]
    fn reversing_mid_flight_keeps_moving_the_same_way_before_turning_back() {
        let start = Instant::now();
        let mut transition = PanelTransition::new(false);
        transition.set_open_at(true, start);
        let reversed_at = start + Duration::from_millis(40);
        let (progress_at_reverse, velocity_at_reverse) = transition.state_at(reversed_at);
        assert!(velocity_at_reverse > 0.0, "40ms 时应正往打开方向走");

        transition.set_open_at(false, reversed_at);
        let (_, velocity_after) = transition.state_at(reversed_at);
        assert!(
            (velocity_after - velocity_at_reverse).abs() < 1e-3,
            "换目标的那一刻速度必须连续：{velocity_at_reverse} → {velocity_after}"
        );
        let (progress_soon_after, _) = transition.state_at(reversed_at + Duration::from_millis(8));
        assert!(
            progress_soon_after > progress_at_reverse,
            "惯性让面板先继续展开一点，再减速收回"
        );
        let frame = transition.frame_at(reversed_at + Duration::from_millis(500));
        assert!(!frame.mounted && !frame.animating);
    }
}
