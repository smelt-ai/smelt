//! 压测采集与报告：GPUI 逐帧/分阶段耗时、CPU 占用、内存分布与增长、线程数及 JSON 输出。

use std::time::Duration;

use gpui::{
    WindowId,
    profiler::{FrameEvent, FrameTimingCollector, set_trace_enabled},
};
use serde::Serialize;
use smelt_plugin_api as api;

/// 超过一帧 60Hz 预算（16.667ms）即记为慢帧。
const SLOW_FRAME_THRESHOLD: Duration = Duration::from_micros(16_667);
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

pub fn enable_frame_tracing() {
    set_trace_enabled(true);
}

pub fn disable_frame_tracing() {
    set_trace_enabled(false);
}

/// 按窗口过滤采集 `gpui::profiler` 的绘制与呈递事件。
pub struct FrameCollector {
    collector: FrameTimingCollector,
    window_id: WindowId,
}

impl FrameCollector {
    pub fn new(window_id: WindowId) -> Self {
        Self {
            collector: FrameTimingCollector::new(),
            window_id,
        }
    }

    /// 预热结束时清掉积压样本，只保留测量窗口内的帧。
    pub fn reset(&mut self) {
        let _ = self.collector.collect_unseen();
    }

    /// 排空测量窗口内本窗口的全部帧样本并聚合统计。
    pub fn finish(&mut self, wall_seconds: f64) -> FrameReport {
        let mut draws_ms = Vec::new();
        let mut layout_sum_ms = 0.0;
        let mut prepaint_sum_ms = 0.0;
        let mut paint_sum_ms = 0.0;
        let mut over_budget_frames = 0u64;
        let mut slow_16ms_frames = 0u64;
        let mut invalidations_sum = 0u64;
        let mut presented_frames = 0u64;

        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(timing) if timing.window_id == self.window_id => {
                    let draw = timing.draw_duration();
                    draws_ms.push(draw.as_secs_f64() * 1000.0);
                    layout_sum_ms += timing.phases.request_layout.as_secs_f64() * 1000.0;
                    prepaint_sum_ms += timing.phases.prepaint.as_secs_f64() * 1000.0;
                    paint_sum_ms += timing.phases.paint.as_secs_f64() * 1000.0;
                    if draw > timing.budget() {
                        over_budget_frames += 1;
                    }
                    if draw > SLOW_FRAME_THRESHOLD {
                        slow_16ms_frames += 1;
                    }
                    invalidations_sum += timing.invalidations;
                }
                FrameEvent::Present(timing) if timing.window_id == self.window_id => {
                    presented_frames += 1;
                }
                FrameEvent::Draw(_) | FrameEvent::Present(_) => {}
            }
        }

        let drawn_frames = draws_ms.len() as u64;
        let fps = if wall_seconds > 0.0 {
            presented_frames as f64 / wall_seconds
        } else {
            0.0
        };
        if draws_ms.is_empty() {
            return FrameReport {
                drawn_frames: 0,
                presented_frames,
                fps,
                draw_avg_ms: 0.0,
                draw_p50_ms: 0.0,
                draw_p95_ms: 0.0,
                draw_p99_ms: 0.0,
                draw_max_ms: 0.0,
                layout_avg_ms: 0.0,
                prepaint_avg_ms: 0.0,
                paint_avg_ms: 0.0,
                over_budget_frames: 0,
                slow_16ms_frames: 0,
                slow_16ms_pct: 0.0,
                mean_invalidations: 0.0,
            };
        }

        draws_ms.sort_by(f64::total_cmp);
        let count_f = drawn_frames as f64;
        let draw_sum_ms: f64 = draws_ms.iter().sum();

        FrameReport {
            drawn_frames,
            presented_frames,
            fps,
            draw_avg_ms: draw_sum_ms / count_f,
            draw_p50_ms: nearest_rank(&draws_ms, 0.50),
            draw_p95_ms: nearest_rank(&draws_ms, 0.95),
            draw_p99_ms: nearest_rank(&draws_ms, 0.99),
            draw_max_ms: *draws_ms.last().unwrap_or(&0.0),
            layout_avg_ms: layout_sum_ms / count_f,
            prepaint_avg_ms: prepaint_sum_ms / count_f,
            paint_avg_ms: paint_sum_ms / count_f,
            over_budget_frames,
            slow_16ms_frames,
            slow_16ms_pct: (slow_16ms_frames as f64 * 100.0) / count_f,
            mean_invalidations: invalidations_sum as f64 / count_f,
        }
    }
}

fn nearest_rank(sorted: &[f64], percentile: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0.0;
    };
    let rank = ((percentile.clamp(0.0, 1.0) * last as f64).round() as usize).min(last);
    sorted[rank]
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FrameReport {
    pub drawn_frames: u64,
    pub presented_frames: u64,
    pub fps: f64,
    pub draw_avg_ms: f64,
    pub draw_p50_ms: f64,
    pub draw_p95_ms: f64,
    pub draw_p99_ms: f64,
    pub draw_max_ms: f64,
    pub layout_avg_ms: f64,
    pub prepaint_avg_ms: f64,
    pub paint_avg_ms: f64,
    pub over_budget_frames: u64,
    pub slow_16ms_frames: u64,
    pub slow_16ms_pct: f64,
    pub mean_invalidations: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CpuSample {
    /// 主线程用掉的 CPU 秒数。
    pub main_secs: f64,
    /// 整个进程用掉的 CPU 秒数。
    pub process_secs: f64,
}

impl CpuSample {
    #[cfg(unix)]
    pub fn now() -> Self {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: 传入有效指针，clock_gettime 只写入 tv_sec / tv_nsec。
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        let main_secs = ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9;

        // SAFETY: 全零 rusage 合法，getrusage 只写回自身统计。
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: 同上。
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        let to_secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Self {
            main_secs,
            process_secs: to_secs(usage.ru_utime) + to_secs(usage.ru_stime),
        }
    }

    #[cfg(not(unix))]
    pub fn now() -> Self {
        Self::default()
    }

    pub fn diff(self, after: Self, wall_seconds: f64, drawn_frames: u64) -> CpuReport {
        let main_delta = (after.main_secs - self.main_secs).max(0.0);
        let process_delta = (after.process_secs - self.process_secs).max(0.0);
        let wall = wall_seconds.max(0.001);
        let (main_cpu_ms_per_frame, process_cpu_ms_per_frame) = if drawn_frames > 0 {
            let frames = drawn_frames as f64;
            (
                main_delta * 1000.0 / frames,
                process_delta * 1000.0 / frames,
            )
        } else {
            (0.0, 0.0)
        };
        CpuReport {
            main_cpu_ms_per_s: main_delta * 1000.0 / wall,
            process_cpu_ms_per_s: process_delta * 1000.0 / wall,
            main_cpu_ms_per_frame,
            process_cpu_ms_per_frame,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuReport {
    pub main_cpu_ms_per_s: f64,
    pub process_cpu_ms_per_s: f64,
    pub main_cpu_ms_per_frame: f64,
    pub process_cpu_ms_per_frame: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub graphics_bytes: Option<u64>,
    pub heap_bytes: Option<u64>,
    pub other_bytes: Option<u64>,
}

impl MemorySnapshot {
    pub fn capture(sampler: &mut api::process_usage::ProcessSampler) -> Self {
        if let Some(own) = api::process_usage::own_memory() {
            return Self {
                total_bytes: own.total,
                graphics_bytes: Some(own.graphics),
                heap_bytes: Some(own.heap),
                other_bytes: Some(own.other()),
            };
        }
        let total_bytes = sampler
            .sample_process(std::process::id())
            .map_or(0, |usage| usage.memory_bytes);
        Self {
            total_bytes,
            graphics_bytes: None,
            heap_bytes: None,
            other_bytes: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryReport {
    pub mem_total_mb: f64,
    pub mem_delta_mb: f64,
    pub mem_graphics_mb: Option<f64>,
    pub mem_heap_mb: Option<f64>,
    pub mem_heap_delta_mb: Option<f64>,
    pub mem_other_mb: Option<f64>,
}

impl MemoryReport {
    pub fn from_pair(before: MemorySnapshot, after: MemorySnapshot) -> Self {
        let to_mb = |bytes: u64| bytes as f64 / BYTES_PER_MIB;
        let delta_mb = |b: u64, a: u64| (a as f64 - b as f64) / BYTES_PER_MIB;
        let mem_heap_delta_mb = match (before.heap_bytes, after.heap_bytes) {
            (Some(b), Some(a)) => Some(delta_mb(b, a)),
            _ => None,
        };
        Self {
            mem_total_mb: to_mb(after.total_bytes),
            mem_delta_mb: delta_mb(before.total_bytes, after.total_bytes),
            mem_graphics_mb: after.graphics_bytes.map(to_mb),
            mem_heap_mb: after.heap_bytes.map(to_mb),
            mem_heap_delta_mb,
            mem_other_mb: after.other_bytes.map(to_mb),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn thread_count() -> Option<usize> {
    let pid = std::process::id().to_string();
    let out = std::process::Command::new("ps")
        .args(["-M", "-p", &pid])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().count().saturating_sub(1))
}

#[cfg(not(target_os = "macos"))]
pub fn thread_count() -> Option<usize> {
    None
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BootPluginTiming {
    pub id: String,
    pub compile_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BootReport {
    pub compile_total_ms: Option<f64>,
    pub compile_shell_ms: Option<f64>,
    pub compile_plugins_ms: Option<f64>,
    pub stage_ms: Option<f64>,
    pub ready_ms: f64,
    pub plugins: usize,
    pub plugin_vms: usize,
    pub plugin_errors: usize,
    pub slowest_plugins: Vec<BootPluginTiming>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ExtraEvents {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcasts: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switches: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_events: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resizes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appended_entries: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub scenario: String,
    pub count: usize,
    pub warmup_s: f64,
    pub measure_s: f64,
    pub wall_s: f64,
    pub frames: FrameReport,
    pub cpu: CpuReport,
    pub memory: MemoryReport,
    pub threads: Option<usize>,
    pub terminal_renders_per_frame: f64,
    pub errors: usize,
    pub extra_events: ExtraEvents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot: Option<BootReport>,
}

impl BenchReport {
    pub fn format_human(&self) -> String {
        let label = if self.count > 1 {
            format!("{} {}", self.scenario, self.count)
        } else {
            self.scenario.clone()
        };
        let f = &self.frames;
        let c = &self.cpu;
        let m = &self.memory;

        let mem_part = match (m.mem_heap_mb, m.mem_heap_delta_mb, m.mem_graphics_mb) {
            (Some(heap), Some(heap_d), Some(gfx)) => format!(
                "内存 {:.0} MB ({:+.1} MB, heap {:.0} MB {:+.1}, gfx {:.0} MB)",
                m.mem_total_mb, m.mem_delta_mb, heap, heap_d, gfx
            ),
            _ => format!("内存 {:.0} MB ({:+.1} MB)", m.mem_total_mb, m.mem_delta_mb),
        };
        let threads_part = self
            .threads
            .map_or_else(String::new, |t| format!("，线程 {t}"));

        let mut extras = Vec::new();
        if self.terminal_renders_per_frame > 0.0 {
            extras.push(format!(
                "假终端 render {:.1}/f",
                self.terminal_renders_per_frame
            ));
        }
        if let Some(n) = self.extra_events.broadcasts {
            extras.push(format!("broadcasts={n}"));
        }
        if let Some(n) = self.extra_events.switches {
            extras.push(format!("switches={n}"));
        }
        if let Some(n) = self.extra_events.scroll_events {
            extras.push(format!("scrolls={n}"));
        }
        if let Some(n) = self.extra_events.resizes {
            extras.push(format!("resizes={n}"));
        }
        if let Some(n) = self.extra_events.appended_entries {
            extras.push(format!("appended={n}"));
        }
        extras.push(format!("errors={}", self.errors));

        format!(
            "RESULT {label}: 帧 {} (呈现 {}, {:.1} FPS) · draw avg {:.2} / p95 {:.2} / p99 {:.2} / max {:.2} ms (layout {:.2} + prepaint {:.2} + paint {:.2}) · >16.7ms {:.1}% · inv {:.2}x | 主线程 {:.2} ms/f ({:.1} ms/s)，进程 {:.2} ms/f ({:.1} ms/s) | {mem_part}{threads_part} | {}",
            f.drawn_frames,
            f.presented_frames,
            f.fps,
            f.draw_avg_ms,
            f.draw_p95_ms,
            f.draw_p99_ms,
            f.draw_max_ms,
            f.layout_avg_ms,
            f.prepaint_avg_ms,
            f.paint_avg_ms,
            f.slow_16ms_pct,
            f.mean_invalidations,
            c.main_cpu_ms_per_frame,
            c.main_cpu_ms_per_s,
            c.process_cpu_ms_per_frame,
            c.process_cpu_ms_per_s,
            extras.join(", "),
        )
    }
}
