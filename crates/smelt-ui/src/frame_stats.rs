//! 视图重绘统计：每秒在 stderr 打一行，各视图 render 了几次、render 本身花了多久。
//!
//! 用 `SMELT_FRAME_STATS=1` 打开，默认关闭且不计时。只量 render 函数体（建元素树），
//! 不含其后的布局和绘制；拿来比较拆视图、换渲染后端前后的重绘次数和单次开销。
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(1);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("SMELT_FRAME_STATS").is_ok_and(|value| !value.is_empty() && value != "0")
    })
}

#[derive(Default)]
struct ViewStats {
    renders: u32,
    total: Duration,
    max: Duration,
}

impl ViewStats {
    fn record(&mut self, elapsed: Duration) {
        self.renders += 1;
        self.total += elapsed;
        self.max = self.max.max(elapsed);
    }

    fn line(&self, view: &str) -> String {
        let avg = self.total / self.renders.max(1);
        format!(
            "{view} {} 次/秒 · 平均 {:.2}ms · 最长 {:.2}ms",
            self.renders,
            avg.as_secs_f64() * 1000.0,
            self.max.as_secs_f64() * 1000.0
        )
    }
}

struct Window {
    started: Instant,
    views: BTreeMap<&'static str, ViewStats>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// 一次 render 的计时。随作用域结束记账；未开启统计时什么都不做。
#[must_use = "scope 一 drop 就结束计时"]
pub struct RenderTiming {
    view: &'static str,
    started: Option<Instant>,
}

impl RenderTiming {
    pub fn start(view: &'static str) -> Self {
        Self {
            view,
            started: enabled().then(Instant::now),
        }
    }
}

impl Drop for RenderTiming {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let now = Instant::now();
        WINDOW.with(|window| {
            let mut window = window.borrow_mut();
            let current = window.get_or_insert_with(|| Window {
                started: now,
                views: BTreeMap::new(),
            });
            current
                .views
                .entry(self.view)
                .or_default()
                .record(now - started);
            if now - current.started >= REPORT_INTERVAL {
                let lines = current
                    .views
                    .iter()
                    .map(|(view, stats)| stats.line(view))
                    .collect::<Vec<_>>();
                eprintln!("[smelt frame] {}", lines.join(" | "));
                *window = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_reports_count_average_and_max() {
        let mut stats = ViewStats::default();
        stats.record(Duration::from_millis(2));
        stats.record(Duration::from_millis(4));
        assert_eq!(
            stats.line("Workspace"),
            "Workspace 2 次/秒 · 平均 3.00ms · 最长 4.00ms"
        );
    }
}
