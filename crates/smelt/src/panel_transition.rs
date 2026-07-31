use std::time::{Duration, Instant};

const DEFAULT_DURATION: Duration = Duration::from_millis(180);

/// 可复用的面板挂载过渡。过渡期间保持面板挂载，到关闭进度归零后才卸载。
pub(crate) struct PanelTransition {
    progress: f32,
    from: f32,
    target: f32,
    started_at: Option<Instant>,
    duration: Duration,
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
            target: progress,
            started_at: None,
            duration: DEFAULT_DURATION,
        }
    }

    pub(crate) fn set_open(&mut self, open: bool) {
        let target = if open { 1.0 } else { 0.0 };
        if target == self.target {
            return;
        }
        let now = Instant::now();
        self.progress = self.value_at(now);
        self.from = self.progress;
        self.target = target;
        self.started_at = Some(now);
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }

    pub(crate) fn frame(&mut self) -> Frame {
        let now = Instant::now();
        self.progress = self.value_at(now);
        let done = self
            .started_at
            .is_some_and(|start| now.duration_since(start) >= self.scaled_duration());
        if done {
            self.progress = self.target;
            self.from = self.target;
            self.started_at = None;
        }
        Frame {
            progress: self.progress.clamp(0.0, 1.0),
            mounted: self.progress > 0.0 || self.target > 0.0,
            animating: self.started_at.is_some(),
        }
    }

    fn value_at(&self, now: Instant) -> f32 {
        let Some(start) = self.started_at else {
            return self.progress;
        };
        let duration = self.scaled_duration().as_secs_f32();
        let raw = if duration <= f32::EPSILON {
            1.0
        } else {
            (now.duration_since(start).as_secs_f32() / duration).min(1.0)
        };
        let eased = 1.0 - (1.0 - raw).powi(5);
        self.from + (self.target - self.from) * eased
    }

    fn scaled_duration(&self) -> Duration {
        self.duration
            .mul_f32((self.target - self.from).abs().max(0.01))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
