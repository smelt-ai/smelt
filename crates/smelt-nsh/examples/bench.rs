//! 量 NSH 嵌进 GPUI 的代价，不进 App。开一个 1200×800 的窗口，预热 3 秒，再量 6 秒，打印
//! 主线程和整个进程的 CPU、这几秒画了几帧、平均每帧主线程多少、内存、线程数，然后退出。窗口被
//! 挡住时系统不让它出帧，所以比每帧的，不比每秒的。
//!
//! ```text
//! cargo run -j 2 --release -p smelt-nsh --example bench -- <场景> [块数]
//! ```
//!
//! - `idle`：左边 NSH 会话列表（不转），右边 GPUI 假终端，各是各的视图。
//! - `split`：同上，列表第一行在转（方案 1 / 3：每块一个 GPUI 视图）。
//! - `whole`：整窗一块 NSH，列表在转，假终端作原生组件嵌在里面（方案 2）。
//! - `split4`、`whole4`：同 `split`、`whole`，右边换成四块假终端（2×2），像真窗口里有几块重面板。
//! - `wholev`、`whole4v`：同 `whole`、`whole4`，但原生组件交出的是一个 GPUI 视图，不是每帧
//!   新建的元素，看 GPUI Fast 能不能沿用上一帧。
//! - `gpui`：参照，同样的列表用 GPUI 写，转圈是单独的小视图，右边 GPUI 假终端。
//! - `views N`：N 块不转的 NSH 列表，量每多一块的内存和线程。

use std::{
    path::PathBuf,
    rc::Rc,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, WindowBounds, WindowOptions, div, px, rgb, size,
};
use nsh_platform_gpui::{
    App as NshApp, AppOptions, AppView, AssetBase, Bundle, ComponentContext, NativeComponent,
};

const WARMUP: Duration = Duration::from_secs(3);
const MEASURE: Duration = Duration::from_secs(6);

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "split".to_owned());
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    log::set_logger(&Logger).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    gpui_platform::application().run(move |cx| {
        nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry()]);
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            inactive_frame_interval: None,
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| Bench::new(&mode, count, window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}

/// NSH 画过的帧，从它每秒一行的统计里加起来（所以精度是秒）。
static NSH_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 参照场景里转圈视图画过的帧。
static GPUI_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 只打 NSH 的帧统计（`nsh::fps`）和错误。
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.target().starts_with("nsh::fps") || meta.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        // `fps N active …`：上一秒 NSH 画了几帧。
        if record.target().starts_with("nsh::fps") {
            let line = record.args().to_string();
            if let Some(n) = line
                .split_whitespace()
                .nth(1)
                .and_then(|n| n.parse::<u64>().ok())
            {
                NSH_FRAMES.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

enum Layout {
    /// 左边一块，右边一块。
    Split(gpui::AnyView, Vec<Entity<Terminal>>),
    /// 一块铺满。
    Whole(Entity<AppView>),
    /// 一格一块。
    Grid(Vec<Entity<AppView>>),
}

struct Bench {
    layout: Layout,
}

impl Bench {
    fn new(mode: &str, count: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layout = match mode {
            "idle" => Layout::Split(
                nsh(window, cx, "bench-side-idle").into(),
                vec![cx.new(|_| Terminal)],
            ),
            "split" => Layout::Split(
                nsh(window, cx, "bench-side").into(),
                vec![cx.new(|_| Terminal)],
            ),
            "whole" => Layout::Whole(nsh(window, cx, "bench-whole")),
            "split4" => Layout::Split(
                nsh(window, cx, "bench-side").into(),
                (0..4).map(|_| cx.new(|_| Terminal)).collect(),
            ),
            "whole4" => Layout::Whole(nsh(window, cx, "bench-whole4")),
            "wholev" | "whole4v" => {
                VIEW_BACKED.store(true, std::sync::atomic::Ordering::Relaxed);
                let name = if mode == "wholev" {
                    "bench-whole"
                } else {
                    "bench-whole4"
                };
                Layout::Whole(nsh(window, cx, name))
            }
            "gpui" => Layout::Split(
                cx.new(|cx| GpuiSide {
                    spinner: cx.new(|_| Spinner {
                        start: Instant::now(),
                    }),
                })
                .into(),
                vec![cx.new(|_| Terminal)],
            ),
            "views" => Layout::Grid(
                (0..count)
                    .map(|_| nsh(window, cx, "bench-side-idle"))
                    .collect(),
            ),
            other => panic!("不认识的场景 {other}"),
        };
        let label = format!("{mode} {count}");
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(WARMUP).await;
            let before = Usage::now();
            cx.background_executor().timer(MEASURE).await;
            let after = Usage::now();
            let seconds = MEASURE.as_secs_f64();
            let pid = std::process::id().to_string();
            let ps = |args: &[&str]| {
                std::process::Command::new("ps")
                    .args(args)
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default()
            };
            let rss_mb = ps(&["-o", "rss=", "-p", &pid])
                .trim()
                .parse::<f64>()
                .unwrap_or(0.)
                / 1024.;
            let threads = ps(&["-M", "-p", &pid]).lines().count().saturating_sub(1);
            let frames = after.frames - before.frames;
            let per_frame = |cpu: f64| cpu * 1000. / frames.max(1) as f64;
            println!(
                "RESULT {label}: 帧 {frames}，每帧主线程 {:.2} ms、进程 {:.2} ms，每帧终端 render {:.1} 次；主线程 {:.1} ms/s，内存 {rss_mb:.0} MB，线程 {threads}",
                per_frame(after.main - before.main),
                per_frame(after.process - before.process),
                (after.terminal - before.terminal) as f64 / frames.max(1) as f64,
                (after.main - before.main) * 1000. / seconds,
            );
            cx.update(|cx| cx.quit());
        })
        .detach();
        Self { layout }
    }
}

impl Render for Bench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().bg(rgb(0xffffff));
        match &self.layout {
            Layout::Split(side, terminals) => {
                let cell = |t: &Entity<Terminal>| {
                    div().flex_1().min_w_0().min_h_0().h_full().child(t.clone())
                };
                let right = match terminals.as_slice() {
                    [one] => div().flex_1().min_w_0().h_full().child(one.clone()),
                    many => div()
                        .flex_1()
                        .h_full()
                        .flex()
                        .flex_col()
                        .children(many.chunks(2).map(|row| {
                            div()
                                .flex_1()
                                .w_full()
                                .flex()
                                .flex_row()
                                .children(row.iter().map(cell))
                        })),
                };
                root.flex()
                    .flex_row()
                    .child(div().w(px(280.)).flex_none().h_full().child(side.clone()))
                    .child(right)
            }
            Layout::Whole(app) => root.child(app.clone()),
            Layout::Grid(apps) => root.flex().flex_row().flex_wrap().children(
                apps.iter()
                    .map(|app| div().w(px(240.)).h(px(400.)).child(app.clone())),
            ),
        }
    }
}

/// 一块 NSH 界面，跑构建时打好的 `name` 包。
fn nsh(window: &mut Window, cx: &mut Context<Bench>, name: &str) -> Entity<AppView> {
    let dir = PathBuf::from(env!("OUT_DIR")).join(name);
    let options = AppOptions {
        asset_base: Some(AssetBase::Directory(dir.join("assets"))),
        ..AppOptions::default()
    };
    let app = cx.new(|cx| AppView::new(options, window, cx));
    // SAFETY: 包是本 crate 构建时打的。
    let bundle = unsafe { Bundle::read_trusted(&dir.join("bundle.nshc")) }.unwrap();
    app.update(cx, |view, _| {
        view.register_component("bench.terminal", Rc::new(NativeTerminal::make));
        view.on_error(|message| eprintln!("[bench] {message}"));
        view.set_fps_monitor(true);
        view.load(NshApp::Js(bundle));
    });
    app
}

// --- 假终端 ---------------------------------------------------------------------------

static LINES: LazyLock<Vec<SharedString>> = LazyLock::new(|| {
    (0..60)
        .map(|i| {
            let mut line = format!("{i:>3} │ ");
            while line.chars().count() < 120 {
                line.push_str("cargo check -j 2 -p smelt-nsh  Compiling rolldown v1.2.13 ");
            }
            line.chars().take(120).collect::<String>().into()
        })
        .collect()
});

static TERMINAL_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 60 行、每行 120 个等宽字，代替真终端。
fn terminal_element() -> AnyElement {
    TERMINAL_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    div()
        .size_full()
        .overflow_hidden()
        .bg(rgb(0x1e1e1e))
        .p_2()
        .font_family("Menlo")
        .text_size(px(12.))
        .text_color(rgb(0xd4d4d4))
        .flex()
        .flex_col()
        .children(
            LINES
                .iter()
                .map(|line| div().h(px(15.)).child(line.clone())),
        )
        .into_any_element()
}

/// 方案 1 / 3：终端是自己的 GPUI 视图。
struct Terminal;

impl Render for Terminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        terminal_element()
    }
}

/// 原生组件交出 GPUI 视图（、），不每帧新建元素。
static VIEW_BACKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 方案 2：终端是 NSH 里的原生组件。
struct NativeTerminal {
    view: Option<Entity<Terminal>>,
}

impl NativeTerminal {
    fn make(_: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(NativeTerminal { view: None })
    }
}

impl NativeComponent for NativeTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        if VIEW_BACKED.load(std::sync::atomic::Ordering::Relaxed) {
            let view = self.view.get_or_insert_with(|| cx.new(|_| Terminal));
            return Some(view.clone().into_any_element());
        }
        Some(terminal_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&serde_json::Value>, _: &mut Window, _: &mut App) {}
}

// --- 参照：GPUI 写的列表 ----------------------------------------------------------------

struct GpuiSide {
    spinner: Entity<Spinner>,
}

impl Render for GpuiSide {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .overflow_hidden()
            .p_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .children((0..80).map(|i| {
                let icon: AnyElement = if i == 0 {
                    self.spinner.clone().into_any_element()
                } else {
                    div()
                        .size_3()
                        .rounded_sm()
                        .bg(rgb(0xeeeeee))
                        .into_any_element()
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .child(icon)
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div().text_sm().child(format!(
                                    "会话 {i}：修一下侧栏在毛玻璃和背景图之间闪的问题"
                                )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x888888))
                                    .child(format!("~/wawa/smelt · js-plugins · {} 个改动", i % 7)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child(format!("{} 分钟前", (i * 7) % 60)),
                    )
            }))
    }
}

/// 每帧变一次透明度，自己请求下一帧：只有它自己重画。
struct Spinner {
    start: Instant,
}

impl Render for Spinner {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        GPUI_FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let phase = self.start.elapsed().as_secs_f32() % 1.;
        div()
            .size_3()
            .rounded_full()
            .bg(rgb(0x333333))
            .opacity(0.2 + 0.8 * phase)
    }
}

// --- CPU 时间 ---------------------------------------------------------------------------

struct Usage {
    /// 主线程（调用这里的线程）用掉的 CPU 秒数。
    main: f64,
    /// 整个进程。
    process: f64,
    terminal: u64,
    frames: u64,
}

impl Usage {
    fn now() -> Self {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: 传进去的是有效的 timespec。
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        let main = ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9;
        // SAFETY: 全零的 rusage 是合法的，getrusage 只往里写。
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: 同上。
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Self {
            main,
            process: seconds(usage.ru_utime) + seconds(usage.ru_stime),
            terminal: TERMINAL_RENDERS.load(std::sync::atomic::Ordering::Relaxed),
            frames: NSH_FRAMES.load(std::sync::atomic::Ordering::Relaxed)
                + GPUI_FRAMES.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}
